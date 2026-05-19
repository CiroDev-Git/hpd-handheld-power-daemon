use hpd_capabilities::probe::DmiInfo;
use std::fs;

pub fn read_system_dmi() -> DmiInfo {
    let vendor = fs::read_to_string("/sys/class/dmi/id/board_vendor")
        .unwrap_or_else(|_| "Unknown".to_string())
        .trim()
        .to_string();
        
    let board = fs::read_to_string("/sys/class/dmi/id/board_name")
        .unwrap_or_else(|_| "Unknown".to_string())
        .trim()
        .to_string();
        
    let product = fs::read_to_string("/sys/class/dmi/id/product_name")
        .unwrap_or_else(|_| "Unknown".to_string())
        .trim()
        .to_string();

    DmiInfo {
        board_vendor: vendor,
        board_name: board,
        product_name: product,
    }
}