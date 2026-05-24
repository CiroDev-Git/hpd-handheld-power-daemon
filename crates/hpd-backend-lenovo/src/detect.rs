// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::probe::DmiInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum LenovoModel {
    LegionGo,  // 83E1
    LegionGoS, // In case of newest version
}

pub fn matches_lenovo_handheld(dmi: &DmiInfo) -> Option<LenovoModel> {
    if !dmi.board_vendor.eq_ignore_ascii_case("LENOVO") {
        return None;
    }

    if dmi.product_name.contains("83E1") || dmi.board_name.contains("83E1") {
        return Some(LenovoModel::LegionGo);
    }
    None
}
