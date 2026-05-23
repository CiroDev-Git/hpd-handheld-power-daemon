use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileName, RuntimeConfig, TdpPreset};

/// Represents any external event who is trying to alter the state
#[derive(Debug, Clone)]
pub enum Transition {
    SetPreset(TdpPreset),
    SetSpl(u32),
    SetEnvelope(PowerEnvelopeTarget),
    SetProfile(ProfileName),
    ChargeThresholdChanged(u8),
    SyncPowerTarget(PowerEnvelopeTarget),
    AcPowerChanged(bool),
    SystemResumed,
    EnableFanAuto,
    /// Hot-reload of runtime-tunable config. Intercepted by the Executor
    /// before `reduce()` is called: the executor swaps its own
    /// `RuntimeConfig` and the next transition uses the new values. The
    /// reducer treats it as a no-op so calling `reduce()` with this
    /// variant in isolation (e.g. in unit tests) is harmless.
    ConfigReload(RuntimeConfig),
    /// Daemon is shutting down (SIGINT/SIGTERM received). The reducer
    /// emits `Effect::PersistState` so the in-memory state hits disk
    /// before the process exits; the Executor breaks its `run()` loop
    /// after dispatching the resulting effects.
    Shutdown,
}
