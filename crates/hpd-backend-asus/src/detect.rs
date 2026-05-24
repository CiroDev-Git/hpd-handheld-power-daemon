// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::probe::DmiInfo;

/// Variants of the ASUS handheld lineup this backend supports.
///
/// Comments next to each variant list the DMI `board_name` string the
/// kernel exposes for that SKU.
#[derive(Debug, Clone, PartialEq)]
pub enum AsusModel {
    /// ROG Ally (board `RC71L`).
    RogAlly,
    /// ROG Ally X (boards `RC72L` / `RC72LA`).
    RogAllyX,
    /// Xbox-edition ROG Ally X (board `RC73XA`).
    RogXboxAllyX,
}

/// Match a [`DmiInfo`] against the supported ASUS handhelds.
///
/// Returns `Some(model)` only when both the vendor and board-name
/// strings indicate one of the SKUs this backend is known to drive
/// safely. The caller (the daemon's composition root) treats `None`
/// as "no ASUS backend should run on this hardware".
pub fn matches_asus_handheld(dmi: &DmiInfo) -> Option<AsusModel> {
    if !dmi
        .board_vendor
        .eq_ignore_ascii_case("ASUSTeK COMPUTER INC.")
    {
        return None;
    }

    match dmi.board_name.trim() {
        "RC71L" => Some(AsusModel::RogAlly),
        "RC72L" | "RC72LA" => Some(AsusModel::RogAllyX),
        "RC73XA" => Some(AsusModel::RogXboxAllyX),
        _ => None,
    }
}
