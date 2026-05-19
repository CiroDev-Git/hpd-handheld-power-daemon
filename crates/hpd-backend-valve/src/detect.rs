use hpd_capabilities::probe::DmiInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum ValveModel {
    SteamDeckLcd,  // Jupiter
    SteamDeckOled, // Galileo
}

pub fn matches_valve_handheld(dmi: &DmiInfo) -> Option<ValveModel> {
    if !dmi.board_vendor.eq_ignore_ascii_case("Valve") {
        return None;
    }

    match dmi.product_name.as_str() {
        "Jupiter" => Some(ValveModel::SteamDeckLcd),
        "Galileo" => Some(ValveModel::SteamDeckOled),
        _ => None,
    }
}