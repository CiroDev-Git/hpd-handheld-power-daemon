// SPDX-License-Identifier: GPL-3.0-or-later

use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, instrument, warn};

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::RuntimeConfig;
use hpd_error::HpdError;

use crate::effect::Effect;
use crate::reducer::reduce;
use crate::state::ProfileState;
use crate::transition::Transition;

/// Bound on the Transition mpsc channel. Sized for the bursty case of a
/// user spamming the CLI while AC + suspend events also enqueue
/// transitions; 32 has been more than enough in practice and gives
/// back-pressure long before any producer can OOM us.
pub const TRANSITION_CHANNEL_CAPACITY: usize = 32;

/// Main daemon orchestrator
pub struct Executor<B: HwBackend> {
    backend: B,
    device_limits: PowerEnvelopeLimits,
    /// Runtime-tunable knobs (cooling thresholds + SPPT/FPPT factors).
    /// Owned mutably by the executor and swapped on
    /// `Transition::ConfigReload` — passed to `reduce()` by reference on
    /// every iteration so the new values take effect immediately.
    config: RuntimeConfig,

    transition_rx: mpsc::Receiver<Transition>,

    state_tx: watch::Sender<ProfileState>,

    internal_tx: mpsc::Sender<Transition>,
    persister: crate::persistence::StatePersister,
}

impl<B: HwBackend> Executor<B> {
    /// Build a new executor and return it alongside a `watch::Receiver`
    /// observers can subscribe to (used by `hpd-dbus` to emit
    /// `PropertiesChanged`). `internal_tx` is a clone of the sender side
    /// of `transition_rx`; the executor uses it to enqueue
    /// `Transition::SyncPowerTarget` when a hardware write fails.
    pub fn new(
        backend: B,
        initial_state: ProfileState,
        device_limits: PowerEnvelopeLimits,
        config: RuntimeConfig,
        transition_rx: mpsc::Receiver<Transition>,
        internal_tx: mpsc::Sender<Transition>,
        persister: crate::persistence::StatePersister,
    ) -> (Self, watch::Receiver<ProfileState>) {
        let (state_tx, state_rx) = watch::channel(initial_state);

        let executor = Self {
            backend,
            device_limits,
            config,
            transition_rx,
            state_tx,
            internal_tx,
            persister,
        };

        (executor, state_rx)
    }

    /// Drains the transition channel until it closes, applying the
    /// reducer and dispatching the resulting effects in order. Exits
    /// cleanly once a `Transition::Shutdown` has been processed.
    pub async fn run(mut self) {
        info!("Executor started. Listening for transitions...");

        while let Some(transition) = self.transition_rx.recv().await {
            debug!(?transition, "Received transition");

            // ConfigReload mutates executor-owned runtime config, not
            // ProfileState. Intercept it before reduce() so the reducer
            // stays pure and the new values apply from the next iteration.
            if let Transition::ConfigReload(new_config) = transition {
                if new_config == self.config {
                    debug!("ConfigReload: no change");
                } else {
                    info!(
                        sppt_factor = new_config.sppt_factor,
                        fppt_factor = new_config.fppt_factor,
                        low_frac = new_config.profile_thresholds.low_frac,
                        high_frac = new_config.profile_thresholds.high_frac,
                        ac_max_performance = new_config.ac_max_performance,
                        "ConfigReload applied"
                    );
                    self.config = new_config;

                    // Refresh the derived `ac_locked` flag so a live toggle of
                    // `ac_max_performance` is reflected in `AcLocked` without
                    // waiting for the next AC edge. (The forced-max / restore
                    // *behaviour* still applies on the next plug/unplug — a
                    // SIGHUP does not force or restore mid-session.)
                    let mut state = self.state_tx.borrow().clone();
                    let locked = state.is_ac_connected && self.config.ac_max_performance;
                    if state.ac_locked != locked {
                        state.ac_locked = locked;
                        let _ = self.state_tx.send(state);
                    }
                }
                continue;
            }

            // Shutdown is processed through the reducer (which emits
            // PersistState), but we remember the variant so we can break
            // the loop *after* the resulting effects have been
            // dispatched — that guarantees state hits disk before exit.
            let is_shutdown = matches!(transition, Transition::Shutdown);

            let current_state = self.state_tx.borrow().clone();

            match reduce(
                &current_state,
                transition,
                &self.device_limits,
                &self.config,
            ) {
                Ok(output) => {
                    // `ac_locked` is derived, not persisted: recompute it from
                    // the post-transition AC state + live config so the D-Bus
                    // `AcLocked` property always reflects reality. The reducer
                    // owns the *behaviour* of the lock; the executor owns this
                    // reported flag (it holds the authoritative config).
                    let mut new_state = output.new_state;
                    new_state.ac_locked =
                        new_state.is_ac_connected && self.config.ac_max_performance;
                    if self.state_tx.send(new_state).is_err() {
                        error!("State observers dropped, stopping executor.");
                        break;
                    }

                    for effect in output.effects {
                        self.handle_effect(effect).await;
                    }
                    // Auto-cooling follow-up is the reducer's job:
                    // apply_power_target already infers and pushes the
                    // matching ApplyFanCurve effect when fan_follows_tdp is
                    // on. The platform profile is decoupled and never
                    // inferred. No post-reduce inference here.
                }
                Err(e) => {
                    error!(error = %e, "Reducer rejected transition due to invariant violation");
                }
            }

            if is_shutdown {
                info!("Shutdown processed, executor exiting");
                break;
            }
        }

        info!("Executor stopped.");
    }

