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
