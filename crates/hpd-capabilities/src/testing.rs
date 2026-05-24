// SPDX-License-Identifier: GPL-3.0-or-later

//! In-process test fixture implementing every L2 capability trait.
//!
//! Gated behind `#[cfg(any(test, feature = "testing"))]` so the production
//! daemon never links it. `MockBackend` records every write in `write_log`
//! and can be flipped into a failure mode via `fail_writes`, which the L3
//! `Executor` uses to exercise its rollback path.
//!
//! All inner state is `Arc`-wrapped so the fixture can be cloned and
//! introspected from the test thread after the original instance has been
//! moved into the `Executor`.
//!
//! Lint policy: `.expect()` on `Mutex::lock()` is intentional throughout —
//! mock fixtures never run alongside panicking code, and a poisoned mutex
//! here would indicate a test bug worth surfacing loudly, not a runtime
//! condition to recover from.
#![allow(clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use hpd_error::{BackendError, HpdError};

use crate::backend::HwBackend;
use crate::charge::{ChargeControl, DEFAULT_CHARGE_THRESHOLD};
use crate::fan::FanControl;
use crate::platform_profile::PlatformProfile;
use crate::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use crate::profile::ProfileName;
use crate::units::Rpm;

/// One observable write performed against the mock. Tests assert on the
/// ordered sequence of these to verify what the L3 executor dispatched.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordedCall {
    /// `PowerEnvelope::set_target` was invoked.
    SetTarget(PowerEnvelopeTarget),
    /// `PlatformProfile::set_active_profile` was invoked.
    SetProfile(ProfileName),
    /// `ChargeControl::set_end_threshold` was invoked.
    SetChargeThreshold(u8),
}

/// In-memory backend that satisfies `HwBackend`. Cloning shares state via
/// `Arc`, so the test thread can keep a handle for introspection while
/// moving another clone into the `Executor`.
#[derive(Clone)]
pub struct MockBackend {
    /// Current programmed power envelope.
    pub power: Arc<Mutex<PowerEnvelopeTarget>>,
    /// Current ACPI platform profile.
    pub profile: Arc<Mutex<ProfileName>>,
    /// Current charge end threshold (percent).
    pub charge: Arc<Mutex<u8>>,
    /// Hardware-reported envelope limits returned by `get_limits`.
    pub limits: PowerEnvelopeLimits,
    /// Value returned by `is_ac_connected()`.
    pub ac_connected: Arc<AtomicBool>,
    /// When true, every `set_*` returns `Err` without mutating internal
    /// state — letting `get_*` continue to report the real hardware value.
    pub fail_writes: Arc<AtomicBool>,
    /// Append-only log of every successful write the backend received.
    pub write_log: Arc<Mutex<Vec<RecordedCall>>>,
}

impl MockBackend {
    /// Build a fresh `MockBackend` seeded with the given initial envelope
    /// and reported limits. Other fields take their documented defaults
    /// (Balanced profile, AC disconnected, `fail_writes = false`, empty
    /// log, charge = `DEFAULT_CHARGE_THRESHOLD`).
    pub fn new(initial_power: PowerEnvelopeTarget, limits: PowerEnvelopeLimits) -> Self {
        Self {
            power: Arc::new(Mutex::new(initial_power)),
            profile: Arc::new(Mutex::new(ProfileName::Balanced)),
            charge: Arc::new(Mutex::new(DEFAULT_CHARGE_THRESHOLD)),
            limits,
            ac_connected: Arc::new(AtomicBool::new(false)),
            fail_writes: Arc::new(AtomicBool::new(false)),
            write_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a snapshot of every write recorded so far. Lock is
    /// released before returning, so calling this in a tight loop is
    /// fine.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.write_log
            .lock()
            .expect("mock mutex never poisoned in tests")
            .clone()
    }
}

fn fail(what: &'static str) -> HpdError {
    HpdError::Backend(BackendError::Other(format!(
        "mock: simulated failure on {}",
        what
    )))
}

impl PowerEnvelope for MockBackend {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        Ok(self.limits.clone())
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        Ok(self
            .power
            .lock()
            .expect("mock mutex never poisoned in tests")
            .clone())
    }

    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(fail("set_target"));
        }
        *self
            .power
            .lock()
            .expect("mock mutex never poisoned in tests") = target.clone();
        self.write_log
            .lock()
            .expect("mock mutex never poisoned in tests")
            .push(RecordedCall::SetTarget(target.clone()));
        Ok(())
    }
}

impl ChargeControl for MockBackend {
    fn is_ac_connected(&self) -> Result<bool, HpdError> {
        Ok(self.ac_connected.load(Ordering::SeqCst))
    }

    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(fail("set_end_threshold"));
        }
        *self
            .charge
            .lock()
            .expect("mock mutex never poisoned in tests") = threshold;
        self.write_log
            .lock()
            .expect("mock mutex never poisoned in tests")
            .push(RecordedCall::SetChargeThreshold(threshold));
        Ok(())
    }

    fn get_end_threshold(&self) -> Result<u8, HpdError> {
        Ok(*self
            .charge
            .lock()
            .expect("mock mutex never poisoned in tests"))
    }
}

impl PlatformProfile for MockBackend {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError> {
        Ok(self
            .profile
            .lock()
            .expect("mock mutex never poisoned in tests")
            .clone())
    }

    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(fail("set_active_profile"));
        }
        *self
            .profile
            .lock()
            .expect("mock mutex never poisoned in tests") = profile.clone();
        self.write_log
            .lock()
            .expect("mock mutex never poisoned in tests")
            .push(RecordedCall::SetProfile(profile.clone()));
        Ok(())
    }
}

impl FanControl for MockBackend {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError> {
        Ok(Rpm(0))
    }

    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError> {
        Ok(None)
    }
}

impl HwBackend for MockBackend {
    fn power(&self) -> &dyn PowerEnvelope {
        self
    }
    fn charge(&self) -> Option<&dyn ChargeControl> {
        Some(self)
    }
    fn profile(&self) -> Option<&dyn PlatformProfile> {
        Some(self)
    }
    fn fan(&self) -> Option<&dyn FanControl> {
        Some(self)
    }
}
