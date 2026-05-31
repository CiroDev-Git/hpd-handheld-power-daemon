// SPDX-License-Identifier: GPL-3.0-or-later

//! Polkit authorization helper.
//!
//! The interface methods on [`crate::service::PowerDaemonInterface`] call
//! [`check`] before enqueuing a privileged `Transition`. We talk to
//! polkit by hand over `org.freedesktop.PolicyKit1.Authority` instead of
//! pulling in a dedicated crate — the call is small enough that the
//! extra dependency is not worth it.
//!
//! The decision itself lives in polkit, not here: the `auth_admin`
//! defaults in `package/polkit/dev.cirodev.hpd.policy` gate
//! non-administrator callers, while `package/polkit/49-hpd.rules` grants
//! `wheel`-group members every `dev.cirodev.hpd.*` action without a
//! prompt (keyed on group membership, so it holds even when a
//! physically-local session registers as `Remote=yes`). This module only
//! asks the question and enforces the answer fail-closed.
//!
//! ## Failure mode: fail-closed
//!
//! Any error from polkit (proxy creation failure, method call timeout,
//! malformed reply) is logged as a warning and the call returns `false`.
//! The daemon would rather refuse a legitimate request than allow an
//! unauthenticated one through.
//!
//! ## Simulator builds
//!
//! Under `#[cfg(feature = "simulator")]` the helper unconditionally
//! returns `true`. Session-bus runs on macOS / dev hosts have no polkit
//! authority to talk to and gating every setter would make the
//! simulator unusable.
//!
//! ## Registration self-check
//!
//! [`missing_actions`] asks polkit to enumerate its registered actions
//! and returns the subset of [`PolkitAction::ALL`] it does not know.
//! The daemon runs this at startup (loud warning if anything is missing)
//! and exposes it over D-Bus via `get_diagnostics` so `hpdctl status`
//! and the Decky plugin can tell the user *why* every privileged command
//! is being denied: the polkit policy was never installed.

use tracing::debug;
#[cfg(not(feature = "simulator"))]
use tracing::warn;

use crate::actions::PolkitAction;

/// Polkit-managed authorization check for a single privileged action.
///
/// `header` must be the incoming method-call header; we read the
/// sender's unique bus name from it to build the polkit subject.
/// Returns `true` only when polkit explicitly says "authorized".
pub async fn check(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action: PolkitAction,
) -> bool {
    check_inner(conn, header, action.as_id()).await
}

#[cfg(feature = "simulator")]
async fn check_inner(
    _conn: &zbus::Connection,
    _header: &zbus::message::Header<'_>,
    action_id: &str,
) -> bool {
    debug!(action = action_id, "Polkit bypassed (simulator build)");
    true
}

#[cfg(not(feature = "simulator"))]
async fn check_inner(
    conn: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action_id: &str,
) -> bool {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    let Some(sender) = header.sender() else {
        warn!(
            action = action_id,
            "Method call has no sender; denying (fail-closed)"
        );
        return false;
    };
    let sender_name = sender.as_str().to_string();

    // polkit subject for a D-Bus caller: ("system-bus-name", {"name": "<unique-name>"}).
    let mut subject_details: HashMap<String, Value<'_>> = HashMap::new();
    subject_details.insert("name".to_string(), Value::new(sender_name.clone()));
    let subject = ("system-bus-name".to_string(), subject_details);

    let details: HashMap<&str, &str> = HashMap::new();
    // 1 = AllowUserInteraction: let polkit pop a prompt agent if needed.
    let flags: u32 = 1;
    let cancellation_id = "";

    let proxy = match zbus::Proxy::new(
        conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, action = action_id, "Polkit proxy creation failed; denying (fail-closed)");
            return false;
        }
    };

    let result: zbus::Result<(bool, bool, HashMap<String, String>)> = proxy
        .call(
            "CheckAuthorization",
            &(subject, action_id, details, flags, cancellation_id),
        )
        .await;

    match result {
        Ok((is_authorized, is_challenge, _)) => {
            debug!(
                action = action_id,
                sender = %sender_name,
                authorized = is_authorized,
                challenge = is_challenge,
                "Polkit check returned"
            );
            is_authorized
        }
        Err(e) => {
            // Special-case the single most common deployment failure: the
            // action is not registered with polkit because the policy file
            // was never installed (binary copied without
            // package/polkit/*). Polkit reports this as
            // `...PolicyKit1.Error.Failed: Action <id> is not registered`.
            // Without this branch the only symptom is an opaque AuthFailed
            // on every setter, which is exactly the trap this code path
            // exists to make obvious in the journal.
            let msg = e.to_string();
            if msg.contains("not registered") {
                warn!(
                    action = action_id,
                    "Polkit does not know this action — the hpd polkit policy is NOT installed. \
                     Every privileged command will be denied. Fix: reinstall via ./install.sh or the AUR \
                     package, or copy package/polkit/dev.cirodev.hpd.policy → /usr/share/polkit-1/actions/ \
                     and package/polkit/49-hpd.rules → /usr/share/polkit-1/rules.d/, then restart polkit. Denying (fail-closed)."
                );
            } else {
                warn!(error = %e, action = action_id, sender = %sender_name, "Polkit call failed; denying (fail-closed)");
            }
            false
        }
    }
}

/// Action IDs the daemon expects polkit to know about that polkit does
/// **not** currently have registered.
///
/// This mirrors what `pkaction` / `pkcheck` would report: it asks polkit
/// to enumerate every registered action and returns the subset of
/// [`PolkitAction::ALL`] that is missing. An empty vector means every
/// privileged setter is gated by a real, installed policy. A non-empty
/// vector almost always means a partial install (the binary was deployed
/// without `package/polkit/*`), which makes every privileged setter fail
/// with an opaque `AuthFailed`.
///
/// Errors only on a transport-level failure talking to polkit (polkit
/// not running, malformed reply) — callers should treat that as "cannot
/// confirm authorization is wired up" and surface it the same way as a
/// missing action.
#[cfg(not(feature = "simulator"))]
pub async fn missing_actions(
    conn: &zbus::Connection,
) -> Result<Vec<&'static str>, zbus::Error> {
    use std::collections::HashSet;

    // Polkit `EnumerateActions` returns `a(ssssssuuua{ss})`: per action
    // (action_id, description, message, vendor_name, vendor_url,
    // icon_name, implicit_any, implicit_inactive, implicit_active,
    // annotations). We only need the first field. Deserializing into a
    // tuple avoids defining a typed struct (and pulling in serde derive).
    type ActionDescription = (
        String,
        String,
        String,
        String,
        String,
        String,
        u32,
        u32,
        u32,
        std::collections::HashMap<String, String>,
    );

    let proxy = zbus::Proxy::new(
        conn,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;

    // The single argument is a locale; "" asks polkit for the default.
    let registered: Vec<ActionDescription> =
        proxy.call("EnumerateActions", &("",)).await?;
    let known: HashSet<&str> = registered.iter().map(|a| a.0.as_str()).collect();

    Ok(PolkitAction::ALL
        .iter()
        .map(|a| a.as_id())
        .filter(|id| !known.contains(id))
        .collect())
}

/// Simulator builds bypass polkit entirely (session bus, no authority),
/// so there is nothing to verify — report every action as registered.
#[cfg(feature = "simulator")]
pub async fn missing_actions(
    _conn: &zbus::Connection,
) -> Result<Vec<&'static str>, zbus::Error> {
    Ok(Vec::new())
}
