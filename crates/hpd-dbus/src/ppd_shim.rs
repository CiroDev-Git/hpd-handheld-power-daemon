// SPDX-License-Identifier: GPL-3.0-or-later

// zbus's `#[interface]` macro synthesises items (the interface `name()`
// shim, property-changed signal emitters) whose docs we can't attach via
// `///` — same reason `hpd_dbus::service` carries this at module level.
// Every human-written item below is documented individually.
#![allow(missing_docs)]

//! Compatibility shim for `net.hadess.PowerProfiles`
//! (`power-profiles-daemon`, "PPD").
//!
//! ## Why this exists
//!
//! PPD clients — the KDE Plasma battery applet's Eco/Balanced/Performance
//! selector, `powerprofilesctl`, CachyOS's `game-performance` launch
//! wrapper — never touch hardware. They only talk D-Bus to whoever owns
//! the well-known name `net.hadess.PowerProfiles`: a universal remote
//! that doesn't care which "TV" answers. `hpd` masks the real PPD (see
//! [`crate::conflicts`]) because both write the *same*
//! `platform_profile`/EPP files and would otherwise fight over them —
//! that mask stays, unconditionally. But it leaves those clients
//! orphaned: the name has no owner, so the KDE applet hides its selector
//! and `game-performance` fails outright.
//!
//! This module is hpd putting on PPD's mask: it claims the same
//! well-known name and implements the subset of PPD's D-Bus API real
//! clients use, so they revive without knowing anything changed — except
//! now their "remote" controls `hpd`. This is **not** a second source of
//! truth: every request is translated to an ordinary
//! [`Transition::SetProfile`] that flows through the same reducer,
//! AC-lock and rollback as any other caller (`hpdctl power set`, the
//! plugin, …). See `docs/dev/GAMING-ROADMAP-es.md` §4 for the full
//! design (with sequence diagrams).
//!
//! ## Precedence
//!
//! `AC-lock > hold > manual`. A `HoldProfile` while AC-locked is accepted
//! (returns a valid cookie) but moves nothing — `SetProfile` is one of
//! the levers the reducer already no-ops while locked, so no special
//! case is needed here. Among simultaneous holds, upstream PPD's own
//! rule applies: `power-saver` outranks `performance`.
//!
//! ## Scope: what's replicated, what's cut
//!
//! Implements `ActiveProfile` (read-write), `Profiles`, `Actions`,
//! `ActiveProfileHolds`, `PerformanceInhibited`, `PerformanceDegraded`,
//! `HoldProfile`/`ReleaseProfile`, and the `ProfileReleased` signal —
//! the complete real-world API surface (verified against upstream's
//! `net.hadess.PowerProfiles.xml`). `PerformanceDegraded` always reads
//! `""`: hpd has no thermal-degradation detector to plug into it yet.
//! `PerformanceInhibited` is upstream-deprecated and unused since PPD
//! 0.9; replicated as an always-empty string purely for clients that
//! still read it.
//!
//! ## No polkit on this surface, on purpose
//!
//! Upstream PPD requires no authorization for `ActiveProfile` or
//! `HoldProfile`/`ReleaseProfile` — any session client may flip the
//! profile freely. Gating this shim behind hpd's own `set-profile`
//! polkit action would silently regress every client that worked
//! passwordlessly against the real PPD (that's the entire point of
//! reviving them). This is a deliberate, compat-only exception scoped to
//! *this* bus name; hpd's own `dev.cirodev.hpd.PowerDaemon1` interface
//! keeps requiring polkit as documented in `hpd_dbus::actions`.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, watch, Mutex as AsyncMutex};
use tracing::{debug, info, warn};
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::InterfaceRef;
use zbus::zvariant::OwnedValue;

use hpd_capabilities::profile::ProfileName;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;

/// Well-known bus name upstream `power-profiles-daemon` owns; hardcoded
/// by every real client, so this must match exactly.
pub const BUS_NAME: &str = "net.hadess.PowerProfiles";
/// Object path upstream serves its interface at; likewise hardcoded by
/// clients.
pub const OBJECT_PATH: &str = "/net/hadess/PowerProfiles";

