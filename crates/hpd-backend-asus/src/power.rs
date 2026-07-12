// SPDX-License-Identifier: GPL-3.0-or-later

use std::thread;
use std::time::Duration;

use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::units::PowerMilliwatts;
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

const BASE_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";

// Canonical attribute names exposed by the upstream `asus-armoury` driver.
// Verified on ROG Xbox Ally X (board RC73XA) against Linux's
// drivers/platform/x86/asus-armoury/.
const ATTR_SPL: &str = "ppt_pl1_spl";
const ATTR_SPPT: &str = "ppt_pl2_sppt";
const ATTR_FPPT: &str = "ppt_pl3_fppt";

/// Found on-device (2026-07-12): writing SPPT/FPPT immediately after a
/// large SPL jump (e.g. the AC-lock forcing max TDP right after a low
/// battery-side value) can occasionally hit a transient `EINVAL` from the
/// EC/firmware — the write is mathematically valid (within
/// `PowerEnvelopeLimits`) but the sustained-rail change hadn't settled
/// yet. A single short retry clears it; this is not a workaround for an
/// out-of-range value (that still fails immediately, see
/// `write_watts_with_settle_retry`'s doc comment) — see the executor's
/// existing rollback, which already converges to a consistent state if
/// even the retry fails.
const SETTLE_RETRY_DELAY: Duration = Duration::from_millis(75);

// Fallback boost-rail maxima for ASUS handhelds when `max_value` is not
// exposed by the driver. Documented values for the ROG Ally / Ally X /
// Xbox Ally X family.
const ASUS_DEFAULT_SPPT_MAX_MW: u32 = 43_000;
const ASUS_DEFAULT_FPPT_MAX_MW: u32 = 53_000;

// Fallback boost-rail *minima* for ASUS handhelds when `min_value` is not
// exposed by the driver. Verified live on the ROG Xbox Ally X (RC73XA):
// `ppt_pl2_sppt`/`ppt_pl3_fppt` report 13W/19W respectively, both above
// `ppt_pl1_spl`'s 7W floor — assumed to hold across the same family as
// the maxima above (same silicon generation), pending confirmation on
// other boards.
const ASUS_DEFAULT_SPPT_MIN_MW: u32 = 13_000;
const ASUS_DEFAULT_FPPT_MIN_MW: u32 = 19_000;

/// [`PowerEnvelope`] implementation for ASUS handhelds.
///
/// Reads and writes the SPL / SPPT / FPPT rails through the upstream
/// `asus-armoury` firmware-attributes driver. The kernel exposes those
/// rails in **watts** (integer); this backend converts to/from the
/// domain's [`PowerMilliwatts`] at the I/O boundary so the rest of the
/// reducer never sees raw kernel units.
///
/// Falls back to the documented ROG Ally / Ally X / Xbox Ally X
/// boost-rail maxima when the `max_value` attribute is missing.
pub struct AsusPowerBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusPowerBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    fn read_watts(&self, attr: &str, suffix: &str) -> Result<PowerMilliwatts, HpdError> {
        let path = format!("{}/{}/{}", BASE_PATH, attr, suffix);
        let val_str = self.sysfs.read_string(&path)?;
        let watts: u32 = val_str.parse().map_err(|_| BackendError::ParseFailed {
            field: "watts",
            raw: val_str.clone(),
            reason: format!("expected integer at {}", path),
        })?;
        // Convert from W (kernel) to mW (domain).
        PowerMilliwatts::from_watts(watts)
    }

    fn write_watts(&self, attr: &str, target_mw: PowerMilliwatts) -> Result<(), HpdError> {
        let path = format!("{}/{}/current_value", BASE_PATH, attr);
        // Convert from mW (domain) to W (kernel).
        let watts = target_mw.as_watts();
        self.sysfs.write_string(&path, &watts.to_string())?;
        Ok(())
    }

    /// Like [`Self::write_watts`], but retries once after
    /// [`SETTLE_RETRY_DELAY`] on failure.
    ///
    /// Only for the boost rails (SPPT/FPPT): the EC has occasionally been
    /// observed to reject a mathematically-valid write with `EINVAL` right
    /// after a large SPL jump on the *same* `set_target` call, before the
    /// sustained-rail change has settled. An out-of-range value (rejected
    /// by `validate_power_envelope` before this backend is ever called)
    /// fails deterministically and a retry does not change that — this
    /// only papers over the EC's transient timing, not a logic error.
    fn write_watts_with_settle_retry(
        &self,
        attr: &str,
        target_mw: PowerMilliwatts,
    ) -> Result<(), HpdError> {
        match self.write_watts(attr, target_mw) {
            Ok(()) => Ok(()),
            Err(first_err) => {
                tracing::warn!(
                    attr,
                    error = %first_err,
                    "transient sysfs write failure, retrying once after settle delay"
                );
                thread::sleep(SETTLE_RETRY_DELAY);
                self.write_watts(attr, target_mw)
            }
        }
    }
}

