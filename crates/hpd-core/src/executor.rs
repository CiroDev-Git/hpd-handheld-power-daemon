use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, instrument};

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileThresholds;

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
    profile_thresholds: ProfileThresholds,
    
    transition_rx: mpsc::Receiver<Transition>,
    
    state_tx: watch::Sender<ProfileState>,

    internal_tx: mpsc::Sender<Transition>,
    persister: crate::persistence::StatePersister,
}

impl<B: HwBackend> Executor<B> {
    pub fn new(
        backend: B,
        initial_state: ProfileState,
        device_limits: PowerEnvelopeLimits,
        profile_thresholds: ProfileThresholds,
        transition_rx: mpsc::Receiver<Transition>,
        internal_tx: mpsc::Sender<Transition>,
        persister: crate::persistence::StatePersister,
    ) -> (Self, watch::Receiver<ProfileState>) {
        let (state_tx, state_rx) = watch::channel(initial_state);

        let executor = Self {
            backend,
            device_limits,
            profile_thresholds,
            transition_rx,
            state_tx,
            internal_tx,
            persister
        };

        (executor, state_rx)
    }

    pub async fn run(mut self) {
        info!("Executor started. Listening for transitions...");

        while let Some(transition) = self.transition_rx.recv().await {
            debug!(?transition, "Received transition");

            let current_state = self.state_tx.borrow().clone();

            match reduce(
                &current_state,
                transition,
                &self.device_limits,
                &self.profile_thresholds,
            ) {
                Ok(output) => {
                    if self.state_tx.send(output.new_state).is_err() {
                        error!("State observers dropped, stopping executor.");
                        break;
                    }

                    for effect in output.effects {
                        self.handle_effect(effect).await;
                    }
                    // Auto-cooling profile follow-up is the reducer's job:
                    // apply_target_and_profile already infers and pushes the
                    // matching ApplyPlatformProfile effect when fan_follows_tdp
                    // is on. No post-reduce inference here.
                }
                Err(e) => {
                    error!(error = %e, "Reducer rejected transition due to invariant violation");
                }
            }
        }

        info!("Executor stopped.");
    }

    /// Dispatch a single efect to some backend
    #[instrument(skip(self), level = "debug")]
    async fn handle_effect(&self, effect: Effect) {
        match effect {
            Effect::ApplyPowerEnvelope(target) => {
                if let Err(e) = self.backend.set_target(&target) {
                    error!(error = %e, "Failed to apply Power Envelope to hardware");
                    match self.backend.get_target() {
                        Ok(real_target) => {
                            error!("Rolling back state to match hardware reality: {:?}", real_target);
                            let _ = self.internal_tx.send(Transition::SyncPowerTarget(real_target)).await;
                        }
                        Err(read_err) => {
                            error!("CRITICAL: Hardware state unreadable after write failure: {}", read_err);
                        }
                    }
                } else {
                    debug!("Power Envelope applied successfully");
                }
            }
            Effect::ApplyPlatformProfile(profile) => {
                if let Err(e) = self.backend.set_active_profile(&profile) {
                    error!(error = %e, "Failed to apply Platform Profile via PPD");
                } else {
                    debug!("Platform Profile applied successfully");
                }
            }
            Effect::ApplyChargeThreshold(threshold) => {
                if let Err(e) = self.backend.set_end_threshold(threshold) {
                    error!(error = %e, "Failed to apply charge threshold");
                } else {
                    debug!("Charge threshold applied successfully");
                }
            }
            Effect::PersistState => {
                let current_state = self.state_tx.borrow().clone();
                self.persister.save(&current_state).await;
            }
        }
    }
}