/// Convert an owned `String` to an `OwnedValue`, for the `aa{sv}`
/// properties below. `OwnedValue` has no direct `From<String>` (only the
/// generic `TryFrom<Value>` path), so this goes through `Value` first.
/// Practically infallible for a plain string — `None` is only a
/// theoretical fallback, silently omitting the key rather than a
/// placeholder, matching `hpd_dbus::service`'s `insert_dbus_value`
/// convention.
fn str_value(s: impl Into<String>) -> Option<OwnedValue> {
    OwnedValue::try_from(zbus::zvariant::Value::from(s.into())).ok()
}

/// One live `HoldProfile` registration.
#[derive(Debug, Clone)]
struct Hold {
    cookie: u32,
    profile: ProfileName,
    application_id: String,
    reason: String,
    holder: OwnedUniqueName,
}

/// State shared between the interface object and its two background
/// tasks (external-override reconciliation, holder-disconnect
/// detection) — split out so both sides of that boundary can hold a
/// cheap `Clone` instead of fighting over one long-lived borrow.
#[derive(Clone)]
struct Shared {
    tx: mpsc::Sender<Transition>,
    holds: Arc<AsyncMutex<Vec<Hold>>>,
    next_cookie: Arc<AtomicU32>,
    /// The profile to restore once every hold drains, snapshotted the
    /// moment the *first* hold in a stack is created. `None` whenever no
    /// hold is outstanding.
    pre_hold_profile: Arc<AsyncMutex<Option<ProfileName>>>,
}

impl Shared {
    async fn send_profile(&self, profile: ProfileName) {
        debug!(profile = %profile, "ppd shim: applying profile");
        if self.tx.send(Transition::SetProfile(profile)).await.is_err() {
            warn!("ppd shim: executor channel closed, could not apply profile");
        }
    }

    /// `power-saver` outranks `performance` when both are held at once
    /// (upstream PPD's own documented precedence). `None` when nothing
    /// is held.
    fn effective_hold_profile(holds: &[Hold]) -> Option<ProfileName> {
        if holds.iter().any(|h| h.profile == ProfileName::PowerSaver) {
            Some(ProfileName::PowerSaver)
        } else if holds.iter().any(|h| h.profile == ProfileName::Performance) {
            Some(ProfileName::Performance)
        } else {
            None
        }
    }

    /// Register a new hold and apply the resulting precedence if it
    /// changes anything. Snapshots `current_active` as the profile to
    /// restore later, but only if this is the first hold in a fresh
    /// stack (an existing snapshot from an earlier hold in the same
    /// stack must not be clobbered).
    async fn hold(
        &self,
        profile: ProfileName,
        reason: String,
        application_id: String,
        holder: OwnedUniqueName,
        current_active: ProfileName,
    ) -> u32 {
        let cookie = self.next_cookie.fetch_add(1, Ordering::Relaxed);
        let mut pre_hold = self.pre_hold_profile.lock().await;
        let mut holds = self.holds.lock().await;
        if holds.is_empty() {
            *pre_hold = Some(current_active);
        }
        drop(pre_hold);
        holds.push(Hold {
            cookie,
            profile,
            application_id,
            reason,
            holder,
        });
        let target = Self::effective_hold_profile(&holds);
        drop(holds);
        if let Some(target) = target {
            self.send_profile(target).await;
        }
        cookie
    }

    /// Remove every hold `should_remove` matches, returning their
    /// cookies and the precedence recomputed over what remains (`None`
    /// if nothing is held anymore). Applying the fallout — reasserting a
    /// remaining hold's profile, restoring the pre-hold snapshot, or (for
    /// an external override) doing neither — is the caller's call, since
    /// the three removal reasons (explicit release, holder disconnect,
    /// external override) each resolve it differently.
    async fn remove_holds(
        &self,
        mut should_remove: impl FnMut(&Hold) -> bool,
    ) -> (Vec<u32>, Option<ProfileName>) {
        let mut holds = self.holds.lock().await;
        let mut removed = Vec::new();
        holds.retain(|h| {
            if should_remove(h) {
                removed.push(h.cookie);
                false
            } else {
                true
            }
        });
        let target = Self::effective_hold_profile(&holds);
        (removed, target)
    }

