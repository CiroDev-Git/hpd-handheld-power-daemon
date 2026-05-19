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
        Commands::Status => {
            let spl_watts = proxy.current_spl().await?;
            println!("--- Handheld Power Daemon (Status) ---");
            println!("⚡ TDP (SPL):\t{}W", spl_watts);
            println!("---------------------------------------");
        }
    }
    Ok(())
}