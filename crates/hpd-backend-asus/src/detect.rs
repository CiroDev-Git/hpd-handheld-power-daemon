use hpd_capabilities::probe::DmiInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum AsusModel {
    RogAlly,      // RC71L
    RogAllyX,     // RC72L
    RogXboxAllyX, // RC73XA
}

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
