pub mod backend;
pub mod charge;
pub mod error;
pub mod fan;
pub mod platform_profile;
pub mod power;
pub mod profile;
pub mod units;
pub mod probe;

#[cfg(any(test, feature = "testing"))]
pub mod testing;