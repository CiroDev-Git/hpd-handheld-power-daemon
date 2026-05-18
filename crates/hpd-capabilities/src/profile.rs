use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileName {
    PowerSaver,
    Balanced,
    Performance,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileThresholds {
    pub low_frac: f32,
    pub high_frac: f32,
}