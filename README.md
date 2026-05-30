# hpd — Handheld Power Daemon

[![CI](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/actions/workflows/ci.yml/badge.svg)](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

A Linux system daemon for Windows-handheld-class PCs that owns four things
the platform usually scatters across BIOS, firmware attributes and ad-hoc
sysfs writes:

- **TDP envelope** (SPL / SPPT / FPPT) — sustained and burst power limits.
- **Platform / cooling profile** — `power-saver`, `balanced`, `performance`.
- **Custom fan curves** — EC-mediated temperature→speed curves
  (`silent` / `balanced` / `aggressive`) that cool harder than the
  conservative firmware default. See [`docs/fan-curves.md`](docs/fan-curves.md).
- **Battery charge threshold** — the upper SoC cap used to extend cell life.
- **Fan & temperature telemetry** — read-only RPM and CPU/GPU temperature
  reporting for the in-tree UIs.

Everything sits behind a single D-Bus interface
(`dev.cirodev.hpd.PowerDaemon1`) on the system bus, and a thin CLI
(`hpdctl`) drives it.

> **Status:** `v1.0.0` — the public surface (D-Bus interface, `hpdctl`
> CLI, on-disk state at `/var/lib/hpd/state.toml`, polkit action IDs)
> is stable and follows [SemVer](https://semver.org/). Future
> breaking changes require a major bump. See [`CHANGELOG.md`](CHANGELOG.md).

---

## Hardware support

Capability columns: **TDP**, **Charge** threshold, platform **Profile**,
fan **Curve** (write), **Fan/Temp** telemetry (read).

| Vendor / model | Backend crate | TDP | Charge | Profile | Curve | Fan/Temp | Status |
|---|---|:---:|:---:|:---:|:---:|:---:|---|
| ASUS ROG Ally          | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ⚠️ | ✅ | **Stable** (curve presets shared, not yet model-calibrated) |
| ASUS ROG Ally X        | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ⚠️ | ✅ | **Stable** (curve presets shared, not yet model-calibrated) |
| ASUS ROG Xbox Ally X   | `hpd-backend-asus`   | ✅ | ✅ | ✅ | ✅ | ✅ | **Stable** (primary test target — board `RC73XA`) |
| Lenovo Legion Go       | —                    | — | — | — | — | — | Planned — no backend crate yet ([open an issue](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/issues) to contribute) |
| Valve Steam Deck       | —                    | — | — | — | — | — | Planned — no backend crate yet ([open an issue](https://github.com/CiroDev-Git/hpd-handheld-power-daemon/issues) to contribute) |

Detection is driven by DMI (`/sys/class/dmi/id/`). Adding a new vendor
means creating a sibling crate that implements the four L2 traits from
`hpd-capabilities`; see [`CLAUDE.md`](CLAUDE.md) for the recipe.

---

## Install

### Arch / CachyOS / EndeavourOS — recommended

Install the prebuilt AUR package. No Rust toolchain needed on the
handheld; the binaries come straight from the official GitHub
release tarball:

```bash
paru -S hpd-handheld-power-daemon-bin   # or: yay -S …
```

The package enables and starts `hpd.service` automatically on install,
so there is no manual `systemctl` step. Check it with
`systemctl status hpd` / `hpdctl status`.

> **Migrating from a previous `./install.sh` deployment?** Just install
> the AUR package — its install hook automatically removes the files
> `install.sh` placed at `/usr/local/bin`, `/etc` and `/usr/share`
> (which would otherwise shadow the packaged binaries or cause pacman
> file conflicts). No manual cleanup needed.

There are two AUR packages:

- **`hpd-handheld-power-daemon-bin`** — prebuilt x86_64 repack of the
  official release tarball. Fast install, no compilation, no
  toolchain. The right default for end users.
- **`hpd-handheld-power-daemon`** — builds from source on your
  machine. Pulls in `rust`, `cargo`, `pkgconf`, `systemd-libs` as
  `makedepends`. Useful if you want to audit the build, run on a
  non-x86_64 arch in the future, or test pre-release commits.

### Other distros / building from source

Requires Rust ≥ 1.85 (the workspace MSRV), a Linux host with
`systemd`, `dbus`, and `polkit`. Tested on recent Fedora and Debian
families.

```bash
git clone https://github.com/CiroDev-Git/hpd-handheld-power-daemon.git
cd hpd-handheld-power-daemon
./install.sh
```

`install.sh` runs `scripts/doctor.sh` as a preflight: it verifies
cargo + rustc at MSRV, systemd, D-Bus, polkit, a C linker, and
DMI-probes the board against the supported ASUS list. If anything
is missing, install.sh aborts with copy-paste remediation hints
(including the exact rustup / pacman / dnf / apt command for your
distro). Re-run the doctor standalone anytime:

```bash
./scripts/doctor.sh           # full report
./scripts/doctor.sh --quiet   # only warnings + failures
```

Skip the preflight (advanced): `./install.sh --skip-doctor`.

Once the doctor passes, `install.sh` builds release binaries with
the default feature set (`vendor-asus`), copies `hpd-daemon` and
`hpdctl` into `/usr/local/bin/`, installs the systemd unit, D-Bus
policy, polkit policy + rule from `package/`, then enables and starts
`hpd.service`. Live logs:

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
read commands. Write commands (TDP, cooling profile, charge limit) are
authorized by polkit: members of the **`wheel`** group — the device
owner — run them without a password, including over SSH, via the rule
in `package/polkit/49-hpd.rules`. Any other user is prompted to
authenticate as an administrator (the `auth_admin` defaults in
`package/polkit/dev.cirodev.hpd.policy`). Both files are installed by
`install.sh`.

```bash
# Read current state
hpdctl status                  # power target, profile, fan curve, temps, RPM, charge, AC
hpdctl limits                  # hardware SPL/SPPT/FPPT range

# Power envelope
hpdctl tdp set 18              # smart mode: SPL=18W, SPPT/FPPT derived
hpdctl tdp get
hpdctl preset eco|balanced|max # presets relative to hardware range

# Cooling — one lever (platform profile + fan curve together)
hpdctl cool set silent|balanced|aggressive
hpdctl cool auto               # let the daemon pick the level from TDP
hpdctl cool reset              # hand the fans back to firmware control
hpdctl cool get                # current level + mode

# (The raw platform profile and fan curve are available over D-Bus —
#  set_profile / set_fan_curve — for advanced/decoupled use.)

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
the cooling manually (`hpdctl cool set …`) latches `fan_follows_tdp=false`
until you call `hpdctl cool auto` again.

### Cooling is one lever

Internally there are two mechanisms — the ACPI **platform profile** and
the EC **fan curve** — but you drive them as a single thing:

- **`hpdctl cool set <level>`** sets the platform profile *and* the fan
  curve to the matching level (`silent → power-saver`,
  `balanced → balanced`, `aggressive → performance`) and switches to
  manual cooling. While a custom curve is active, *it* drives the fans
  (it overrides the profile's built-in firmware curve).
- **`hpdctl cool auto`** lets the daemon infer the level from the TDP
  fraction of the hardware range (the default mode).

The daemon keeps the two in sync for you:

- **Profile changes re-assert the curve.** Writing the platform profile
  can make the EC drop the custom curve back to automatic, so the daemon
  re-applies the active curve immediately after any profile change and
  after resume from suspend — you never silently lose it.
- **`fan_curve_follows_profile`** (config, **on by default**) is what
  ties them together. Set it to `false` only if you want to drive the raw
  platform profile and fan curve independently over D-Bus (`set_profile`
  / `set_fan_curve`).

The platform profile is not cosmetic: on the Ally family it gates the
*real* power the chip may draw (measured: ~36 °C swing between
`power-saver` and `performance` at a fixed TDP), which is why a cooling
level couples a profile with a curve rather than just a fan speed.

See [`docs/fan-curves.md`](docs/fan-curves.md) for the thermal rationale
and [`docs/dev/FAN_CURVE_TESTING.md`](docs/dev/FAN_CURVE_TESTING.md) for
the on-device validation plan. A plain-language explainer in Spanish
(cooling, auto vs manual, the power gating) lives in
[`docs/COOLING-es.md`](docs/COOLING-es.md).

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

For the full design, the dependency direction rules, the lifecycle
matrix, the rollback contract, and the recipes for adding a new D-Bus
method or vendor backend, read [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).
Each crate also ships a one-page `README.md` under `crates/<name>/`
that explains its purpose, dependencies, and a runnable example.

The terse assistant-oriented version of the architecture doc lives at
[`CLAUDE.md`](CLAUDE.md).

Working audits live under `docs/audit/` (gitignored — internal
notes); the shipped behaviour and per-release breaking-change log
live in [`CHANGELOG.md`](CHANGELOG.md).

---

## Contributing

Bug reports, vendor backends, doc improvements, and CI work are all
welcome. Start with [`CONTRIBUTING.md`](CONTRIBUTING.md) — it covers
the four local gates CI enforces, the commit/CHANGELOG conventions,
the SemVer policy on the public surface, and the recipes for adding
a new D-Bus method or a new vendor backend.

Per-OS development guides live under [`docs/dev/`](docs/dev):

- Linux: [`docs/dev/LINUX.md`](docs/dev/LINUX.md) — production-shape
  + iterative `cargo run` workflows.
- macOS: [`docs/dev/MACOS.md`](docs/dev/MACOS.md) — simulator-first
  workflow against the session D-Bus.

---

## License

`GPL-3.0-or-later`. Full text in [`LICENSE`](LICENSE). Contributions
are accepted under the same license — opening a PR is implicit
agreement, the project does not require a CLA.
