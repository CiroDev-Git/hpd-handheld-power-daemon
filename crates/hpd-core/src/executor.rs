// SPDX-License-Identifier: GPL-3.0-or-later

use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, instrument, warn};

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::RuntimeConfig;
use hpd_error::HpdError;

use crate::effect::Effect;
use crate::inference::gpu_clock_range_for_tier;
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
                        "ConfigReload applied"
                    );
                    self.config = new_config;
                }
                continue;
            }

            // Shutdown is processed through the reducer (which emits
            // PersistState), but we remember the variant so we can break
            // the loop *after* the resulting effects have been
            // dispatched — that guarantees state hits disk before exit.
            let is_shutdown = matches!(transition, Transition::Shutdown);

            let mut current_state = self.state_tx.borrow().clone();

            // On boot/resume, re-read the *real* AC state from hardware before
            // applying the policy. The in-memory `is_ac_connected` can be stale
            // across a suspend if the charger was (un)plugged while suspended
            // and the udev event was missed or arrives after `SystemResumed` —
            // without this, the daemon could (un)lock against the wrong power
            // source until the next live AC edge. Makes `SystemResumed`
            // authoritative regardless of the netlink monitor's timing.
            if matches!(transition, Transition::SystemResumed) {
                if let Some(charge) = self.backend.charge() {
                    match charge.is_ac_connected() {
                        Ok(ac) => current_state.is_ac_connected = ac,
                        Err(e) => {
                            warn!(error = %e, "Resume: could not re-read AC state; using last known")
                        }
                    }
                }
            }

            match reduce(
                &current_state,
                transition,
                &self.device_limits,
                &self.config,
            ) {
                Ok(output) => {
                    // `ac_locked` is derived, not persisted: recompute it from
                    // the post-transition AC state + the (persisted, toggleable)
                    // `ac_max_performance` preference so the D-Bus `AcLocked`
                    // property always reflects reality. The reducer owns the
                    // *behaviour* of the lock; the executor owns this reported
                    // flag.
                    let mut new_state = output.new_state;
                    new_state.ac_locked = new_state.is_ac_connected && new_state.ac_max_performance;
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
            Effect::ApplyGpuClockRange(tier) => {
                let Some(gpu_cap) = self.backend.gpu_clock() else {
                    debug!(
                        effect = "ApplyGpuClockRange",
                        "Backend does not expose GpuClockRangeControl; ignoring"
                    );
                    return;
                };
                // Resolving a tier to a concrete range needs BOTH the
                // executor's `RuntimeConfig` (the curated fractions) AND a
                // live hardware read (`constraints()`) — the reducer has
                // the former but must never do the latter, and the L1
                // backend must never see the former. The executor is the
                // only layer holding both, so it alone can turn the
                // curated tier into a concrete `GpuClockRange`.
                let range = match gpu_cap.constraints() {
                    Ok(constraints) => gpu_clock_range_for_tier(
                        tier,
                        &constraints,
                        &self.config.gpu_clock_fractions,
                    ),
                    Err(e) => {
                        error!(
                            effect = "ApplyGpuClockRange",
                            error = %e,
                            "Could not read live GPU clock constraints; skipping"
                        );
                        return;
                    }
                };
                if let Err(e) = gpu_cap.set_range(&range) {
                    self.rollback("ApplyGpuClockRange", e, || {
                        gpu_cap.active_range().map(Transition::SyncGpuClockRange)
                    })
                    .await;
                } else {
                    debug!("GPU clock range applied successfully");
                }
            }
            Effect::ResetGpuClocks => {
                let Some(gpu_cap) = self.backend.gpu_clock() else {
                    debug!(
                        effect = "ResetGpuClocks",
                        "Backend does not expose GpuClockRangeControl; ignoring"
                    );
                    return;
                };
                if let Err(e) = gpu_cap.reset_to_auto() {
                    self.rollback("ResetGpuClocks", e, || {
                        gpu_cap.active_range().map(Transition::SyncGpuClockRange)
                    })
                    .await;
                } else {
                    debug!("GPU clock reset to firmware auto");
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
    ///
    /// Enqueues the `Sync*` transition with `try_send`, not `send().await`:
    /// this runs on the executor's own task, on the *same* bounded channel
    /// (`TRANSITION_CHANNEL_CAPACITY`) it drains, so an `.await` here would
    /// block the only consumer that could ever free up capacity — a full
    /// channel would deadlock the executor (and with it the whole daemon)
    /// forever. Dropping a rollback under saturation is safe: the next
    /// boot/resume re-assert reconciles state against hardware anyway.
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
                match self.internal_tx.try_send(transition) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => error!(
                        effect = effect_label,
                        "Rollback transition dropped: executor channel is full \
                         (state will reconcile on the next boot/resume re-assert)"
                    ),
                    Err(mpsc::error::TrySendError::Closed(_)) => error!(
                        effect = effect_label,
                        "Rollback transition could not be sent (executor channel closed)"
                    ),
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

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely; the
    // strict bar in `[workspace.lints.clippy]` applies to production code
    // only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::power::PowerEnvelopeTarget;
    use hpd_capabilities::profile::ProfileName;
    use hpd_capabilities::testing::MockBackend;
    use hpd_capabilities::units::PowerMilliwatts;
    use std::time::Duration;

    fn limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7_000),
            spl_max: PowerMilliwatts(35_000),
            sppt_min: PowerMilliwatts(7_000),
            sppt_max: PowerMilliwatts(43_000),
            fppt_min: PowerMilliwatts(7_000),
            fppt_max: PowerMilliwatts(55_000),
        }
    }

    fn sample_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15_000),
                sppt: PowerMilliwatts(15_000),
                fppt: Some(PowerMilliwatts(15_000)),
            },
            active_profile: ProfileName::Balanced,
            is_ac_connected: false,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_state: None,
            active_fan_curve: None,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
            ac_max_performance: true,
            ac_locked: false,
        }
    }

    /// Regression for Audit §2.2 (2026-07): `rollback` used to `.await` a
    /// `send()` on the *same* bounded channel the executor's own `run()`
    /// loop drains. Had that channel ever been saturated when a rollback
    /// fired, the `.await` would never resolve — the only task that could
    /// free capacity is the one blocked on the send — deadlocking the
    /// executor, and with it the whole daemon, forever. `try_send` must
    /// return immediately instead, regardless of channel state.
    #[tokio::test]
    async fn rollback_does_not_block_when_channel_is_saturated() {
        let backend = MockBackend::new(sample_state().power_target.clone(), limits());

        let path = std::env::temp_dir().join(format!(
            "hpd_executor_rollback_test_{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let persister = crate::persistence::StatePersister::new(&path);

        // Capacity-1 channel, saturated by a transition nothing ever drains
        // (this test never spawns `run()`), so any `.await`-based send
        // would hang forever.
        let (tx, rx) = mpsc::channel(1);
        tx.try_send(Transition::Shutdown)
            .expect("first send into an empty capacity-1 channel must succeed");

        let (executor, _state_rx) = Executor::new(
            backend,
            sample_state(),
            limits(),
            RuntimeConfig::DEFAULT,
            rx,
            tx,
            persister,
        );

        let sync_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(10_000),
            sppt: PowerMilliwatts(11_500),
            fppt: Some(PowerMilliwatts(12_500)),
        };
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            executor.rollback("Test", HpdError::FeatureUnsupported, || {
                Ok(Transition::SyncPowerTarget(sync_target.clone()))
            }),
        )
        .await;

        assert!(
            result.is_ok(),
            "rollback must return promptly on a saturated channel, not hang"
        );

        let _ = std::fs::remove_file(&path);
    }
}
