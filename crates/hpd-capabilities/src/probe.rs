// SPDX-License-Identifier: GPL-3.0-or-later

//! DMI snapshot used by L1 backends to recognise the running hardware.

/// Subset of `/sys/class/dmi/id/` fields used for vendor / model
/// detection. Populated by `hpd-daemon` at startup and passed to each
/// backend's `matches_*` function.
#[derive(Debug, Clone)]
pub struct DmiInfo {
    /// Contents of `board_vendor` (e.g. `"ASUSTeK COMPUTER INC."`).
    pub board_vendor: String,
    /// Contents of `board_name` (e.g. `"RC73XA"` on Xbox Ally X).
    pub board_name: String,
    /// Contents of `product_name` (marketing name, e.g. `"ROG Ally X"`).
    pub product_name: String,
}
