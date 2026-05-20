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
    /// Show the current system status
    Status,
}

#[derive(Subcommand)]
enum TdpAction {
    /// Set power limits in Watts
    Set {
        #[arg(help = "Value in Watts (ej. 15)")]
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

#[tokio::main]
async fn main() {
    // Parsing args from terminal
    let cli = Cli::parse();

    // Trying to connect to Session Bus (the same one that daeomon use currently)
    let connection = match zbus::Connection::session().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Fatal error: Cannot connect to D-Bus. ¿Is the D-Bus system running?\nDetail: {}", e);
            process::exit(1);
        }
    };

    // Proxy instance to communicate with daemon
    let proxy = match PowerDaemonProxy::new(&connection).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Fatal error: Daemon hpd not found. ¿Is it running (systemctl status hpd)?\nDetail: {}", e);
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
        Commands::Status => {
            let spl_watts = proxy.current_spl().await?;
            let profile = proxy.active_profile().await?;
            let charge_limit = proxy.charge_end_threshold().await?;

            println!("=======================================");
            println!("  🎮 Handheld Power Daemon Status 🎮  ");
            println!("=======================================");
            println!("  ⚡ TDP (SPL):         {} W", spl_watts);
            println!(" ❄️ Cooling Profile:   {}", profile);
            println!(" 🔋 Battery Limit:     {} %", charge_limit);
            println!("=======================================");
        }
    }
    Ok(())
}