    /// What the current hold stack says `ActiveProfile` should be,
    /// without mutating anything — used by the override reconciler to
    /// tell "we just applied this ourselves" apart from "something else
    /// changed it".
    async fn peek_expected(&self) -> Option<ProfileName> {
        Self::effective_hold_profile(&self.holds.lock().await)
    }

    /// Take (and clear) the pre-hold snapshot, if any, and re-apply it.
    /// Used when a hold stack fully drains via `ReleaseProfile` or a
    /// holder disconnecting — never on an external override, where the
    /// override itself is the new truth.
    async fn restore_pre_hold_snapshot(&self) {
        if let Some(restore) = self.pre_hold_profile.lock().await.take() {
            self.send_profile(restore).await;
        }
    }
}

/// [`net.hadess.PowerProfiles`](self) interface object.
pub struct PowerProfilesShim {
    state_rx: watch::Receiver<ProfileState>,
    shared: Shared,
}

/// Opaque handle to the state a [`PowerProfilesShim`] shares with its
/// background tasks — obtained via [`PowerProfilesShim::handle`] before
/// the interface object is moved into the `ObjectServer` (which consumes
/// it), mirroring how `hpd-daemon` already clones the `state_rx` watch
/// receiver before handing the original to `PowerDaemonInterface::new`.
#[derive(Clone)]
pub struct PpdShimHandle {
    shared: Shared,
    state_rx: watch::Receiver<ProfileState>,
}

impl PowerProfilesShim {
    /// Build a fresh shim. `tx` is the same executor command lane the
    /// main interface uses; `state_rx` mirrors the live `ProfileState`.
    pub fn new(tx: mpsc::Sender<Transition>, state_rx: watch::Receiver<ProfileState>) -> Self {
        Self {
            state_rx: state_rx.clone(),
            shared: Shared {
                tx,
                holds: Arc::new(AsyncMutex::new(Vec::new())),
                next_cookie: Arc::new(AtomicU32::new(1)),
                pre_hold_profile: Arc::new(AsyncMutex::new(None)),
            },
        }
    }

    /// Extract the handle the background tasks need. Call this
    /// **before** handing `self` to `serve_at`/`ObjectServer`.
    pub fn handle(&self) -> PpdShimHandle {
        PpdShimHandle {
            shared: self.shared.clone(),
            state_rx: self.state_rx.clone(),
        }
    }
}

