// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::probe::DmiInfo;
use std::env;
use std::fs;

pub fn read_system_dmi() -> DmiInfo {
    // Simulator mode for macOS/Development
    if env::var("HPD_SIMULATOR").is_ok() {
        return DmiInfo {
            board_vendor: "ASUSTeK COMPUTER INC.".to_string(),
            board_name: "RC73XA".to_string(),
            product_name: "ROG Ally X (Simulator)".to_string(),
        };
    }

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
