// SPDX-License-Identifier: GPL-3.0-or-later

//! `hpdctl` — user-facing CLI for the Handheld Power Daemon.
//!
//! Thin client that talks to the running `hpd-daemon` over the
//! `dev.cirodev.hpd.PowerDaemon1` D-Bus interface. Binds to the
//! **system bus** in production and to the **session bus** when the
//! `HPD_SIMULATOR` env var is set (matching the daemon's behaviour
//! under `--features simulator`).
//!
//! See `hpdctl --help` for the subcommand surface; the public CLI is
//! considered stable under SemVer from `1.0.0` onward.

mod dbus;

use clap::{Parser, Subcommand};
use dbus::PowerDaemonProxy;
use std::process;

/// Handheld Power Daemon CLI (hpdctl)
#[derive(Parser)]
#[command(author, version, about = "Control your handheld's power, fan, and battery settings.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Handle Thermal Design Power (TDP)
    Tdp {
        #[command(subcommand)]
        action: TdpAction,
    },
    /// Handle charge limit of battery
    Charge {
        #[command(subcommand)]
        action: ChargeAction,
    },
    /// Apply a TDP preset (eco / balanced / max).
    ///
    /// `TdpPreset` selects a target SPL wattage; it is NOT the same as
    /// the ACPI platform/cooling profile. With auto-cooling enabled the
    /// platform profile follows the chosen TDP automatically.
    Preset {
        #[arg(help = "TDP preset: eco (min SPL), balanced (midpoint), max (max SPL)")]
        name: String,
    },
    /// Show the device system limits
    Limits,
    /// Show the current system status
    Status,
    /// Open real time panel with refresh each second
    Monitor,
    /// Manual control of fans & profiles
    Fan {
        #[clap(subcommand)]
        action: FanAction,
    },
}

#[derive(Subcommand)]
enum TdpAction {
    /// Set power limits in Watts
    Set {
        #[arg(help = "Value in Watts (e.g., 15)")]
        watts: u32,
    },
    /// Get current TDP
    Get,
}

#[derive(Subcommand)]
enum ChargeAction {
    /// Set the max charge limit
    Set {
        #[arg(help = "Use a value between 20 and 100")]
        limit: u8,
    },
    /// Get the current charge limit
    Get,
}

#[derive(Subcommand)]
enum FanAction {
    /// Set a manual profile (Quiet, Balanced, Performance)
    Set { profile: String },
    /// Reset fan control to Daemon (based on TDP)
    Auto,
}

#[tokio::main]
async fn main() {
    // Parsing args from terminal
    let cli = Cli::parse();

    // System bus in production; session bus only when HPD_SIMULATOR
    // is set (matches the daemon, which only binds to the session bus
    // when itself built with the `simulator` feature).
    let connection_result = if std::env::var("HPD_SIMULATOR").is_ok() {
        zbus::Connection::session().await
    } else {
        zbus::Connection::system().await
    };

    let connection = match connection_result {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!(
                "Fatal error: Cannot connect to D-Bus. Is the D-Bus system running?\nDetail: {}",
                e
            );
            process::exit(1);
        }
    };

    // Proxy instance to communicate with daemon
    let proxy = match PowerDaemonProxy::new(&connection).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Fatal error: Daemon hpd not found. Is it running (systemctl status hpd)?\nDetail: {}", e);
            process::exit(1);
        }
    };

    // Command dispatch
    if let Err(e) = execute_command(cli, proxy).await {
        eprintln!("Error executing command: {}", e);
        process::exit(1);
    }
}

async fn execute_command(cli: Cli, proxy: PowerDaemonProxy<'_>) -> zbus::Result<()> {
    match cli.command {
        Commands::Tdp { action } => match action {
            TdpAction::Set { watts } => {
                println!("Requesting TDP change to {}W...", watts);
                proxy.set_spl(watts).await?;
                println!("✅ TDP successfully changed.");
            }
            TdpAction::Get => {
                let watts = proxy.current_spl().await?;
                println!("Current TDP (SPL): {}W", watts);
            }
        },
        Commands::Charge { action } => match action {
            ChargeAction::Set { limit } => {
                println!("Changing battery limit to {}%...", limit);
                proxy.set_charge_threshold(limit).await?;
                println!("✅ Battery limit successfully changed.");
            }
            ChargeAction::Get => {
                let limit = proxy.charge_end_threshold().await?;
                println!("Current battery limit: {}%", limit);
            }
        },
        Commands::Preset { name } => {
            println!(
                "🚀 Requesting profile change to '{}'...",
                name.to_uppercase()
            );
            if let Err(e) = proxy.set_preset(&name).await {
                eprintln!("❌ Error applying preset: {}", e);
            } else {
                println!("✅ Preset applied successfully.");
                println!("(Cooling profile has changed automatically).");
            }
        }
        Commands::Limits => {
            let (spl_min, spl_max, sppt_max, fppt_max) = proxy.get_hardware_limits().await?;
            println!("📊 Detected hardware limits:");
            println!("  • SPL Min (Base):    {}W", spl_min);
            println!("  • SPL Max (Base):    {}W", spl_max);
            println!("  • SPPT Max (Boost):  {}W", sppt_max);
            println!("  • FPPT Max (Peak):   {}W", fppt_max);
        }
        Commands::Status => {
            print_dashboard(&proxy).await?;
        }
        Commands::Monitor => {
            println!("Starting real time monitor. Ctrl+C to exit...");
            loop {
                print!("\x1B[2J\x1B[1;1H");

                print_dashboard(&proxy).await?;

                // Sleep 1 second
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Commands::Fan { action } => match action {
            FanAction::Set { profile } => {
                proxy.set_profile(&profile).await?;
                println!("❄️ Fan profile manually changed to: {}", profile);
            }
            FanAction::Auto => {
                // Notify the daemon to re-enable auto mode
                proxy.set_fan_auto().await?;
                println!("🔄 Automatic fan control enabled (based on TDP).");
            }
        },
    }
    Ok(())
}

async fn print_dashboard(proxy: &PowerDaemonProxy<'_>) -> zbus::Result<()> {
    let spl_watts = proxy.current_spl().await?;
    let profile = proxy.active_profile().await?;
    let charge_limit = proxy.charge_end_threshold().await?;
    let auto_cooling = proxy.auto_cooling().await?;

    let is_ac = proxy.is_ac_connected().await?;
    let power_icon = if is_ac {
        "⚡ Connected (AC)"
    } else {
        "🔋 Battery (DC)"
    };
    let cooling_mode = if auto_cooling {
        "auto (follows TDP)"
    } else {
        "manual"
    };

    println!("=======================================");
    println!("  🎮 Handheld Power Daemon Status 🎮  ");
    println!("=======================================");
    println!("   ⚡ TDP (SPL):        {}W", spl_watts);
    println!("  ❄️ Cooling Profile:  {}", profile);
    println!("  🔁 Cooling Mode:     {}", cooling_mode);
    println!("  🔌 Power adapter:    {}", power_icon);
    println!("  🔋 Battery Limit:    {}%", charge_limit);
    println!("=======================================");

    Ok(())
}
