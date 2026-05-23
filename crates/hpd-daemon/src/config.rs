// SPDX-License-Identifier: GPL-3.0-or-later
//! On-disk daemon configuration.
//!
//! Schema is intentionally minimal — `serde + toml`, no `figment`, no
//! filesystem watcher. A missing file falls back to defaults; a corrupt
//! file logs a warning and falls back to defaults. The daemon must
//! **never** die because of a bad config.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use hpd_capabilities::charge::DEFAULT_CHARGE_THRESHOLD;
use hpd_capabilities::profile::{ProfileThresholds, RuntimeConfig};
use hpd_core::executor::TRANSITION_CHANNEL_CAPACITY;

/// Daemon runtime configuration.
///
/// `#[serde(default)]` at both struct and (transitively) field level
/// means a missing field falls back to its `Default` value. That keeps
/// old TOMLs valid as the schema grows.
///
/// Field groups:
/// * `state_path`, `channel_capacity` — startup-only; logged with a
///   warning if changed via `Transition::ConfigReload` since they can
///   only take effect at the next daemon restart.
/// * `default_charge_threshold` — seeds the very first persisted state
///   when the backend can't report the current value. Reload is a no-op.
/// * everything wrapped in `RuntimeConfig` — fully hot-swappable.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub state_path: PathBuf,
    pub channel_capacity: usize,
    pub default_charge_threshold: u8,
    pub profile_thresholds: ProfileThresholds,
    pub sppt_factor: f32,
    pub fppt_factor: f32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            state_path: PathBuf::from("/var/lib/hpd/state.toml"),
            channel_capacity: TRANSITION_CHANNEL_CAPACITY,
            default_charge_threshold: DEFAULT_CHARGE_THRESHOLD,
            profile_thresholds: ProfileThresholds::DEFAULT,
            sppt_factor: RuntimeConfig::DEFAULT.sppt_factor,
            fppt_factor: RuntimeConfig::DEFAULT.fppt_factor,
        }
    }
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
    /// `Transition::ConfigReload`. Daemon-only fields (paths, channel
    /// sizing, startup-only defaults) stay behind.
    pub fn to_runtime(&self) -> RuntimeConfig {
        RuntimeConfig {
            profile_thresholds: self.profile_thresholds.clone(),
            sppt_factor: self.sppt_factor,
            fppt_factor: self.fppt_factor,
        }
    }
}
