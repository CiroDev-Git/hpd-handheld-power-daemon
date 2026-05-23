pub mod io;
pub mod real;

#[cfg(feature = "mock")]
pub mod mock;

pub use hpd_error::SysfsError;
pub use io::SysfsIo;
pub use real::RealSysfs;

#[cfg(feature = "mock")]
pub use mock::testing::MockSysfs;