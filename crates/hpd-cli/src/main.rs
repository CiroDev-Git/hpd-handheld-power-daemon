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
#[command(
    name = "hpdctl",
    author,
    version,
    about = "Control your handheld's power, fan, and battery settings.",
    long_about = "hpdctl is the command-line client for hpd, the Handheld Power Daemon.\n\
        \n\
        It talks to the running hpd-daemon over D-Bus to manage four things:\n\
        \n  \
        • TDP / power envelope  — how many watts the APU is allowed to draw\n  \
        • Cooling profile       — the ACPI platform profile (quiet/balanced/performance)\n  \
        • Fan control           — automatic (follows TDP) or a manual profile\n  \
        • Battery charge limit  — cap charging to extend battery lifespan\n\
        \n\
        Reading status (status, monitor, limits, *-get) never needs root.\n\
        Changing settings needs no sudo if you are in the 'wheel' (admin)\n\
        group — including over SSH; other users are prompted to authenticate.",
    after_help = "EXAMPLES:\n  \
        hpdctl status                 Show a one-shot status dashboard\n  \
        hpdctl monitor                Live dashboard, refreshes every second\n  \
        hpdctl limits                 Show the hardware's TDP min/max\n  \
        hpdctl tdp set 15             Set the power envelope to 15 W\n  \
        hpdctl preset eco             Apply the lowest-power preset\n  \
        hpdctl charge set 80          Stop charging the battery at 80%\n  \
        hpdctl fan set performance    Force the performance cooling profile\n  \
        hpdctl fan auto               Let the daemon pick cooling from TDP\n  \
        hpdctl fan curve set aggressive  Apply an aggressive custom fan curve\n  \
        hpdctl fan curve reset        Return fan control to firmware auto\n\
        \n\
        Run `hpdctl <command> --help` for details on any command."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set or read the TDP / power envelope (watts)
    ///
    /// The TDP (Thermal Design Power) is how much power the APU may draw.
    /// Higher = more performance and heat; lower = cooler and longer
    /// battery. `tdp set` adjusts the sustained limit (SPL); the daemon
    /// derives the boost limits (SPPT/FPPT) from it automatically.
    ///
    /// With auto-cooling on, changing the TDP also moves the cooling
    /// profile to match. Use `hpdctl limits` to see the valid range.
    Tdp {
        #[command(subcommand)]
        action: TdpAction,
    },
    /// Set or read the battery charge limit (%)
    ///
    /// Caps how full the battery is allowed to charge. Holding a handheld
    /// at 80% instead of 100% noticeably slows long-term battery wear,
    /// which is useful when the device is docked / on AC most of the time.
    Charge {
        #[command(subcommand)]
        action: ChargeAction,
    },
    /// Apply a TDP preset: eco, balanced, or max
    ///
    /// A preset is a shortcut that picks a target SPL wattage for you:
    /// `eco` = minimum SPL, `balanced` = midpoint, `max` = maximum SPL.
    ///
    /// This is NOT the same as the ACPI cooling profile (see `hpdctl fan`).
    /// With auto-cooling enabled the cooling profile follows the preset's
    /// TDP automatically.
    Preset {
        #[arg(help = "Preset name: eco (min SPL), balanced (midpoint), or max (max SPL)")]
        name: String,
    },
    /// Show the hardware's TDP limits (SPL/SPPT/FPPT)
    ///
    /// Prints the min/max watts the detected hardware accepts. Use these
    /// numbers as the valid range for `hpdctl tdp set`.
    Limits,
    /// Show a one-shot status dashboard
    ///
    /// Prints current TDP, cooling profile, cooling mode (auto/manual),
    /// AC/battery state, and the battery charge limit, then exits.
    Status,
    /// Live status dashboard, refreshes every second
    ///
    /// Same dashboard as `status` but redrawn once per second. Press
    /// Ctrl+C to exit.
    Monitor,
    /// Control the fan / cooling profile (auto or manual)
    ///
    /// The daemon can either pick the cooling profile automatically from
    /// the current TDP (`fan auto`) or hold a profile you choose
    /// (`fan set <profile>`). Setting a manual profile turns auto-cooling
    /// off until you run `fan auto` again.
    Fan {
        #[clap(subcommand)]
        action: FanAction,
    },
}

#[derive(Subcommand)]
enum TdpAction {
    /// Set the sustained power limit, in watts
    ///
    /// The daemon clamps the value to the hardware range (see
    /// `hpdctl limits`) and derives the boost limits from it.
    Set {
        #[arg(help = "Sustained power limit in watts, e.g. 15")]
        watts: u32,
    },
    /// Print the current TDP (SPL) in watts
    Get,
}

#[derive(Subcommand)]
enum ChargeAction {
    /// Set the charge end threshold, as a percentage
    ///
    /// Charging stops once the battery reaches this level. 80 is a good
    /// default for longevity; 100 disables the limit.
    Set {
        #[arg(help = "Charge limit percentage, between 20 and 100")]
        limit: u8,
    },
    /// Print the current charge limit (%)
    Get,
}

#[derive(Subcommand)]
enum FanAction {
    /// Hold a manual cooling profile (quiet, balanced, performance)
    ///
    /// Pins the ACPI platform/cooling profile and disables auto-cooling
    /// until you run `hpdctl fan auto`.
    Set {
        #[arg(help = "Cooling profile: quiet, balanced, or performance")]
        profile: String,
    },
    /// Re-enable automatic cooling (profile follows TDP)
    Auto,
    /// Program a custom fan curve (silent, balanced, aggressive)
    ///
    /// Writes an EC-mediated temperature→speed curve that cools more
    /// aggressively than the conservative firmware default, and is
    /// re-applied automatically after suspend/resume. The curve is
    /// safe: the embedded controller keeps running it even if the
    /// daemon stops.
    Curve {
        #[clap(subcommand)]
        action: FanCurveAction,
    },
}

#[derive(Subcommand)]
enum FanCurveAction {
    /// Apply a named curve preset: silent, balanced, or aggressive
    Set {
        #[arg(help = "Curve preset: silent (quiet), balanced (default), or aggressive (coolest)")]
        preset: String,
    },
    /// Print the active fan curve (preset name, custom, or auto)
    Get,
    /// Hand fan control back to the firmware's automatic curve
    Reset,
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
            FanAction::Curve { action } => match action {
                FanCurveAction::Set { preset } => {
                    if let Err(e) = proxy.set_fan_curve(&preset).await {
                        eprintln!("❌ Error applying fan curve: {}", e);
                    } else {
                        println!("🌀 Fan curve set to: {}", preset);
                    }
                }
                FanCurveAction::Get => {
                    let curve = proxy.fan_curve().await?;
                    println!("🌀 Active fan curve: {}", curve);
                }
                FanCurveAction::Reset => {
                    proxy.reset_fan_curve().await?;
                    println!("🔄 Fan curve reset to firmware automatic.");
                }
            },
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
