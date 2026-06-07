// SPDX-License-Identifier: GPL-3.0-or-later

// zbus's `#[interface]` macro synthesises items (interface `name()`
// shim, `*_changed` signal emitters for properties) whose docs we
// can't attach via `///`. Suppress the lint module-wide; every
// human-written method in here is documented individually.
#![allow(missing_docs)]

use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};
use zbus::interface;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileName;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;

use hpd_capabilities::charge::{MAX_CHARGE_THRESHOLD, MIN_CHARGE_THRESHOLD};

use crate::actions::PolkitAction;
use crate::polkit;

/// Backing object for the `dev.cirodev.hpd.PowerDaemon1` D-Bus interface.
///
/// Holds the channels that connect the D-Bus layer to the rest of the
/// daemon — an [`mpsc::Sender`] for emitting [`Transition`]s into the
/// executor, a [`watch::Receiver`] for reading the current
/// [`ProfileState`] in property getters without locking, and a frozen
/// snapshot of the hardware [`PowerEnvelopeLimits`] for the
/// `get_hardware_limits` reply.
pub struct PowerDaemonInterface {
    tx: mpsc::Sender<Transition>,
    state_rx: watch::Receiver<ProfileState>,
    limits: PowerEnvelopeLimits,
    /// Shared backend handle, used only to read live telemetry (fan RPM,
    /// temperatures) on demand. Command mutations still flow through
    /// `tx` into the executor — this handle is never used to write.
    backend: Arc<dyn HwBackend>,
}

impl PowerDaemonInterface {
    /// Build the interface from the daemon's wiring. `tx` is the
    /// command lane into the [`Executor`](hpd_core::executor::Executor);
    /// `state_rx` is the live state mirror property getters read from;
    /// `limits` is the immutable hardware envelope detected at startup;
    /// `backend` is the shared handle used for live telemetry reads.
    pub fn new(
        tx: mpsc::Sender<Transition>,
        state_rx: watch::Receiver<ProfileState>,
        limits: PowerEnvelopeLimits,
        backend: Arc<dyn HwBackend>,
    ) -> Self {
        Self {
            tx,
            state_rx,
            limits,
            backend,
        }
    }

    /// Forward a [`Transition`] into the executor, mapping a closed
    /// command channel (executor gone) to the standard `executor_down()`
    /// D-Bus error. Every polkit-gated setter funnels through here so the
    /// "send or report executor down" boilerplate lives in one place.
    async fn send(&self, transition: Transition) -> zbus::fdo::Result<()> {
        if self.tx.send(transition).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }
}

/// Sentinel returned in [`PowerDaemonInterface::get_thermal_status`] for
/// a sensor or fan the hardware does not expose (or that failed to
/// read). Distinct from `0`, which is a valid reading (a stopped fan).
const TELEMETRY_UNAVAILABLE: i32 = i32::MIN;

fn auth_denied() -> zbus::fdo::Error {
    zbus::fdo::Error::AuthFailed("Not authorized by polkit".into())
}

fn executor_down() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("Internal daemon error: Executor down".into())
}

