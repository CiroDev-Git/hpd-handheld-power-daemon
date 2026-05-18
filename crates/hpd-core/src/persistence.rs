use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, error};
use crate::state::ProfileState;

pub struct StatePersister {
    path: PathBuf,
}

impl StatePersister {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub async fn load(&self) -> Option<ProfileState> {
        if !self.path.exists() { // path.exists() es rápido, pero fs::metadata sería mejor en async estricto
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