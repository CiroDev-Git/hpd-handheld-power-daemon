// SPDX-License-Identifier: GPL-3.0-or-later

//! Atomic on-disk persistence for [`ProfileState`].

use crate::state::ProfileState;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, error};

/// Owns the path to the TOML state file and handles atomic writes
/// (temp file + rename).
pub struct StatePersister {
    path: PathBuf,
}

impl StatePersister {
    /// Construct a persister bound to `path`. The file is created on
    /// the first successful `save`; missing/unreadable files result in
    /// `None` from `load` rather than an error.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Reads the persisted state from disk. Returns `None` when the
    /// file does not exist, cannot be read, or fails to parse — the
    /// daemon falls back to defaults rather than refusing to start.
    pub async fn load(&self) -> Option<ProfileState> {
        if !self.path.exists() {
            // path.exists() is sync; tokio::fs::try_exists is the async equivalent
            return None;
        }

        match fs::read_to_string(&self.path).await {
            Ok(content) => match toml::from_str(&content) {
                Ok(state) => Some(state),
                Err(e) => {
                    error!("Failed to parse state file: {}", e);
                    None
                }
            },
            Err(e) => {
                error!("Failed to read state file: {}", e);
                None
            }
        }
    }

    /// Writes `state` to disk via temp-file + rename for atomicity.
    /// Errors are logged but never propagated: a persistence failure
    /// must not bring the daemon down.
    pub async fn save(&self, state: &ProfileState) {
        let content = match toml::to_string_pretty(state) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to serialize state: {}", e);
                return;
            }
        };

        let tmp_path = self.path.with_extension("tmp");
        if let Err(e) = fs::write(&tmp_path, content).await {
            error!("Failed to write temporary state file: {}", e);
            return;
        }

        if let Err(e) = fs::rename(&tmp_path, &self.path).await {
            error!("Failed to commit state file: {}", e);
        } else {
            debug!("State persisted successfully to {}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::power::PowerEnvelopeTarget;
    use hpd_capabilities::profile::ProfileName;
    use hpd_capabilities::units::PowerMilliwatts;

    /// Per-process-unique temp path so concurrent test threads
    /// don't collide. Cleared at test entry in case a previous run
    /// crashed mid-write and left junk behind.
    fn fresh_temp_path(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("hpd_persist_{}_{}.toml", label, std::process::id()));
        let _ = std::fs::remove_file(&path);
        // The save() helper also writes a `.tmp` neighbour during
        // the atomic-rename; clear that too so a partial run can
        // not leak across tests.
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        path
    }

    fn sample_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(20_000),
                sppt: PowerMilliwatts(23_000),
                fppt: Some(PowerMilliwatts(25_000)),
            },
            active_profile: ProfileName::Performance,
            is_ac_connected: true,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_state: None,
            active_fan_curve: None,
            ac_locked: false,
        }
    }

    #[tokio::test]
    async fn save_then_load_roundtrips_persisted_fields() {
        // Field-by-field round trip. `is_ac_connected` is
        // `#[serde(skip)]` on `ProfileState` (boot re-queries it
        // from hardware), so the disk → memory round trip drops
        // that field to its Default::default() of `false` — that
        // is the *intended* contract and is asserted explicitly so
        // a future change to the skip annotation surfaces here.
        let path = fresh_temp_path("roundtrip");
        let persister = StatePersister::new(&path);
        let saved = sample_state();
        persister.save(&saved).await;

        let loaded = StatePersister::new(&path)
            .load()
            .await
            .expect("save then load must yield Some");

        assert_eq!(loaded.power_target, saved.power_target);
        assert_eq!(loaded.active_profile, saved.active_profile);
        assert_eq!(loaded.charge_end_threshold, saved.charge_end_threshold);
        assert_eq!(loaded.fan_follows_tdp, saved.fan_follows_tdp);
        assert_eq!(loaded.last_dc_state, saved.last_dc_state);
        assert!(
            !loaded.is_ac_connected,
            "is_ac_connected is #[serde(skip)]; must reset to false on load"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn load_missing_file_returns_none_without_error() {
        let path = fresh_temp_path("missing");
        assert!(!path.exists(), "fixture must not pre-exist");
        let persister = StatePersister::new(&path);
        assert!(persister.load().await.is_none());
    }

    #[tokio::test]
    async fn load_corrupt_toml_returns_none_without_panic() {
        // Survival invariant: a corrupt state file (e.g. a partial
        // write from a previous crashed boot) must not bring the
        // daemon down. `load` swallows the parse error and returns
        // None; the daemon then re-seeds state from hardware.
        let path = fresh_temp_path("corrupt");
        std::fs::write(&path, b"this is not TOML at all !!! [unclosed").expect("temp write");
        let persister = StatePersister::new(&path);
        assert!(persister.load().await.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn second_save_overwrites_first_via_atomic_rename() {
        // Two successive `save` calls; the second value must be
        // what `load` returns. Indirectly exercises the temp-file +
        // rename path — if a future regression were to drop the
        // rename atomicity, this test would not catch it directly,
        // but at least the overwrite contract is locked.
        let path = fresh_temp_path("overwrite");
        let persister = StatePersister::new(&path);
        let mut state = sample_state();
        persister.save(&state).await;

        state.charge_end_threshold = 50;
        state.active_profile = ProfileName::PowerSaver;
        persister.save(&state).await;

        let loaded = StatePersister::new(&path).load().await.expect("must load");
        assert_eq!(loaded.charge_end_threshold, 50);
        assert_eq!(loaded.active_profile, ProfileName::PowerSaver);

        let _ = std::fs::remove_file(&path);
    }
}