    /// Dispatch a single effect to the backend. Every `Apply*` arm
    /// shares the same contract via [`Executor::rollback`]: on write
    /// failure, read the kernel-reported value back and re-inject the
    /// matching `Sync*` transition so the in-memory `ProfileState`
    /// stays consistent with hardware reality.
    ///
    /// `ApplyPlatformProfile` and `ApplyChargeThreshold` are no-ops
    /// when the underlying backend does not expose the matching
    /// capability ([`HwBackend::profile`] / [`HwBackend::charge`]
    /// returns `None`); the effect is logged at `debug` level and
    /// dropped. The reducer keeps emitting these effects regardless
    /// of capability presence — only the executor knows what the
    /// backend can do.
    #[instrument(skip(self), level = "debug")]
    async fn handle_effect(&self, effect: Effect) {
        match effect {
            Effect::ApplyPowerEnvelope(target) => {
                let power = self.backend.power();
                if let Err(e) = power.set_target(&target) {
                    self.rollback("ApplyPowerEnvelope", e, || {
                        power.get_target().map(Transition::SyncPowerTarget)
                    })
                    .await;
                } else {
                    debug!("Power Envelope applied successfully");
                }
            }
            Effect::ApplyPlatformProfile(new_profile) => {
                let Some(profile_cap) = self.backend.profile() else {
                    debug!(
                        effect = "ApplyPlatformProfile",
                        "Backend does not expose PlatformProfile; ignoring"
                    );
                    return;
                };
                if let Err(e) = profile_cap.set_active_profile(&new_profile) {
                    self.rollback("ApplyPlatformProfile", e, || {
                        profile_cap
                            .get_active_profile()
                            .map(Transition::SyncPlatformProfile)
                    })
                    .await;
                } else {
                    debug!("Platform Profile applied successfully");
                }
            }
            Effect::ApplyChargeThreshold(threshold) => {
                let Some(charge_cap) = self.backend.charge() else {
                    debug!(
                        effect = "ApplyChargeThreshold",
                        "Backend does not expose ChargeControl; ignoring"
                    );
                    return;
                };
                if let Err(e) = charge_cap.set_end_threshold(threshold) {
                    self.rollback("ApplyChargeThreshold", e, || {
                        charge_cap
                            .get_end_threshold()
                            .map(Transition::SyncChargeThreshold)
                    })
                    .await;
                } else {
                    debug!("Charge threshold applied successfully");
                }
            }
            Effect::ApplyFanCurve(selection) => {
                let Some(curve_cap) = self.backend.fan_curve() else {
                    debug!(
                        effect = "ApplyFanCurve",
                        "Backend does not expose FanCurveControl; ignoring"
                    );
                    return;
                };
                // The backend reads the curve back and fails closed if the
                // EC rejected it. On failure, roll the in-memory level back
                // to what the EC actually runs (read live) so the reported
                // `fan_curve` never claims a preset the hardware refused.
                if let Err(e) = curve_cap.apply(&selection) {
                    self.rollback("ApplyFanCurve", e, || {
                        curve_cap.active_selection().map(Transition::SyncFanCurve)
                    })
                    .await;
                } else {
                    debug!("Fan curve applied successfully");
                }
            }
            Effect::ResetFanCurve => {
                let Some(curve_cap) = self.backend.fan_curve() else {
                    debug!(
                        effect = "ResetFanCurve",
                        "Backend does not expose FanCurveControl; ignoring"
                    );
                    return;
                };
                if let Err(e) = curve_cap.reset_to_auto() {
                    self.rollback("ResetFanCurve", e, || {
                        curve_cap.active_selection().map(Transition::SyncFanCurve)
                    })
                    .await;
                } else {
                    debug!("Fan curve reset to firmware auto");
                }
            }
            Effect::PersistState => {
                let current_state = self.state_tx.borrow().clone();
                self.persister.save(&current_state).await;
            }
        }
    }

    /// Shared rollback path for every `Effect::Apply*` arm. On a backend
    /// write failure, attempt to read the current hardware state and
    /// re-inject the matching `Sync*` transition so `ProfileState`
    /// converges back to what the kernel actually reports.
    ///
    /// `build_sync_transition` is the per-effect adapter: it calls the
    /// matching `backend.get_*()` and wraps the value in the
    /// corresponding `Transition::Sync*` variant. Returning `Err` from
    /// it (hardware fully unreadable) leaves state diverged and logs a
    /// CRITICAL — by then we have no authoritative value to converge
    /// on, and refusing to drop pending effects is safer than guessing.
    async fn rollback<F>(&self, effect_label: &'static str, err: HpdError, build_sync_transition: F)
    where
        F: FnOnce() -> Result<Transition, HpdError>,
    {
        error!(effect = effect_label, error = %err, "Backend write failed");
        match build_sync_transition() {
            Ok(transition) => {
                warn!(
                    effect = effect_label,
                    "Rolling back in-memory state to match hardware"
                );
                if self.internal_tx.send(transition).await.is_err() {
                    error!(
                        effect = effect_label,
                        "Rollback transition could not be sent (executor channel closed)"
                    );
                }
            }
            Err(read_err) => error!(
                effect = effect_label,
                error = %read_err,
                "CRITICAL: hardware state unreadable after write failure"
            ),
        }
    }
}
