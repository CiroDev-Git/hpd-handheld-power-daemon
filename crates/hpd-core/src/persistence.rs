use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, error};
use crate::state::ProfileState;

pub struct StatePersister {
    path: PathBuf,
}

impl StatePersister {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Option<ProfileState> {
        if !self.path.exists() {
            return None;
        }

        match fs::read_to_string(&self.path) {
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

    pub fn save(&self, state: &ProfileState) {
        let content = match toml::to_string_pretty(state) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to serialize state: {}", e);
                return;
            }
        };

        // Atomic write (Making a copy in case of device turned off while saving, keeps the original file safe and not modified)
        let tmp_path = self.path.with_extension("tmp");
        if let Err(e) = fs::write(&tmp_path, content) {
            error!("Failed to write temporary state file: {}", e);
            return;
        }

        if let Err(e) = fs::rename(&tmp_path, &self.path) {
            error!("Failed to commit state file: {}", e);
        } else {
            debug!("State persisted successfully to {}", self.path.display());
        }
    }
}