#[interface(name = "net.hadess.PowerProfiles")]
impl PowerProfilesShim {
    /// Force `profile` (`power-saver` or `performance` only — matching
    /// upstream, which never allows holding `balanced`) active until the
    /// caller disconnects, calls `ReleaseProfile`, or `ActiveProfile`
    /// changes by some other means. Returns a cookie identifying this
    /// hold. No polkit check — see the module docs.
    async fn hold_profile(
        &self,
        profile: String,
        reason: String,
        application_id: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        let profile = ProfileName::from_str(&profile).map_err(zbus::fdo::Error::InvalidArgs)?;
        if !matches!(profile, ProfileName::PowerSaver | ProfileName::Performance) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "HoldProfile only accepts power-saver or performance, got {profile}"
            )));
        }
        let Some(sender) = header.sender() else {
            return Err(zbus::fdo::Error::Failed("method call has no sender".into()));
        };
        let holder: OwnedUniqueName = sender.to_owned().into();
        let current_active = self.state_rx.borrow().active_profile.clone();
        info!(
            %profile, %reason, %application_id,
            "ppd shim: HoldProfile"
        );
        Ok(self
            .shared
            .hold(profile, reason, application_id, holder, current_active)
            .await)
    }

    /// Release a hold previously created with `HoldProfile`. Releasing
    /// an unknown/already-released cookie is a harmless no-op (matches
    /// upstream — a client racing its own cleanup must not crash the
    /// daemon). Never emits `ProfileReleased`: that signal is reserved
    /// for the *involuntary* release triggered by an external profile
    /// change (see the module docs and `ProfileReleased`'s doc comment).
    async fn release_profile(&self, cookie: u32) -> zbus::fdo::Result<()> {
        let (released, target) = self.shared.remove_holds(|h| h.cookie == cookie).await;
        if released.is_empty() {
            return Ok(());
        }
        info!(cookie, "ppd shim: ReleaseProfile");
        match target {
            Some(profile) => self.shared.send_profile(profile).await,
            None => self.shared.restore_pre_hold_snapshot().await,
        }
        Ok(())
    }

    /// This signal fires **only** when a hold is cancelled because
    /// `ActiveProfile` changed by some means other than precedence over
    /// the current holds (e.g. a direct `Properties.Set` from another
    /// client, or hpd's own `set_profile`/CLI/plugin) — never for an
    /// ordinary voluntary `ReleaseProfile` call. Matches upstream's
    /// documented contract exactly.
    #[zbus(signal)]
    pub async fn profile_released(
        signal_context: &zbus::object_server::SignalContext<'_>,
        cookie: u32,
    ) -> zbus::Result<()>;

    /// The live power profile, mirroring hpd's own `ActiveProfile` /
    /// `hpdctl power get`. Read-write: a client (the KDE applet) sets
    /// this directly to change the profile with no hold involved.
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.state_rx.borrow().active_profile.to_string()
    }

    #[zbus(property)]
    async fn set_active_profile(&self, value: String) -> zbus::Result<()> {
        let profile = ProfileName::from_str(&value)
            .map_err(zbus::fdo::Error::InvalidArgs)
            .map_err(zbus::Error::from)?;
        info!(%profile, "ppd shim: ActiveProfile set directly");
        self.shared.send_profile(profile).await;
        Ok(())
    }

    /// Upstream-deprecated since PPD 0.9 and always unused; replicated
    /// as an always-empty string purely so a client that still reads it
    /// gets a well-typed answer instead of an error.
    #[zbus(property)]
    fn performance_inhibited(&self) -> String {
        String::new()
    }

    /// Always `""`: hpd has no thermal-degradation detector to report a
    /// reason (`"lap-detected"`, `"high-operating-temperature"`, …) from
    /// yet. Documented gap, not a bug — see the module docs.
    #[zbus(property)]
    fn performance_degraded(&self) -> String {
        String::new()
    }

    /// The three profiles hpd supports, in upstream's documented order
    /// (power-saver, balanced, performance). `Driver` is always `"hpd"`
    /// — there is exactly one backend, unlike upstream PPD which can mix
    /// drivers per profile.
    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        ["power-saver", "balanced", "performance"]
            .into_iter()
            .map(|name| {
                let mut m = HashMap::new();
                if let Some(v) = str_value(name) {
                    m.insert("Profile".to_string(), v);
                }
                if let Some(v) = str_value("hpd") {
                    m.insert("Driver".to_string(), v);
                }
                m
            })
            .collect()
    }

    /// Empty: hpd implements none of upstream's optional "actions"
    /// (e.g. trickle-charge control).
    #[zbus(property)]
    fn actions(&self) -> Vec<String> {
        Vec::new()
    }

    /// One entry per live hold, keyed exactly as upstream documents:
    /// `ApplicationId`, `Profile`, `Reason`.
    #[zbus(property)]
    async fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
        self.shared
            .holds
            .lock()
            .await
            .iter()
            .map(|h| {
                let mut m = HashMap::new();
                if let Some(v) = str_value(h.application_id.clone()) {
                    m.insert("ApplicationId".to_string(), v);
                }
                if let Some(v) = str_value(h.profile.to_string()) {
                    m.insert("Profile".to_string(), v);
                }
                if let Some(v) = str_value(h.reason.clone()) {
                    m.insert("Reason".to_string(), v);
                }
                m
            })
            .collect()
    }
}

