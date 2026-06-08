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
mod doctor;
mod fix;

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
        • Cooling               — one lever: silent / balanced / aggressive (or auto)\n  \
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
        hpdctl cool set aggressive    Cool harder (fans only)\n  \
        hpdctl power set performance  Power mode (EPP): full TDP\n  \
        hpdctl cool auto              Let the daemon pick cooling from TDP\n  \
        hpdctl cool get               Show current cooling level + mode\n  \
        hpdctl doctor                 Check that hpd is the sole power manager\n  \
        hpdctl doctor --fix           Neutralize competing daemons + install polkit\n\
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
    /// With auto-cooling on, changing the TDP also moves the fan curve to
    /// match (quieter at low TDP, cooler at high). Power and cooling are
    /// decoupled — the SPL you set here is the real limit. Use
    /// `hpdctl limits` to see the valid range.
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
    /// This is NOT the same as the cooling level (see `hpdctl cool`).
    /// With auto-cooling enabled the fan curve follows the preset's TDP
    /// automatically; power is unaffected by cooling.
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
    /// AC/battery state, and the battery charge limit, followed by a
    /// "System health" section (the same checks as `hpdctl doctor`):
    /// whether polkit is installed, whether any competing power daemon
    /// (power-profiles-daemon, steamos-manager) is fighting hpd, whether
    /// GameMode is live, and whether you are in a gamescope session — then
    /// exits. It gives the all-clear when nothing is wrong.
    Status,
    /// Live status dashboard, refreshes every second
    ///
    /// Same dashboard as `status` but redrawn once per second. Press
    /// Ctrl+C to exit.
    Monitor,
    /// Cooling: pick how hard the fans work (independent of power)
    ///
    /// `cool set <level>` sets the **fan curve** only — it does not change
    /// power. Cooling and power are decoupled: `tdp set` is the single
    /// power lever (the SPL you set is the real limit), and `cool` just
    /// trades noise for temperature. `cool auto` lets the daemon pick the
    /// fan curve from the current TDP; `cool reset` hands the fans back to
    /// firmware control. Levels, quietest → coolest: `silent`, `balanced`,
    /// `aggressive`.
    ///
    /// (The ACPI platform profile / EPP is a separate power knob that
    /// defaults to `performance`; it stays available over D-Bus for
    /// advanced use.)
    Cool {
        #[clap(subcommand)]
        action: CoolAction,
    },
    /// Power mode (EPP / platform profile) — the advanced power lever
    ///
    /// Separate from `tdp` (the watt limit) and from `cool` (fans only).
    /// `performance` (the default) lets your TDP apply in full; `balanced`
    /// and `eco` bias efficiency by letting the firmware clamp power below
    /// your TDP. Most users leave this on `performance`.
    Power {
        #[clap(subcommand)]
        action: PowerAction,
    },
    /// Lock to maximum performance while on AC (on / off)
    ///
    /// When ON (the default), plugging in the charger pins Performance /
    /// Max TDP / Aggressive cooling and LOCKS those controls until you
    /// unplug — the battery charge limit stays editable. When OFF, AC is
    /// fully manual (plugging in changes nothing). Run with no argument to
    /// print the current preference + live lock state. The setting persists
    /// across reboots.
    AcLock {
        #[arg(
            value_parser = ["on", "off"],
            help = "on = lock max on AC; off = fully manual; omit to show state"
        )]
        state: Option<String>,
    },
    /// Install the polkit policy so privileged commands work
    ///
    /// Fixes the "Permission denied / AuthFailed" you hit when the daemon
    /// was deployed without its polkit policy (e.g. a hand-copied binary).
    /// Prompts for administrator access (pkexec/sudo), writes the policy +
    /// rules, and reloads polkit. Run once; the daemon does not need a
    /// restart. Needs neither the daemon running nor D-Bus.
    FixPolkit {
        /// Internal: perform the writes (already elevated). Not for manual use.
        #[arg(long, hide = true)]
        apply: bool,
    },
    /// Diagnose and repair hpd's power ownership (polkit + competing daemons)
    ///
    /// `hpdctl doctor` reports whether the polkit policy is installed and
    /// whether a competing power daemon (power-profiles-daemon,
    /// steamos-manager) is running and fighting hpd over TDP / profile /
    /// charge. `hpdctl doctor --fix` neutralizes those daemons (mask) and
    /// installs the polkit policy in one elevated step (pkexec/sudo), so
    /// hpd becomes the sole power manager — a superset of `fix-polkit`.
    /// Read-only without `--fix`.
    Doctor {
        /// Neutralize competing power daemons and install the polkit policy.
        #[arg(long)]
        fix: bool,
        /// Internal: perform the privileged work (already elevated). Not for manual use.
        #[arg(long, hide = true)]
        apply: bool,
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
enum PowerAction {
    /// Set the power mode: performance, balanced, or eco
    ///
    /// Maps to the ACPI platform profile (`eco` = power-saver). This is the
    /// power/EPP lever, independent of cooling. `performance` keeps your TDP
    /// fully usable; `balanced`/`eco` let the firmware clamp power lower.
    Set {
        #[arg(help = "Power mode: performance (full TDP), balanced, or eco (max efficiency)")]
        mode: String,
    },
    /// Print the current power mode
    Get,
}

#[derive(Subcommand)]
enum CoolAction {
    /// Set the cooling level: silent, balanced, or aggressive
    ///
    /// Sets the fan curve only (noise vs temperature) and switches to
    /// manual cooling (until `cool auto`). Does not change power.
    Set {
        #[arg(help = "Cooling level: silent (quietest), balanced, or aggressive (coolest)")]
        level: String,
    },
    /// Let the daemon pick the cooling level from the current TDP
    Auto,
    /// Hand the fans back to the firmware's automatic curve
    Reset,
    /// Show the current cooling level and mode
    Get,
    /// Draw the active fan curve (temperature → fan speed)
    Curve,
}

#[tokio::main]
async fn main() {
    // Parsing args from terminal
    let cli = Cli::parse();

    // `fix-polkit` repairs polkit itself and needs neither the daemon nor
    // D-Bus — handle it before we touch the bus so it works even with hpd
    // stopped or the policy missing.
    if let Commands::FixPolkit { apply } = &cli.command {
        process::exit(fix::run(*apply));
    }

    // `doctor --fix` does privileged systemctl/polkit work and needs no
    // D-Bus — intercept it before the bus setup so it runs even with hpd
    // stopped or the policy missing. The read-only `doctor` report needs
    // the proxy, so it falls through to the dispatch below.
    if let Commands::Doctor { fix: true, apply } = &cli.command {
        process::exit(doctor::run_fix(*apply));
    }

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
        let msg = e.to_string();
        if msg.contains("AuthFailed") || msg.contains("not authorized") {
            eprintln!("Permission denied by polkit.");
            eprintln!(
                "If the polkit policy isn't installed (common with a hand-copied binary), run:"
            );
            eprintln!("    hpdctl fix-polkit");
            eprintln!("Otherwise you may need an admin password, or to be in the `wheel` group.");
        } else {
            eprintln!("Error executing command: {}", e);
        }
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
            print_health_section(&proxy).await;
            offer_polkit_fix_if_broken(&proxy).await;
        }
        Commands::Monitor => {
            println!("Starting real time monitor. Ctrl+C to exit...");
            // Health (polkit / competing daemons) changes rarely and its
            // poll hits polkit's EnumerateActions, so refresh it every ~5
            // frames rather than every second; the telemetry above is what
            // actually moves at 1 Hz.
            let mut health_line = String::new();
            let mut tick: u32 = 0;
            loop {
                print!("\x1B[2J\x1B[1;1H");

                print_dashboard(&proxy).await?;

                if tick % 5 == 0 {
                    health_line = doctor::health_summary(&proxy).await;
                }
                println!("  🩺 Health:           {health_line}");

                tick = tick.wrapping_add(1);
                // Sleep 1 second
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Commands::Cool { action } => match action {
            CoolAction::Set { level } => {
                if let Err(e) = proxy.set_cooling_level(&level).await {
                    eprintln!("❌ Error setting cooling level: {}", e);
                } else {
                    println!("🧊 Cooling level set to: {}", level);
                }
            }
            CoolAction::Auto => {
                proxy.set_fan_auto().await?;
                println!("🔄 Automatic cooling enabled (follows TDP).");
            }
            CoolAction::Reset => {
                proxy.reset_fan_curve().await?;
                println!("🔄 Fans handed back to firmware automatic control.");
            }
            CoolAction::Get => {
                let level = proxy.fan_curve().await?;
                let mode = if proxy.auto_cooling().await? {
                    "auto"
                } else {
                    "manual"
                };
                println!("🧊 Cooling: {} ({})", level, mode);
            }
            CoolAction::Curve => {
                let level = proxy.fan_curve().await?;
                let (cpu, gpu) = proxy.get_fan_curve().await?;
                println!("🌀 Fan curve: {}", level);
                render_curve("CPU fan  (temp → speed)", &cpu);
                render_curve("GPU fan  (temp → speed)", &gpu);
            }
        },
        Commands::Power { action } => match action {
            PowerAction::Set { mode } => {
                // Friendly names → ACPI platform profile. `eco` is the
                // user-facing name for `power-saver`.
                let profile = match mode.to_lowercase().as_str() {
                    "performance" | "perf" => "performance",
                    "balanced" => "balanced",
                    "eco" | "power-saver" | "powersave" | "power_saver" => "power-saver",
                    other => {
                        eprintln!(
                            "❌ Unknown power mode '{}'. Use: performance, balanced, or eco.",
                            other
                        );
                        process::exit(2);
                    }
                };
                if let Err(e) = proxy.set_profile(profile).await {
                    eprintln!("❌ Error setting power mode: {}", e);
                } else {
                    println!(
                        "🔧 Power mode set to: {} ({})",
                        mode.to_lowercase(),
                        profile
                    );
                }
            }
            PowerAction::Get => {
                let profile = proxy.active_profile().await?;
                // Map the daemon's canonical name back to the friendly one.
                let friendly = match profile.as_str() {
                    "power-saver" => "eco",
                    other => other,
                };
                println!("🔧 Power mode: {} ({})", friendly, profile);
            }
        },
        Commands::AcLock { state } => match state.as_deref() {
            Some(s @ ("on" | "off")) => {
                let enabled = s == "on";
                if let Err(e) = proxy.set_ac_max_performance(enabled).await {
                    eprintln!("❌ Error setting AC lock: {}", e);
                } else if enabled {
                    println!(
                        "🔒 AC lock ENABLED — plugging in now pins max performance (Performance / Max / Aggressive) and locks the controls. The battery charge limit stays editable."
                    );
                } else {
                    println!(
                        "🔓 AC lock DISABLED — AC is now fully manual; plugging in changes nothing."
                    );
                }
            }
            None => {
                let pref = proxy.ac_max_performance().await?;
                let live = proxy.ac_locked().await?;
                println!(
                    "🔌 AC lock: {} (preference) · currently {}",
                    if pref { "on" } else { "off" },
                    if live { "LOCKED (on AC)" } else { "unlocked" }
                );
            }
            // clap's value_parser restricts to on/off/None, so this is unreachable.
            Some(other) => {
                eprintln!("❌ Unknown argument '{}'. Use: on, off, or omit.", other);
                process::exit(2);
            }
        },
        Commands::FixPolkit { apply } => {
            // Normally intercepted in main() before the D-Bus setup; this
            // arm keeps the match exhaustive and behaves identically if
            // reached.
            process::exit(fix::run(apply));
        }
        Commands::Doctor { fix, apply } => {
            if fix {
                // Normally intercepted in main() before the D-Bus setup;
                // kept for exhaustiveness and identical behaviour.
                process::exit(doctor::run_fix(apply));
            }
            // Read-only health report against the running daemon.
            doctor::report(&proxy).await;
        }
    }
    Ok(())
}

/// Draw an 8-point fan curve as horizontal bars: temperature on the
/// left, the fan duty as a bar and a percentage on the right.
fn render_curve(label: &str, points: &[(u32, u32)]) {
    const BAR_W: u32 = 24;
    if points.is_empty() {
        println!("  {label}: firmware automatic (no custom curve)");
        return;
    }
    println!("  {label}:");
    for (temp, pwm) in points {
        let pwm = (*pwm).min(255);
        let pct = pwm * 100 / 255;
        let filled = (pwm * BAR_W / 255) as usize;
        let bar = "█".repeat(filled);
        let pad = " ".repeat(BAR_W as usize - filled);
        println!("    {temp:>3}°C │{bar}{pad}│ {pct:>3}%");
    }
}

/// Render a telemetry field, mapping the `i32::MIN` "unavailable"
/// sentinel from `get_thermal_status` to `n/a`.
fn fmt_telemetry(value: i32, unit: &str) -> String {
    if value == i32::MIN {
        "n/a".to_string()
    } else {
        format!("{}{}", value, unit)
    }
}

async fn print_dashboard(proxy: &PowerDaemonProxy<'_>) -> zbus::Result<()> {
    let spl_watts = proxy.current_spl().await?;
    let charge_limit = proxy.charge_end_threshold().await?;
    let auto_cooling = proxy.auto_cooling().await?;
    let fan_curve = proxy.fan_curve().await?;
    let (cpu_temp, gpu_temp, cpu_rpm, gpu_rpm, soc_power_mw) = proxy.get_thermal_status().await?;

    let is_ac = proxy.is_ac_connected().await?;
    let power_icon = if is_ac {
        "⚡ Connected (AC)"
    } else {
        "🔋 Battery (DC)"
    };
    let cooling_mode = if auto_cooling { "auto" } else { "manual" };
    // The fan curve is what actually drives the fans, so it is the
    // user-facing "cooling level"; `auto` means the firmware curve.
    let cooling_level = if fan_curve == "auto" {
        "firmware".to_string()
    } else {
        fan_curve
    };

    // Actual power draw vs the configured TDP cap. soc_power is in mW.
    let power_line = if soc_power_mw == i32::MIN {
        format!("{}W (TDP cap)", spl_watts)
    } else {
        format!("{}W now · {}W TDP cap", soc_power_mw / 1000, spl_watts)
    };

    println!("=======================================");
    println!("  🎮 Handheld Power Daemon Status 🎮  ");
    println!("=======================================");
    println!("   ⚡ Power:            {}", power_line);
    println!(
        "  🧊 Cooling:          {} ({})",
        cooling_level, cooling_mode
    );
    println!(
        "  🌡️ Temps:            CPU {} · GPU {}",
        fmt_telemetry(cpu_temp, "°C"),
        fmt_telemetry(gpu_temp, "°C")
    );
    println!(
        "  💨 Fans:             CPU {} · GPU {}",
        fmt_telemetry(cpu_rpm, " RPM"),
        fmt_telemetry(gpu_rpm, " RPM")
    );
    println!("  🔌 Power adapter:    {}", power_icon);
    println!("  🔋 Battery Limit:    {}%", charge_limit);
    println!("=======================================");

    Ok(())
}

/// Print the "System health" section under `hpdctl status`: a banner in the
/// dashboard's style wrapping the shared health block ([`doctor::print_health`]),
/// which reports polkit, competing daemons, GameMode, and the gamescope
/// session — and gives the all-clear when nothing is wrong.
async fn print_health_section(proxy: &PowerDaemonProxy<'_>) {
    println!();
    println!("=======================================");
    println!("  🩺 System health  ");
    println!("=======================================");
    doctor::print_health(proxy).await;
    println!("=======================================");
}

/// When a partial install left the polkit actions unregistered, offer to
/// install the policy right here in an interactive terminal — the one
/// keystroke fix. The health section above already explained the problem on
/// stdout; this only adds the interactive prompt (to stderr, keeping
/// stdout's dashboard clean). Degrades silently against an older daemon that
/// does not expose `get_diagnostics`.
async fn offer_polkit_fix_if_broken(proxy: &PowerDaemonProxy<'_>) {
    let (polkit_ok, _missing) = match proxy.get_diagnostics().await {
        Ok(diag) => diag,
        Err(_) => return,
    };
    if polkit_ok {
        return;
    }

    // Offer to fix it right here when run interactively, so the user never
    // has to open another shell or type a long command. Outside a TTY
    // (scripts, the Decky plugin shelling out) the health block already
    // named the fix, so stay quiet.
    use std::io::{IsTerminal, Write};
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        eprint!("Install the polkit policy now? [Y/n] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_ok() {
            let a = answer.trim().to_lowercase();
            if a.is_empty() || a == "y" || a == "yes" {
                fix::run(false);
                return;
            }
        }
        eprintln!("Skipped. Run `hpdctl fix-polkit` (or `hpdctl doctor --fix`) later.");
    }
}