impl<S: SysfsIo> PowerEnvelope for AsusPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        let spl_min = self.read_watts(ATTR_SPL, "min_value")?;
        let spl_max = self.read_watts(ATTR_SPL, "max_value")?;

        // Fallbacks for hardware that doesn't expose the min/max attribute.
        let sppt_min = self
            .read_watts(ATTR_SPPT, "min_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_SPPT_MIN_MW));
        let sppt_max = self
            .read_watts(ATTR_SPPT, "max_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_SPPT_MAX_MW));
        let fppt_min = self
            .read_watts(ATTR_FPPT, "min_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_FPPT_MIN_MW));
        let fppt_max = self
            .read_watts(ATTR_FPPT, "max_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_FPPT_MAX_MW));

        Ok(PowerEnvelopeLimits {
            spl_min,
            spl_max,
            sppt_min,
            sppt_max,
            fppt_min,
            fppt_max,
        })
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        let spl = self.read_watts(ATTR_SPL, "current_value")?;
        let sppt = self.read_watts(ATTR_SPPT, "current_value")?;
        let fppt = self.read_watts(ATTR_FPPT, "current_value")?;

        Ok(PowerEnvelopeTarget {
            spl,
            sppt,
            fppt: Some(fppt),
        })
    }

    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        self.write_watts(ATTR_SPL, target.spl)?;
        self.write_watts_with_settle_retry(ATTR_SPPT, target.sppt)?;

        if let Some(fppt) = target.fppt {
            self.write_watts_with_settle_retry(ATTR_FPPT, fppt)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::units::PowerMilliwatts;
    use hpd_error::SysfsError;
    use hpd_sysfs::MockSysfs; // Simulator based on TempDir
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Wraps a [`MockSysfs`] and fails the first `remaining_failures`
    /// writes whose path contains `fail_path_substr` with an `EINVAL`-style
    /// `SysfsError::Io`, then delegates to the real mock. Simulates the
    /// on-device transient EC rejection `write_watts_with_settle_retry`
    /// exists to recover from.
    struct FlakySysfs {
        inner: MockSysfs,
        fail_path_substr: &'static str,
        remaining_failures: AtomicUsize,
    }

    impl FlakySysfs {
        fn new(
            inner: MockSysfs,
            fail_path_substr: &'static str,
            remaining_failures: usize,
        ) -> Self {
            Self {
                inner,
                fail_path_substr,
                remaining_failures: AtomicUsize::new(remaining_failures),
            }
        }
    }

    impl SysfsIo for FlakySysfs {
        fn read_string(&self, path: impl AsRef<Path>) -> Result<String, SysfsError> {
            self.inner.read_string(path)
        }

        fn write_string(&self, path: impl AsRef<Path>, val: &str) -> Result<(), SysfsError> {
            let p = path.as_ref();
            if p.to_string_lossy().contains(self.fail_path_substr)
                && self
                    .remaining_failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                        if n > 0 {
                            Some(n - 1)
                        } else {
                            None
                        }
                    })
                    .is_ok()
            {
                return Err(SysfsError::Io {
                    path: p.to_path_buf(),
                    source: std::io::Error::from_raw_os_error(22), // EINVAL
                });
            }
            self.inner.write_string(path, val)
        }

        fn exists(&self, path: impl AsRef<Path>) -> bool {
            self.inner.exists(path)
        }

        fn read_dir_names(&self, path: impl AsRef<Path>) -> Vec<String> {
            self.inner.read_dir_names(path)
        }
    }

    #[test]
    fn test_asus_power_translation_mw_to_watts() {
        // 1. Arrange: Prepare system with fake files
        let mock = MockSysfs::new();

        // MockSysfs strips the leading '/' when handling absolute paths.
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value",
            "7",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value",
            "35",
        );
        // Canonical max attributes for the boost rails.
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/max_value",
            "43",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/max_value",
            "55",
        );

        let backend = AsusPowerBackend::new(mock.clone());

        // 2. Act & Assert (Read): Check that "15" on disk is read as 15000mW
        let target = backend.get_target().expect("Must be able to read target");
        assert_eq!(target.spl, PowerMilliwatts(15000));
        assert_eq!(target.sppt, PowerMilliwatts(15000));

        let limits = backend
            .get_limits()
            .expect("Must be able to read the limits");
        assert_eq!(limits.spl_min, PowerMilliwatts(7000));
        assert_eq!(limits.spl_max, PowerMilliwatts(35000));
        assert_eq!(limits.sppt_max, PowerMilliwatts(43000));
        // Regression for the ppt_fppt vs ppt_pl3_fppt bug (Audit §3.2 / Lote 4).
        // If get_limits reads the wrong attribute it falls back silently to
        // ASUS_DEFAULT_FPPT_MAX_MW (53000), so this `55_000` assertion is
        // what proves the canonical attribute is being read.
        assert_eq!(limits.fppt_max, PowerMilliwatts(55000));
        // No `min_value` seeded for SPPT/FPPT above — falls back to the
        // documented ASUS defaults (13W/19W), not to 0 or to spl_min.
        assert_eq!(limits.sppt_min, PowerMilliwatts(13000));
        assert_eq!(limits.fppt_min, PowerMilliwatts(19000));

        // 3. Act & Assert (Write): write 25000mW and check that disk stored "25".
        let new_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(20000),
            sppt: PowerMilliwatts(25000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        backend
            .set_target(&new_target)
            .expect("Must be able to write the target");

        // Use the mock to spy what was written in file
        let spl_written = mock
            .read_string(
                "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            )
            .unwrap();
        let sppt_written = mock
            .read_string(
                "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            )
            .unwrap();

        assert_eq!(spl_written, "20", "20000mW must translate to string '20'");
        assert_eq!(sppt_written, "25", "25000mW must translate to string '25'");
    }

    #[test]
    fn get_limits_reads_sppt_fppt_min_value_when_present() {
        // Regression found on-device (2026-07-12) on the ROG Xbox Ally X
        // (RC73XA): `ppt_pl2_sppt`/`ppt_pl3_fppt` report a `min_value`
        // *above* `ppt_pl1_spl`'s — a derived envelope that only floors at
        // SPL, not at these, can undershoot the real hardware minimum and
        // get rejected with `EINVAL` on write. `get_limits` must surface
        // the real values instead of silently assuming they track `spl_min`.
        let mock = MockSysfs::new();
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value",
            "7",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value",
            "35",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/min_value",
            "13",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/max_value",
            "45",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/min_value",
            "19",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/max_value",
            "55",
        );

        let backend = AsusPowerBackend::new(mock);
        let limits = backend.get_limits().expect("must read limits");

        assert_eq!(limits.spl_min, PowerMilliwatts(7_000));
        assert_eq!(limits.sppt_min, PowerMilliwatts(13_000));
        assert_eq!(limits.sppt_max, PowerMilliwatts(45_000));
        assert_eq!(limits.fppt_min, PowerMilliwatts(19_000));
        assert_eq!(limits.fppt_max, PowerMilliwatts(55_000));
    }

    #[test]
    fn set_target_recovers_from_a_single_transient_sppt_write_failure() {
        // Regression for the on-device (2026-07-12) AC-lock finding: SPPT
        // can transiently EINVAL right after a large SPL jump, then succeed
        // cleanly on an immediate retry. `set_target` must not surface that
        // first failure to the caller.
        let mock = MockSysfs::new();
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            "12",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            "13",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value",
            "19",
        );

        let flaky = FlakySysfs::new(mock.clone(), "ppt_pl2_sppt", 1);
        let backend = AsusPowerBackend::new(flaky);

        let target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_000),
            fppt: Some(PowerMilliwatts(43_000)),
        };

        backend
            .set_target(&target)
            .expect("a single transient SPPT failure must be recovered by the retry");

        let sppt_written = mock
            .read_string(
                "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            )
            .unwrap();
        assert_eq!(sppt_written, "40");
    }

    #[test]
    fn set_target_propagates_error_when_sppt_write_fails_twice() {
        // The retry is a single attempt, not an unbounded loop: a
        // persistently failing write (e.g. a genuinely out-of-range value
        // that slipped past `validate_power_envelope`, or real hardware
        // fault) must still surface as an error rather than being silently
        // swallowed.
        let mock = MockSysfs::new();
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            "12",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            "13",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value",
            "19",
        );

        let flaky = FlakySysfs::new(mock, "ppt_pl2_sppt", 2);
        let backend = AsusPowerBackend::new(flaky);

        let target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_000),
            fppt: Some(PowerMilliwatts(43_000)),
        };

        let result = backend.set_target(&target);
        assert!(
            result.is_err(),
            "a second consecutive failure must propagate"
        );
    }
}