/// Background task: watches hpd's `ProfileState` for a change to
/// `active_profile` that isn't explained by the current hold stack's own
/// precedence (i.e. something *else* changed it — a direct
/// `Properties.Set`, hpd's own `set_profile`, the CLI, the plugin) and,
/// per upstream's documented contract, cancels every outstanding hold
/// and signals `ProfileReleased` to each holder. Exits when the state
/// channel closes (daemon shutting down).
pub async fn run_external_override_reconciler(
    handle: PpdShimHandle,
    iface_ref: InterfaceRef<PowerProfilesShim>,
) {
    let mut state_rx = handle.state_rx;
    loop {
        if state_rx.changed().await.is_err() {
            debug!("ppd shim: state channel closed, stopping override reconciler");
            return;
        }
        let current = state_rx.borrow_and_update().active_profile.clone();
        let expected = handle.shared.peek_expected().await;
        if expected.as_ref().is_none_or(|p| *p == current) {
            // No holds, or the current value already matches what the
            // hold stack would produce (this is our *own* SetProfile
            // from applying a hold — not an external override).
            continue;
        }
        // Something else changed it: cancel every hold, signal each
        // holder, and — unlike a voluntary release — do NOT restore the
        // pre-hold snapshot. The external value is the new truth.
        let (released, _) = handle.shared.remove_holds(|_| true).await;
        *handle.shared.pre_hold_profile.lock().await = None;
        if released.is_empty() {
            continue;
        }
        info!(
            cookies = ?released,
            new_profile = %current,
            "ppd shim: ActiveProfile changed externally, releasing all holds"
        );
        let ctx = iface_ref.signal_context();
        for cookie in released {
            if let Err(e) = PowerProfilesShim::profile_released(ctx, cookie).await {
                warn!(error = %e, cookie, "ppd shim: failed to emit ProfileReleased");
            }
        }
    }
}

