// SPDX-License-Identifier: GPL-3.0-or-later
//! On-disk daemon configuration.
//!
//! Schema is intentionally minimal — `serde + toml`, no `figment`, no
//! filesystem watcher. A missing file falls back to defaults; a corrupt
//! file logs a warning and falls back to defaults. The daemon must
//! **never** die because of a bad config.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer};
use tracing::{info, warn};

use hpd_capabilities::charge::DEFAULT_CHARGE_THRESHOLD;
use hpd_capabilities::fan_curve::FanCurvePreset;
use hpd_capabilities::profile::{ProfileName, RuntimeConfig};
use hpd_core::executor::TRANSITION_CHANNEL_CAPACITY;

/// Daemon runtime configuration.
///
/// Two field groups:
///
/// * **Startup-only fields** (`state_path`, `channel_capacity`,
///   `default_charge_threshold`) live directly on this struct. A
///   `Transition::ConfigReload` cannot change them in-flight — the
///   daemon logs that a restart is required.
/// * **Runtime-tunable fields** live inside the embedded
///   [`RuntimeConfig`]. `#[serde(flatten)]` keeps the on-disk TOML
///   schema flat (operators still write `sppt_factor = 1.20` at the
///   top level, not `[runtime] sppt_factor = 1.20`) so this refactor
///   is *zero migration* for installed configs.
///
/// `#[serde(default)]` at struct level — and on both `RuntimeConfig`
/// and `ProfileThresholds` underneath — means any subset of fields is
/// valid. Adding new fields never breaks an old config.
///
/// `Serialize` is deliberately **not** derived: nothing in the tree
/// writes a `DaemonConfig` back to disk today. Add it the day a
/// `--dump-config` (or similar) command lands, not before.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Where the daemon persists `ProfileState`. Under systemd this is
    /// normally overridden by the `STATE_DIRECTORY` env var injected
    /// via `StateDirectory=hpd`.
    pub state_path: PathBuf,

    /// Bound on the internal `Transition` mpsc channel. Startup-only.
    pub channel_capacity: usize,

    /// Charge end threshold to seed the very first persisted state on
    /// hosts where no state file exists yet and the backend cannot
    /// report the current value. Startup-only.
    pub default_charge_threshold: u8,

    /// Fan-curve preset to program at startup when no curve is persisted
    /// yet (first boot). `Some(Balanced)` by default so a fresh install
    /// immediately runs a cooler-than-firmware curve; set to `None` to
    /// leave the firmware's automatic curve untouched until the operator
    /// picks one. Startup-only — a persisted `active_fan_curve` in
    /// `state.toml` takes precedence on subsequent boots.
    pub default_fan_curve: Option<FanCurvePreset>,

    /// ACPI `platform_profile` (EPP / SMU power behaviour) to program at
    /// startup. Defaults to [`ProfileName::Performance`] so the SPL the
    /// user sets is the real, usable power ceiling — the platform profile
    /// is a power lever decoupled from cooling and is *not* dragged toward
    /// `PowerSaver` by the TDP any more. Applied on every boot (so it also
    /// migrates a device left in a throttling profile by an older hpd);
    /// override to `balanced` / `power-saver` for an efficiency bias.
    /// Startup-only.
    #[serde(deserialize_with = "de_platform_profile")]
    pub default_platform_profile: ProfileName,

    /// Hot-swappable subset: thresholds + SPPT/FPPT boost multipliers
    /// the reducer reads on every transition. Replaced wholesale on
    /// `Transition::ConfigReload(RuntimeConfig)`.
    ///
    /// `#[serde(flatten)]` exposes its fields at the top level of the
    /// TOML so existing configs do not need to nest them under a
    /// `[runtime]` table.
    #[serde(flatten)]
    pub runtime: RuntimeConfig,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_path: PathBuf::from("/var/lib/hpd/state.toml"),
            channel_capacity: TRANSITION_CHANNEL_CAPACITY,
            default_charge_threshold: DEFAULT_CHARGE_THRESHOLD,
            default_fan_curve: Some(FanCurvePreset::Balanced),
            default_platform_profile: ProfileName::Performance,
            runtime: RuntimeConfig::DEFAULT,
        }
    }
}

