use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Represents an action with side effect (I/O) that Executor should dispatch.
///
/// D-Bus `PropertiesChanged` signals are NOT modelled here: they are emitted
/// implicitly by a dedicated task in `hpd-daemon` that subscribes to the
/// state `watch::Receiver` and calls the zbus-generated `<prop>_changed`
/// notifiers. See `daemon::main::spawn_properties_changed_emitter`.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Write a new power envelope to the L1 backend.
    ApplyPowerEnvelope(PowerEnvelopeTarget),
    /// Write the ACPI platform profile to the L1 backend.
    ApplyPlatformProfile(ProfileName),
    /// Write the battery charge end threshold to the L1 backend.
    ApplyChargeThreshold(u8),
    /// Flush the current `ProfileState` to disk via the persister.
    PersistState,
}
