# hpd-cli (`hpdctl`)

> User-facing CLI client for the `hpd` daemon.

| Field   | Value                                                  |
|---------|--------------------------------------------------------|
| Layer   | **L4** — interface                                     |
| Stable  | since `1.0.0`                                          |
| Crate   | `hpd-cli`                                              |
| Binary  | `hpdctl`                                               |

## Purpose

Thin D-Bus client that talks to the running `hpd-daemon` over the
`dev.cirodev.hpd.PowerDaemon1` interface. Binds to the **system bus**
in production and to the **session bus** when `HPD_SIMULATOR` is set,
mirroring the daemon's `simulator` feature.

The CLI surface is stable under SemVer from `1.0.0` forward.

## Subcommands

| Command                       | Description                                 |
|-------------------------------|---------------------------------------------|
| `hpdctl tdp set <W>`          | Set SPL in watts (SPPT/FPPT derived).       |
| `hpdctl tdp get`              | Show current SPL.                           |
| `hpdctl charge set <%>`       | Set battery end threshold (20-100).         |
| `hpdctl charge get`           | Show current charge end threshold.          |
| `hpdctl preset <name>`        | Apply preset: `eco` / `balanced` / `max`.   |
| `hpdctl limits`               | Show hardware SPL/SPPT/FPPT ranges.         |
| `hpdctl status`               | One-shot dashboard (power, cooling, AC, charge + a "System health" section: polkit, competing/advisory daemons, gamescope hint). |
| `hpdctl monitor`              | Live dashboard, refreshed every second.     |
| `hpdctl cool set <level>`     | Set the **fan curve** (fans only): `silent`/`balanced`/`aggressive`. Decoupled from power. |
| `hpdctl cool auto`            | Let the daemon pick the fan curve from TDP. |
| `hpdctl cool reset`           | Hand the fans back to firmware control.     |
| `hpdctl cool get`             | Show current cooling level and mode.        |
| `hpdctl cool curve`           | Draw the active fan curve (temperature → fan speed). |
| `hpdctl cool set-custom <8 temp:pwm pairs>` | Set a hand-drawn 8-point curve (advanced), e.g. `45:20 54:50 62:95 69:145 75:190 80:225 85:255 92:255`. Daemon ≥ 2.9.0. |
| `hpdctl power set <mode>`     | Power mode / EPP: `performance`/`balanced`/`eco`. Default `performance`. |
| `hpdctl power get`            | Show current power mode.                    |
| `hpdctl ac-lock [on\|off]`    | Lock max performance on AC (on by default). No arg = show state. |
| `hpdctl gpu auto`             | Let the daemon infer the GPU clock ceiling from TDP. Daemon ≥ 2.12.0; `hpdctl gpu` shipped in 2.13.0. |
| `hpdctl gpu set <min_mhz> <max_mhz>` | Pin an explicit GPU clock range (MHz), disengages auto-follow. Daemon ≥ 2.12.0; `hpdctl gpu` shipped in 2.13.0. |
| `hpdctl gpu reset`            | Hand the GPU clock back to firmware auto. Daemon ≥ 2.12.0; `hpdctl gpu` shipped in 2.13.0. |
| `hpdctl gpu get`              | Show current GPU clock mode and committed range. Daemon ≥ 2.12.0; `hpdctl gpu` shipped in 2.13.0. |
| `hpdctl gpu limits`           | Show this device's supported GPU clock range (live `OD_RANGE`). Daemon ≥ 2.12.0; `hpdctl gpu` shipped in 2.13.0. |
| `hpdctl restore-defaults`     | Restore recommended defaults in one shot: TDP → Balanced, Power mode → Performance, Charge cap → 80%, Cooling → firmware auto, GPU clock → firmware auto (only if already opted in). Unreleased — merged to `main`, not yet in a tagged release (next after `2.13.0`, expected `2.14.0`). |
| `hpdctl doctor`               | Report whether polkit is installed and whether a competing power daemon is fighting hpd over TDP/profile/charge. Read-only. |
| `hpdctl doctor --fix`         | Neutralize competing daemons (mask) and install the polkit policy in one elevated step — a superset of `fix-polkit`. |
| `hpdctl fix-polkit`           | Install the polkit policy + rules and reload polkit (self-elevates via pkexec/sudo). |

Privileged subcommands (`tdp set`, `charge set`, `preset`, `cool set`,
`cool auto`, `cool reset`, `cool set-custom`, `power set`, `ac-lock`,
`gpu auto`, `gpu set`, `gpu reset`, `restore-defaults`, `doctor --fix`,
`fix-polkit`) are authorized by polkit. Members of the `wheel` group (the
device owner) run them without any prompt — including over SSH — via
`package/polkit/49-hpd.rules`. Any other user gets a polkit prompt
(answered once per 5-minute window for `set-profile`, per call for
`set-tdp` / `set-charge`).

## Dependencies

| Dep      | Purpose                                          |
|----------|--------------------------------------------------|
| `clap`   | Argument parsing (`derive` feature).             |
| `zbus`   | D-Bus proxy generated via `#[proxy]`.            |
| `tokio`  | Async runtime (`#[tokio::main]`).                |

The CLI does **not** depend on any internal `hpd-*` crate — its
D-Bus contract is the same one a third-party client would consume.

## Example

```bash
hpdctl status
hpdctl tdp set 15
hpdctl preset eco
hpdctl monitor          # Ctrl+C to exit
```

Simulator mode (macOS / dev hosts):

```bash
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
```

## Docs

```bash
cargo doc -p hpd-cli --no-deps --open
```

## License

GPL-3.0-or-later. See the workspace `Cargo.toml`.