/// Deserialize `default_platform_profile` through [`ProfileName`]'s
/// case-insensitive `FromStr` (accepts `performance`, `balanced`,
/// `power-saver`, the ACPI aliases `quiet` / `low-power`, or any vendor
/// string) instead of serde's CamelCase variant-name matching. An empty
/// or otherwise invalid value falls back to `Performance` — a config
/// typo must never be fatal.
fn de_platform_profile<'de, D>(deserializer: D) -> Result<ProfileName, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(ProfileName::from_str(&raw).unwrap_or(ProfileName::Performance))
}

impl DaemonConfig {
    /// Load from `path`. Failure modes:
    /// * missing file → defaults (informational log),
    /// * I/O error    → defaults (warn log),
    /// * parse error  → defaults (warn log).
    ///
    /// Never returns `Err` and never panics: daemon survival outranks
    /// honouring an operator's typo.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str::<Self>(&contents) {
                Ok(cfg) => {
                    info!(path = %path.display(), "Loaded config");
                    cfg
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Config parse error, using defaults");
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "No config file, using defaults");
                Self::default()
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Cannot read config, using defaults");
                Self::default()
            }
        }
    }

    /// Project the hot-swappable subset that travels with
    /// `Transition::ConfigReload`. After Lote 36 this is a thin clone
    /// of the embedded `runtime` field — kept as a method so the
    /// callsites in `main.rs` (and any future ones) read intent
    /// rather than poking the struct internals.
    pub fn to_runtime(&self) -> RuntimeConfig {
        self.runtime.clone()
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::profile::ProfileThresholds;

    /// `#[serde(flatten)]` carries the on-disk schema invariant: the
    /// runtime sub-fields live at the *top* of the TOML document, not
    /// inside a `[runtime]` table. Locks the format so operators'
    /// existing configs keep working across the Lote 36 refactor.
    #[test]
    fn flatten_keeps_runtime_fields_at_top_level() {
        let toml = r#"
state_path = "/tmp/foo.toml"
sppt_factor = 1.25

[profile_thresholds]
low_frac  = 0.40
high_frac = 0.80
"#;
        let cfg: DaemonConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.state_path, PathBuf::from("/tmp/foo.toml"));
        // Runtime sub-fields parsed from flat top-level keys.
        assert_eq!(cfg.runtime.sppt_factor, 1.25);
        assert_eq!(cfg.runtime.profile_thresholds.low_frac, 0.40);
        assert_eq!(cfg.runtime.profile_thresholds.high_frac, 0.80);
        // Missing runtime sub-field took the default.
        assert_eq!(cfg.runtime.fppt_factor, RuntimeConfig::DEFAULT.fppt_factor);
        // Missing startup-only field took the default.
        assert_eq!(
            cfg.channel_capacity,
            DaemonConfig::default().channel_capacity
        );
    }

    /// Survival invariant (REMEDIATION_V1 §D): an empty config must
    /// parse to `Default::default()`. Combined with the flatten test
    /// above this proves the "any subset is valid" property.
    #[test]
    fn empty_toml_parses_to_default() {
        let cfg: DaemonConfig = toml::from_str("").expect("empty TOML parses");
        assert_eq!(cfg, DaemonConfig::default());
    }

    /// Round-trip against the shipped operator template — the file
    /// `install.sh` deploys as `/etc/hpd/config.toml.example`. If a
    /// future edit silently changes the schema, this test breaks. We
    /// embed the file contents at compile time so the test does not
    /// need the file at runtime (CI sandboxing is fine).
    #[test]
    fn shipped_example_template_parses_to_defaults() {
        const EXAMPLE: &str = include_str!("../../../package/hpd-example.toml");
        let cfg =
            toml::from_str::<DaemonConfig>(EXAMPLE).expect("shipped hpd-example.toml must parse");
        assert_eq!(
            cfg,
            DaemonConfig::default(),
            "every field in the example template is commented out, so the parsed config must match Default::default()"
        );
    }

    /// `to_runtime` is the projection that travels with a
    /// `Transition::ConfigReload`. Before Lote 36 it built a new
    /// `RuntimeConfig` field-by-field; after the refactor it is just
    /// a clone of the embedded field. Confirms the simplification did
    /// not change observable behaviour.
    #[test]
    fn to_runtime_clones_embedded_runtime_verbatim() {
        let cfg = DaemonConfig {
            state_path: PathBuf::from("/var/lib/hpd/state.toml"),
            channel_capacity: 64,
            default_charge_threshold: 90,
            default_fan_curve: Some(FanCurvePreset::Balanced),
            default_platform_profile: ProfileName::Performance,
            runtime: RuntimeConfig {
                profile_thresholds: ProfileThresholds {
                    low_frac: 0.25,
                    high_frac: 0.75,
                },
                sppt_factor: 1.10,
                fppt_factor: 1.30,
            },
        };
        assert_eq!(cfg.to_runtime(), cfg.runtime);
    }

    // ---------- file-IO paths through DaemonConfig::load (Lote 41) ----------

    /// Per-process-unique temp path so concurrent tests don't collide.
    fn fresh_temp_path(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hpd_daemon_config_{}_{}.toml",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// Survival invariant (REMEDIATION_V1 §D): a `load` against a
    /// path that does not exist returns `Default::default()` rather
    /// than erroring. The daemon never refuses to start over a
    /// missing config file.
    #[test]
    fn load_missing_file_returns_defaults() {
        let path = fresh_temp_path("missing");
        assert!(!path.exists(), "fixture must not pre-exist");
        let cfg = DaemonConfig::load(&path);
        assert_eq!(cfg, DaemonConfig::default());
    }

    /// Survival invariant: a corrupt config file (bad TOML / partial
    /// write from a crashed editor / operator typo) must not bring
    /// the daemon down. `load` logs a warning and falls back to
    /// defaults so the daemon continues with sensible values.
    #[test]
    fn load_corrupt_toml_returns_defaults_without_panic() {
        let path = fresh_temp_path("corrupt");
        std::fs::write(&path, b"this is not valid TOML !!! [unclosed").expect("temp write");
        let cfg = DaemonConfig::load(&path);
        assert_eq!(cfg, DaemonConfig::default());
        let _ = std::fs::remove_file(&path);
    }

    /// Partial-config tolerance: setting only one runtime field via
    /// the flattened schema must keep all the other fields at their
    /// defaults. Together with the parser-level
    /// `flatten_keeps_runtime_fields_at_top_level` test (which only
    /// proves the TOML *string* parses correctly) this nails down
    /// the same property end-to-end through file IO.
    #[test]
    fn load_partial_toml_keeps_defaults_for_missing_fields() {
        let path = fresh_temp_path("partial");
        std::fs::write(&path, b"sppt_factor = 1.30\n").expect("temp write");
        let cfg = DaemonConfig::load(&path);
        let defaults = DaemonConfig::default();
        assert_eq!(cfg.runtime.sppt_factor, 1.30);
        // Everything else stays default.
        assert_eq!(cfg.runtime.fppt_factor, defaults.runtime.fppt_factor);
        assert_eq!(
            cfg.runtime.profile_thresholds,
            defaults.runtime.profile_thresholds
        );
        assert_eq!(cfg.state_path, defaults.state_path);
        assert_eq!(cfg.channel_capacity, defaults.channel_capacity);
        assert_eq!(
            cfg.default_charge_threshold,
            defaults.default_charge_threshold
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Full positive case: every field set explicitly and every
    /// field read back exactly. Locks the on-disk schema *as a
    /// whole* against silent breakage.
    #[test]
    fn load_full_valid_toml_reflects_every_field() {
        let path = fresh_temp_path("full");
        let body = r#"
state_path = "/tmp/explicit-state.toml"
channel_capacity = 64
default_charge_threshold = 90

sppt_factor = 1.40
fppt_factor = 1.60

[profile_thresholds]
low_frac  = 0.25
high_frac = 0.75
"#;
        std::fs::write(&path, body).expect("temp write");
        let cfg = DaemonConfig::load(&path);
        assert_eq!(cfg.state_path, PathBuf::from("/tmp/explicit-state.toml"));
        assert_eq!(cfg.channel_capacity, 64);
        assert_eq!(cfg.default_charge_threshold, 90);
        assert_eq!(cfg.runtime.sppt_factor, 1.40);
        assert_eq!(cfg.runtime.fppt_factor, 1.60);
        assert_eq!(cfg.runtime.profile_thresholds.low_frac, 0.25);
        assert_eq!(cfg.runtime.profile_thresholds.high_frac, 0.75);
        let _ = std::fs::remove_file(&path);
    }
}
