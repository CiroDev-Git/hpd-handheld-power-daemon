# hpd — Handheld Power Daemon

[![CI](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/actions/workflows/ci.yml/badge.svg)](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

A Linux system daemon for Windows-handheld-class PCs that owns four things
the platform usually scatters across BIOS, firmware attributes and ad-hoc
sysfs writes:

- **TDP envelope** (SPL / SPPT / FPPT) — sustained and burst power limits.
- **Platform / cooling profile** — `power-saver`, `balanced`, `performance`.
- **Battery charge threshold** — the upper SoC cap used to extend cell life.
- **Fan telemetry** — read-only RPM reporting for the in-tree UIs.

Everything sits behind a single D-Bus interface
(`dev.cirodev.hpd.PowerDaemon1`) on the system bus, and a thin CLI
(`hpdctl`) drives it.

> **Status:** `v1.0.0` — the public surface (D-Bus interface, `hpdctl`
> CLI, on-disk state at `/var/lib/hpd/state.toml`, polkit action IDs)
> is stable and follows [SemVer](https://semver.org/). Future
> breaking changes require a major bump. See [`CHANGELOG.md`](CHANGELOG.md).

---

## Hardware support

| Vendor / model | Backend crate | TDP | Charge | Profile | Fan | Status |
|---|---|:---:|:---:|:---:|:---:|---|
| ASUS ROG Ally          | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ✅ | **Stable** |
| ASUS ROG Ally X        | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ✅ | **Stable** |
| ASUS ROG Xbox Ally X   | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ✅ | **Stable** (primary test target — board `RC73XA`) |
| Lenovo Legion Go       | —                    | — | — | — | — | Planned — no backend crate yet ([open an issue](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/issues) to contribute) |
| Valve Steam Deck       | —                    | — | — | — | — | Planned — no backend crate yet ([open an issue](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/issues) to contribute) |

Detection is driven by DMI (`/sys/class/dmi/id/`). Adding a new vendor
means creating a sibling crate that implements the four L2 traits from
`hpd-capabilities`; see [`CLAUDE.md`](CLAUDE.md) for the recipe.

---

## Install

Requires Rust ≥ 1.75, a Linux host with `systemd` and `dbus`. Tested on
recent Arch / Fedora / Debian families.

```bash
./install.sh
```

The script `cargo build --release`s with the default feature set
(`vendor-asus`), copies `hpd-daemon` and `hpdctl` into
`/usr/local/bin/`, installs the systemd unit and D-Bus policy from
`package/`, then enables and starts `hpd.service`. Live logs:

```bash
journalctl -fu hpd
```

State lives in `/var/lib/hpd/state.toml` (created by `StateDirectory=hpd`
in the unit). Uninstall with `./uninstall.sh`; pass `--purge` to also
wipe the state directory.

To opt into additional vendor backends:

```bash
cargo build --release --features vendor-lenovo,vendor-valve
```

---

## Using `hpdctl`

`hpdctl` talks to the running daemon over D-Bus. No root needed for
read commands; writes go through the system bus policy installed by
`install.sh`.

```bash
# Read current state
hpdctl status                  # power target, profile, charge, AC
hpdctl limits                  # hardware SPL/SPPT/FPPT range

# Power envelope
hpdctl tdp set 18              # smart mode: SPL=18W, SPPT/FPPT derived
hpdctl tdp get
hpdctl preset eco|balanced|max # presets relative to hardware range

# Cooling profile (independent of TDP)
hpdctl fan set power-saver|balanced|performance
hpdctl fan auto                # re-bind cooling to follow TDP

# Battery
hpdctl charge set 80           # 20..=100, persisted across reboots
hpdctl charge get

# Live monitor
hpdctl monitor                 # refreshes once a second
```

Run `hpdctl --help` for the full subcommand list and arg shapes.

### TDP envelope vs cooling profile

These are deliberately decoupled:

- **TDP envelope** (SPL/SPPT/FPPT) is *how much power the SoC may draw*.
- **Cooling profile** is *how aggressively the fans + ACPI hints respond*.

When `fan_follows_tdp` is on (default), changing the envelope re-infers
a cooling profile from the SPL fraction of the hardware range
(`< 33% → power-saver`, `< 67% → balanced`, else `performance`). Setting
the profile manually (`hpdctl profile …`) latches `fan_follows_tdp=false`
until you call `hpdctl fan auto` again.

---

## Development

### Running on Linux against real hardware

```bash
cargo run -p hpd-daemon          # debug build, system bus
RUST_LOG=hpd=debug cargo run -p hpd-daemon
```

`hpd-daemon` must run as root (or with the right CAP_SYS_ADMIN /
CAP_DAC_OVERRIDE caps) to write firmware-attribute sysfs nodes.

### Running on macOS / a dev host without real sysfs

The `simulator` Cargo feature compiles in a `MockSysfs` pre-populated
with the expected ASUS firmware attributes, and switches the daemon
+ CLI to the D-Bus **session bus** so root is not required:

```bash
HPD_SIMULATOR=1 cargo run -p hpd-daemon --features simulator
# in another shell, talk to it via:
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
```

The `simulator` feature implies `vendor-asus` because the simulator only
models ASUS firmware today. Production binaries (default `cargo build
--release`) intentionally do **not** include the simulator path — the
env var is a no-op there.

### Tests

```bash
cargo test --workspace          # everything
cargo test -p hpd-core          # one crate
cargo test --test executor_e2e  # the Executor pipeline integration tests
```

The reducer is pure and lives at `hpd-core/src/reducer.rs`; it has
~20 unit tests covering every `Transition` variant and every boundary
of the power envelope invariants. The executor's full pipeline (reduce
→ effect → backend) is covered by `crates/hpd-core/tests/executor_e2e.rs`
against the `MockBackend` fixture from `hpd-capabilities::testing`.

### Lints

```bash
cargo clippy --workspace --all-targets
```

`unsafe_code` is forbidden workspace-wide (only exception: `hpd-netlink`,
which opts in locally). `clippy::unwrap_used`, `expect_used`, `panic`
are warnings — treat them as errors during review.

---

## Architecture

```text
L-1  hpd-error          Cross-cutting error types (no internal deps)
L0   hpd-sysfs          Sysfs read/write trait + RealSysfs + MockSysfs
     hpd-netlink        udev AC/DC event monitor (Linux-only)
L2   hpd-capabilities   Trait surface for L1 backends + value types
L1   hpd-backend-asus   ASUS armoury firmware-attribute backend
                                  (Lenovo and Valve placeholder backends removed in 1.0; reintroduce as 1.x minors when real implementations land)
L3   hpd-core           Pure reducer + Executor + state machine
L4   hpd-dbus           zbus interface
     hpd-cli (hpdctl)   D-Bus client
     hpd-daemon         Composition root + systemd entry point
```

All hardware mutations flow through a `Transition → reduce() → Effect →
Backend` pipeline. The reducer is pure (no I/O, no async). The Executor
runs effects, persists state, and on hardware-write failure re-injects
`SyncPowerTarget` to roll the in-memory state back to reality.

For the full design, the dependency direction rules, and the recipes
for adding a new D-Bus method or vendor backend, see
[`CLAUDE.md`](CLAUDE.md).

Working audits live under `docs/audit/` (gitignored — internal
notes); the shipped behaviour and per-release breaking-change log
live in [`CHANGELOG.md`](CHANGELOG.md).

---

## License

`GPL-3.0-or-later`. Full text in [`LICENSE`](LICENSE). Contributions
are accepted under the same license — opening a PR is implicit
agreement, the project does not require a CLA.
