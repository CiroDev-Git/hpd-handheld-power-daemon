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

> **Status:** `v2.14.0` — the public surface (D-Bus interface, `hpdctl`
> CLI, on-disk state at `/var/lib/hpd/state.toml`, polkit action IDs)
> is stable and follows [SemVer](https://semver.org/). Future
> breaking changes require a major bump. See [`CHANGELOG.md`](CHANGELOG.md)
> for the exact version and everything shipped since.

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

> **`Permission denied` / `AuthFailed` on *every* write?** The polkit
> policy isn't installed — common when only the binary was deployed (a
> hand-copy, or a plugin) without `package/polkit/*`, so polkit doesn't
> know the `dev.cirodev.hpd.*` actions. Recover in one command:
>
> ```bash
> hpdctl fix-polkit          # installs the policy + rules, reloads polkit
> ```
>
> It self-elevates (`pkexec`/`sudo`), needs no daemon restart, and works
> even with the source tree gone (the files are embedded in the binary).
> `hpdctl status` detects this and offers to run it for you; the daemon
> also logs it loudly at startup. Programmatic clients read the
> `GetDiagnostics()` D-Bus method (`(polkit_ok, missing_action_ids)`).

```bash
# Read current state
hpdctl status                  # power target, profile, fan curve, temps, RPM, charge, AC
hpdctl limits                  # hardware SPL/SPPT/FPPT range

# Power envelope
hpdctl tdp set 18              # smart mode: SPL=18W, SPPT/FPPT derived
hpdctl tdp get
hpdctl preset eco|balanced|max # presets relative to hardware range

# Cooling — fans only (noise vs temperature; independent of power)
hpdctl cool set silent|balanced|aggressive
hpdctl cool auto               # let the daemon pick the fan curve from TDP
hpdctl cool reset              # hand the fans back to firmware control
hpdctl cool get                # current level + mode
hpdctl cool curve              # draw the active fan curve
hpdctl cool set-custom 45:20 54:50 62:95 69:145 75:190 80:225 85:255 92:255
                                # hand-drawn 8-point curve (temp_c:pwm), advanced

# Power mode / EPP — a separate lever from TDP and cooling, default `performance`
hpdctl power set performance|balanced|eco
hpdctl power get

# GPU clock range — optional, opt-in frequency ceiling (daemon >= 2.12.0)
hpdctl gpu limits              # this device's supported range (live OD_RANGE)
hpdctl gpu auto                # match the ceiling to the current TDP preset
hpdctl gpu set 700 1500        # pin an explicit MHz range (disengages auto)
hpdctl gpu reset               # hand the GPU clock back to firmware auto
hpdctl gpu get                 # current mode + committed range

# Battery
hpdctl charge set 80           # 20..=100, persisted across reboots
hpdctl charge get

# AC lock — pin max performance while plugged in (on by default)
hpdctl ac-lock                 # show the current state
hpdctl ac-lock on|off          # on = lock max on AC; off = AC fully manual

# Restore recommended defaults in one shot (daemon >= 2.14.0)
hpdctl restore-defaults        # TDP->Balanced, Power->Performance, Charge->80%,
                                # Cooling->firmware auto, GPU clock only if opted in

# Live monitor
hpdctl monitor                 # refreshes once a second
```

Run `hpdctl --help` for the full subcommand list and arg shapes.

### Power and cooling are decoupled

`hpd` has two independent levers:

- **Power** — the **TDP envelope** (SPL/SPPT/FPPT) you set is *how much
  power the SoC may draw*, and it's the real limit. It's backed by the
  ACPI **platform profile** (EPP), which defaults to `performance` so the
  SPL is fully usable (see below).
- **Cooling** — the EC **fan curve** is *how hard the fans work* (noise vs
  temperature). It does **not** affect power.

When `fan_follows_tdp` is on (default), changing the envelope re-infers
the **fan curve** from the SPL fraction of the hardware range
(`< 33% → silent`, `< 67% → balanced`, else `aggressive`). Setting cooling
manually (`hpdctl cool set …`) latches `fan_follows_tdp=false` until you
call `hpdctl cool auto` again. The platform profile is **never** inferred
from TDP.

### Why the decouple? (the platform profile gates power)

On the Ally family the ACPI platform profile doesn't just hint the fans —
its EPP **gates the real power** the chip draws. Measured on the Xbox
Ally X at a fixed SPL: `power-saver` drew ~13 W, `performance` ~29–40 W.

`hpd` used to tie the cooling level to that profile (`silent → power-saver`,
…), so picking a quiet cooling level **silently throttled the chip** — a
`tdp set 25` could run at ~13 W. "TDP didn't mean TDP." So:

- The **platform profile defaults to `performance`** (config
  `default_platform_profile`, applied at boot, which also migrates a
  device left in a throttling profile by an older hpd). Change it with the
  D-Bus `set_profile` (or the config key) only if you want an efficiency
  bias — `performance` / `balanced` / `power-saver`.
- **`hpdctl cool set / auto` drive the fan curve only.** Any combination is
  now valid, including "full TDP + quiet fans".
- The daemon still **re-asserts the active curve after any profile write**
  (and on resume), because writing the profile can make the EC drop the
  custom curve. (The old `fan_curve_follows_profile` config knob was removed
  in 2.6.0; a stale line in an existing `config.toml` is silently ignored.)

**Full user manual:** [`docs/MANUAL.md`](docs/MANUAL.md) (English) ·
[`docs/MANUAL-es.md`](docs/MANUAL-es.md) (Spanish) — every feature, every
combination, and a "what's normal vs. what to worry about" guide.

See [`docs/fan-curves.md`](docs/fan-curves.md) for the thermal rationale
and [`docs/dev/GAMING-ROADMAP-es.md`](docs/dev/GAMING-ROADMAP-es.md)
("Fase 3 — Curvas de ventilador personalizadas") for the on-device
validation plan, including the 2.9.0 custom-curve editor. A
plain-language explainer in Spanish
(the power↔cooling decouple, auto vs manual, what changed and why) lives
in [`docs/COOLING-es.md`](docs/COOLING-es.md). For a **visual** walkthrough
(diagrams of the daemon, the Decky plugin, and how they talk — every
combination, for dummies) see [`docs/DIAGRAMS.md`](docs/DIAGRAMS.md)
(English) / [`docs/DIAGRAMS-es.md`](docs/DIAGRAMS-es.md) (Spanish).

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
