// SPDX-License-Identifier: GPL-3.0-or-later

//! ACPI platform / cooling profile capability.

use crate::error::HpdError;
use crate::profile::ProfileName;

/// Read and write the active ACPI platform profile
/// (`/sys/firmware/acpi/platform_profile`).
pub trait PlatformProfile: Send + Sync {
    /// Returns the currently active platform profile.
    fn get_active_profile(&self) -> Result<ProfileName, HpdError>;
    /// Switches the active platform profile. Backends must reject
    /// `ProfileName::Custom` variants the kernel does not advertise.
    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError>;
}