/// Background task: watches the bus for `NameOwnerChanged` and releases
/// any hold whose holder just disconnected — the involuntary-release
/// path a crashed/killed game (rather than a clean `ReleaseProfile` call)
/// takes. No `ProfileReleased` signal: the holder that would receive it
/// is, by definition, already gone.
pub async fn run_holder_disconnect_watcher(conn: zbus::Connection, handle: PpdShimHandle) {
    use futures_util::StreamExt;

    let dbus = match zbus::fdo::DBusProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "ppd shim: could not watch NameOwnerChanged, holder disconnects won't be detected");
            return;
        }
    };
    let mut stream = match dbus.receive_name_owner_changed().await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "ppd shim: could not subscribe to NameOwnerChanged");
            return;
        }
    };

    while let Some(signal) = stream.next().await {
        let Ok(args) = signal.args() else { continue };
        // A holder disconnects when its unique name loses its *own*
        // connection, i.e. `new_owner` goes empty.
        if !args.new_owner.is_none() {
            continue;
        }
        let Some(gone) = args.old_owner.as_ref() else {
            continue;
        };
        let gone_str = gone.as_str();
        let (released, target) = handle
            .shared
            .remove_holds(|h| h.holder.as_str() == gone_str)
            .await;
        if released.is_empty() {
            continue;
        }
        info!(cookies = ?released, holder = gone_str, "ppd shim: holder disconnected, releasing its holds");
        match target {
            Some(profile) => handle.shared.send_profile(profile).await,
            None => handle.shared.restore_pre_hold_snapshot().await,
        }
    }
    debug!("ppd shim: NameOwnerChanged stream ended, stopping disconnect watcher");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn holder(unique: &str) -> OwnedUniqueName {
        zbus::names::UniqueName::try_from(unique)
            .expect("valid unique name")
            .into()
    }

    fn shared_with_channel() -> (Shared, mpsc::Receiver<Transition>) {
        let (tx, rx) = mpsc::channel(8);
        (
            Shared {
                tx,
                holds: Arc::new(AsyncMutex::new(Vec::new())),
                next_cookie: Arc::new(AtomicU32::new(1)),
                pre_hold_profile: Arc::new(AsyncMutex::new(None)),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn hold_applies_the_requested_profile() {
        let (shared, mut rx) = shared_with_channel();
        let cookie = shared
            .hold(
                ProfileName::Performance,
                "test".into(),
                "app".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        assert_eq!(cookie, 1);
        assert!(matches!(
            rx.recv().await,
            Some(Transition::SetProfile(ProfileName::Performance))
        ));
    }

    #[tokio::test]
    async fn power_saver_outranks_performance_regardless_of_order() {
        let (shared, mut rx) = shared_with_channel();
        shared
            .hold(
                ProfileName::Performance,
                "r1".into(),
                "app1".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        // First hold applies Performance.
        assert!(matches!(
            rx.recv().await,
            Some(Transition::SetProfile(ProfileName::Performance))
        ));

        shared
            .hold(
                ProfileName::PowerSaver,
                "r2".into(),
                "app2".into(),
                holder(":1.2"),
                ProfileName::Balanced,
            )
            .await;
        // Second hold outranks the first: PowerSaver wins.
        assert!(matches!(
            rx.recv().await,
            Some(Transition::SetProfile(ProfileName::PowerSaver))
        ));
    }

    #[tokio::test]
    async fn releasing_the_last_hold_restores_the_pre_hold_snapshot() {
        let (shared, mut rx) = shared_with_channel();
        let cookie = shared
            .hold(
                ProfileName::Performance,
                "r".into(),
                "app".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        assert!(matches!(
            rx.recv().await,
            Some(Transition::SetProfile(ProfileName::Performance))
        ));

        let (removed, target) = shared.remove_holds(|h| h.cookie == cookie).await;
        assert_eq!(removed, vec![cookie]);
        assert_eq!(target, None);
        shared.restore_pre_hold_snapshot().await;
        assert!(matches!(
            rx.recv().await,
            Some(Transition::SetProfile(ProfileName::Balanced))
        ));
    }

    #[tokio::test]
    async fn releasing_one_of_two_holds_reasserts_the_remaining_one() {
        let (shared, mut rx) = shared_with_channel();
        let cookie_a = shared
            .hold(
                ProfileName::PowerSaver,
                "r1".into(),
                "app1".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        rx.recv().await; // PowerSaver applied
        shared
            .hold(
                ProfileName::Performance,
                "r2".into(),
                "app2".into(),
                holder(":1.2"),
                ProfileName::Balanced,
            )
            .await;
        // PowerSaver still outranks the new Performance hold -> no new
        // transition is actually distinguishable here, but nothing errors.

        // Release the PowerSaver hold; only Performance remains.
        let (removed, target) = shared.remove_holds(|h| h.cookie == cookie_a).await;
        assert_eq!(removed, vec![cookie_a]);
        assert_eq!(target, Some(ProfileName::Performance));
    }

    #[tokio::test]
    async fn releasing_an_unknown_cookie_is_a_harmless_noop() {
        let (shared, _rx) = shared_with_channel();
        let (removed, target) = shared.remove_holds(|h| h.cookie == 999).await;
        assert!(removed.is_empty());
        assert_eq!(target, None);
    }

    #[tokio::test]
    async fn holder_disconnect_removes_only_that_holders_holds() {
        let (shared, mut rx) = shared_with_channel();
        shared
            .hold(
                ProfileName::Performance,
                "r1".into(),
                "app1".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        rx.recv().await;

        let (removed, target) = shared.remove_holds(|h| h.holder.as_str() == ":1.1").await;
        assert_eq!(removed.len(), 1);
        assert_eq!(target, None);
    }

    #[tokio::test]
    async fn peek_expected_reflects_current_holds_without_mutating() {
        let (shared, mut rx) = shared_with_channel();
        assert_eq!(shared.peek_expected().await, None);
        shared
            .hold(
                ProfileName::Performance,
                "r".into(),
                "app".into(),
                holder(":1.1"),
                ProfileName::Balanced,
            )
            .await;
        rx.recv().await;
        assert_eq!(shared.peek_expected().await, Some(ProfileName::Performance));
        // Peeking must not remove anything.
        assert_eq!(shared.holds.lock().await.len(), 1);
    }
}