#[interface(name = "dev.cirodev.hpd.PowerDaemon1")]
impl PowerDaemonInterface {
    /// Set the Sustained Power Limit (SPL) in watts.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-tdp` (`auth_admin`).
    /// SPPT and FPPT are derived from SPL via the runtime multipliers
    /// (`sppt_factor` / `fppt_factor`) in the executor.
    async fn set_spl(
        &self,
        watts: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set SPL: {}W", watts);
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetSpl(watts)).await
    }

    /// Apply a named TDP preset (`eco`, `balanced`, `max`).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-tdp`.
    async fn set_preset(
        &self,
        preset_name: String,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Preset: {}", preset_name);
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        let preset = preset_name
            .parse::<hpd_capabilities::profile::TdpPreset>()
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        self.send(Transition::SetPreset(preset)).await
    }

    /// Current SPL in whole watts (the UI never sees milliwatts).
    #[zbus(property)]
    async fn current_spl(&self) -> u32 {
        // UI shows whole watts; conversion lives on the value type.
        self.state_rx.borrow().power_target.spl.as_watts()
    }

    /// Set the battery charge end threshold (percentage, 20-100).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-charge`.
    async fn set_charge_threshold(
        &self,
        threshold: u8,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Charge Limit: {}%", threshold);
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Charge limit must be between 20 and 100".into(),
            ));
        }
        if !polkit::check(conn, &header, PolkitAction::SetCharge).await {
            return Err(auth_denied());
        }
        self.send(Transition::ChargeThresholdChanged(threshold))
            .await
    }

    #[zbus(property)]
    async fn active_profile(&self) -> String {
        // ProfileName::Display is the stable D-Bus contract (kebab-case,
        // roundtrips through FromStr). Do not use Debug here — Debug is an
        // internal representation that can change with refactors.
        self.state_rx.borrow().active_profile.to_string()
    }

    /// Current battery charge end threshold (percentage).
    #[zbus(property)]
    async fn charge_end_threshold(&self) -> u8 {
        self.state_rx.borrow().charge_end_threshold
    }

    /// Whether the daemon is currently inferring the **fan curve** from
    /// the TDP envelope (auto cooling) or honouring the operator's last
    /// explicit `set_cooling_level` (manual cooling).
    ///
    /// Backed by `ProfileState::fan_follows_tdp`. Exposed on D-Bus so
    /// status widgets (KDE / GNOME power applets, overlay HUDs) can
    /// surface the mode without inferring it from observed behaviour.
    /// Re-enable auto cooling with `set_fan_auto`; `set_cooling_level`
    /// flips it back to `false`. The power-profile lever `set_profile` is
    /// decoupled and does **not** affect this flag.
    #[zbus(property)]
    async fn auto_cooling(&self) -> bool {
        self.state_rx.borrow().fan_follows_tdp
    }

    /// The daemon's own version (the crate's `CARGO_PKG_VERSION`, e.g.
    /// `2.4.2`). Read-only, unauthenticated — lets a client (the Decky
    /// plugin) show which daemon it's talking to. A client predating this
    /// method gets a D-Bus error and should fall back to "unknown".
    async fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Hardware-imposed envelope limits (all watts):
    /// `(spl_min, spl_max, sppt_max, fppt_max)`.
    async fn get_hardware_limits(&self) -> zbus::fdo::Result<(u32, u32, u32, u32)> {
        Ok((
            self.limits.spl_min.as_watts(),
            self.limits.spl_max.as_watts(),
            self.limits.sppt_max.as_watts(),
            self.limits.fppt_max.as_watts(),
        ))
    }

    /// Whether the charger is currently plugged in. Re-queried at boot
    /// and updated live from the netlink monitor.
    ///
    /// Kept as a method for backwards compatibility; prefer the
    /// [`ac_connected`](Self::ac_connected) property, which emits
    /// `PropertiesChanged` so clients can react without polling.
    async fn is_ac_connected(&self) -> zbus::fdo::Result<bool> {
        Ok(self.state_rx.borrow().is_ac_connected)
    }

    /// Whether the charger is currently plugged in — as a **property**, so
    /// it emits `PropertiesChanged` on every AC plug/unplug edge (the
    /// `is_ac_connected()` method above does not). Lets clients (the Decky
    /// plugin) drop their AC poll. Re-queried at boot, updated live from
    /// the netlink monitor.
    #[zbus(property)]
    async fn ac_connected(&self) -> bool {
        self.state_rx.borrow().is_ac_connected
    }

    /// Set the ACPI `platform_profile` manually (`power-saver`,
    /// `balanced`, `performance`, or a custom vendor string) — the EPP /
    /// power-bias lever, surfaced to users as "Power mode". Decoupled from
    /// cooling: it does **not** touch the fan curve or the `auto_cooling`
    /// flag (use `set_cooling_level` / `set_fan_auto` for those).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_profile(
        &self,
        profile: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        let profile_enum = ProfileName::from_str(profile).map_err(zbus::fdo::Error::InvalidArgs)?;
        self.send(Transition::SetProfile(profile_enum)).await
    }

    /// Cooling lever: set the fan-curve level (`silent`, `balanced`,
    /// `aggressive`) and latch manual cooling. Decoupled from power — it
    /// programs the fan curve only and does **not** touch the platform
    /// profile / power envelope (use `set_spl` / `set_profile` for those).
    ///
    /// This is the front-end for `hpdctl cool set`. The raw `set_profile`
    /// method remains for advanced callers (the unused raw `set_fan_curve`
    /// was retired in 2.5.0 — `set_cooling_level` covers the fan curve).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_cooling_level(
        &self,
        level: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Cooling Level: {}", level);
        let preset = FanCurvePreset::from_str(level)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetCoolingLevel(preset)).await
    }

    /// Re-enable auto-cooling: the daemon resumes inferring the **fan
    /// curve** from the active TDP envelope (the platform profile is
    /// unaffected — it is a decoupled power lever).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile`.
    async fn set_fan_auto(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::EnableFanAuto).await
    }

    /// Hand fan control back to the firmware's automatic curve.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (grouped with the
    /// other cooling levers `set_cooling_level` / `set_fan_auto`; the
    /// dedicated `set-fan-curve` action was retired in 2.5.0 along with
    /// the unused raw `set_fan_curve` method).
    async fn reset_fan_curve(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to reset fan curve to firmware auto");
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::ResetFanCurve).await
    }

    /// Live thermal telemetry:
    /// `(cpu_temp_c, gpu_temp_c, cpu_fan_rpm, gpu_fan_rpm, soc_power_mw)`.
    /// The first four are whole units (°C, RPM); the last is the actual
    /// SoC power draw in **milliwatts**.
    ///
    /// Read on demand straight from the backend, so callers
    /// (`hpdctl status` / `monitor`) always see current values. Any
    /// field equals `i32::MIN` when that sensor or fan is not exposed by
    /// the hardware (or its read failed) — note `0` is a *valid* fan
    /// reading (a stopped fan), so absence needs its own sentinel.
    async fn get_thermal_status(&self) -> (i32, i32, i32, i32, i32) {
        let cpu_temp = self
            .backend
            .thermal()
            .and_then(|t| t.get_cpu_temp().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |c| i32::from(c.0));
        let gpu_temp = self
            .backend
            .thermal()
            .and_then(|t| t.get_gpu_temp().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |c| i32::from(c.0));
        let cpu_rpm = self
            .backend
            .fan()
            .and_then(|f| f.get_cpu_fan_rpm().ok())
            .map_or(TELEMETRY_UNAVAILABLE, |r| i32::from(r.0));
        let gpu_rpm = self
            .backend
            .fan()
            .and_then(|f| f.get_gpu_fan_rpm().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |r| i32::from(r.0));
        let soc_power_mw = self
            .backend
            .thermal()
            .and_then(|t| t.get_soc_power().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |p| {
                i32::try_from(p.0).unwrap_or(i32::MAX)
            });
        (cpu_temp, gpu_temp, cpu_rpm, gpu_rpm, soc_power_mw)
    }

    /// The eight `(temp_c, pwm)` points of the active CPU and GPU fan
    /// curves as read back from the EC: `(cpu_points, gpu_points)`. `pwm`
    /// is 0–255. Both vectors are empty when the backend exposes no
    /// programmable curve or the read-back failed. Used by
    /// `hpdctl cool curve` to draw the curve.
    async fn get_fan_curve(&self) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
        let Some(cap) = self.backend.fan_curve() else {
            return (Vec::new(), Vec::new());
        };
        match cap.get_curves() {
            Ok(curves) => {
                let to_pairs = |c: &hpd_capabilities::fan_curve::FanCurve| {
                    c.points
                        .iter()
                        .map(|p| (u32::from(p.temp_c), u32::from(p.pwm)))
                        .collect::<Vec<_>>()
                };
                (to_pairs(&curves.cpu), to_pairs(&curves.gpu))
            }
            Err(_) => (Vec::new(), Vec::new()),
        }
    }

    /// Active fan-curve selection: a preset name (`silent`, `balanced`,
    /// `aggressive`), `custom` for an explicit curve, or `auto` when the
    /// firmware's automatic curve is in charge (daemon not managing it).
    #[zbus(property)]
    async fn fan_curve(&self) -> String {
        match self.state_rx.borrow().active_fan_curve {
            None => "auto".to_string(),
            Some(FanCurveSelection::Preset(p)) => p.as_str().to_string(),
            Some(FanCurveSelection::Custom { .. }) => "custom".to_string(),
        }
    }

    /// Daemon self-diagnostics: `(polkit_ok, missing_action_ids)`.
    ///
    /// `polkit_ok == false` (a non-empty `missing_action_ids`) means the
    /// polkit policy was never installed, so every privileged command is
    /// denied with `AuthFailed`. `hpdctl status` / `hpdctl doctor` and the
    /// Decky plugin render this to point the user at `hpdctl fix-polkit`.
    ///
    /// A transport error talking to polkit is reported as "ok" (empty
    /// missing list): the loud startup self-check already covers that
    /// rarer case, and we would rather not cry wolf over a transient
    /// polkit hiccup on every status poll.
    async fn get_diagnostics(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> (bool, Vec<String>) {
        let missing: Vec<String> = polkit::missing_actions(conn)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(String::from)
            .collect();
        (missing.is_empty(), missing)
    }

    /// Friendly names of competing power daemons currently live on the bus
    /// (e.g. `power-profiles-daemon`, `steamos-manager`).
    ///
    /// These write the same TDP / platform-profile / charge surfaces hpd
    /// owns; running one alongside hpd makes the effective state flap. An
    /// empty list means hpd is the sole power owner. The repair is the
    /// user-side `hpdctl doctor` run in fix mode (the `fix` subcommand
    /// flag) — the daemon is sandboxed and cannot disable another service
    /// itself. See [`crate::conflicts`].
    //
    // NOTE: keep this doc-comment free of `--` (two ASCII hyphens). zbus
    // copies each `///` line verbatim into the introspection `<!-- ... -->`
    // block, and XML forbids `--` inside a comment; a stray `--fix` here
    // produced a malformed document that strict parsers (Python expat, used
    // by the Decky plugin's dbus-next) rejected outright. The
    // `introspection_xml_is_well_formed` regression test guards this.
    async fn get_power_conflicts(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Vec<String> {
        crate::conflicts::power_conflicts(conn).await
    }

    /// Friendly names of power-adjacent advisory daemons currently live on
    /// the bus (today Feral `gamemoded`, activated by Steam / Lutris around
    /// a running game).
    ///
    /// Unlike `get_power_conflicts`, these are not rivals to neutralize:
    /// they may raise the CPU governor while a game runs, which is wanted,
    /// so `hpdctl doctor` in fix mode reports them but never masks them. An
    /// empty list means no advisory daemon is live. The call errors against a
    /// daemon predating this method; callers degrade to "unknown". See
    /// [`crate::conflicts`].
    //
    // NOTE: keep this doc-comment free of `--` (two ASCII hyphens) for the
    // same XML-introspection reason documented on `get_power_conflicts`.
    async fn get_advisory_daemons(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Vec<String> {
        crate::conflicts::advisory_daemons(conn).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
    use hpd_capabilities::testing::MockBackend;
    use hpd_capabilities::units::PowerMilliwatts;
    use std::sync::Arc;
    // Brings `introspect_to_writer` into scope — it's a method on the
    // `Interface` trait the `#[interface]` macro implements for us.
    use zbus::object_server::Interface;

    fn sample_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15000),
                sppt: PowerMilliwatts(17000),
                fppt: Some(PowerMilliwatts(20000)),
            },
            active_profile: hpd_capabilities::profile::ProfileName::Balanced,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_target: None,
            active_fan_curve: None,
            is_ac_connected: false,
        }
    }

    fn sample_limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(53000),
        }
    }

    /// Render the introspection document for the exported object path exactly
    /// as zbus' `ObjectServer` assembles it: the DOCTYPE + `<node>` envelope
    /// wrapping our interface's `introspect_to_writer` output (the same
    /// generation path `Introspectable.Introspect` serves at runtime). The
    /// standard `Peer` / `Properties` / `Introspectable` interfaces zbus
    /// injects carry no user doc-comments and are always well-formed, so our
    /// own interface is the only regression surface that matters.
    fn introspection_document() -> String {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (_state_tx, state_rx) = tokio::sync::watch::channel(sample_state());
        let backend: Arc<dyn HwBackend> = Arc::new(MockBackend::new(
            sample_state().power_target,
            sample_limits(),
        ));
        let iface = PowerDaemonInterface::new(tx, state_rx, sample_limits(), backend);

        let mut body = String::new();
        iface.introspect_to_writer(&mut body, 2);

        format!(
            "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n\
             \x20\"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n\
             <node>\n{body}</node>\n"
        )
    }

    /// The introspection XML for the exported object path must be well-formed
    /// under a *strict* parser. A `///` doc-comment containing `--` slips past
    /// lenient parsers (libxml2 / gdbus) but is rejected by Python's expat,
    /// which broke the Decky plugin (panel stuck on "Daemon: unreachable").
    #[test]
    fn introspection_xml_is_well_formed() {
        let xml = introspection_document();

        let mut reader = quick_xml::Reader::from_str(&xml);
        reader.config_mut().check_comments = true;

        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!(
                    "introspection XML is not well-formed under a strict parser: {e}\n\
                     A `///` doc-comment on the D-Bus interface almost certainly contains \
                     `--`, which XML forbids inside a comment. Document:\n{xml}"
                ),
            }
        }
    }

    /// Belt-and-braces guard for the exact bug that shipped: `--` inside any
    /// `<!-- ... -->` block. Independent of the parser's default config, so a
    /// future `quick-xml` bump that changes comment handling can't silence it.
    #[test]
    fn introspection_comments_have_no_double_hyphen() {
        let xml = introspection_document();
        for (start, _) in xml.match_indices("<!--") {
            let rest = &xml[start + 4..];
            let end = rest
                .find("-->")
                .expect("every opened XML comment is closed");
            let body = &rest[..end];
            assert!(
                !body.contains("--"),
                "XML comment body contains `--`, which strict parsers reject:\n{body}"
            );
        }
    }
}
