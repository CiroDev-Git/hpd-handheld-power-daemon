/// Represents power in miliwatts to guarantes don't mix W with mW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PowerMilliwatts(pub u32);