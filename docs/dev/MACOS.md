# Development on macOS

> Local dev loop on a Mac, against the in-memory simulator. There is
> no real sysfs on macOS, so the daemon runs in **simulator mode**
> against `MockSysfs`, binds to the **D-Bus session bus** instead of
> the system bus, and bypasses polkit entirely.
>
> Linux dev workflow: [`LINUX.md`](LINUX.md).
> Architecture context: [`../ARCHITECTURE.md`](../ARCHITECTURE.md).
> Contribution rules: [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md).

---

## 1. What works and what doesn't on macOS

| Capability                                           | macOS dev host        |
|------------------------------------------------------|-----------------------|
| Build the whole workspace                            | ✅                    |
| Run `cargo test --workspace` (the full suite)        | ✅                    |
| `cargo clippy`, `cargo fmt`, `cargo doc`             | ✅                    |
| Run `hpd-daemon` against fake ASUS firmware           | ✅ (simulator mode)   |
| Run `hpdctl` against the simulated daemon            | ✅ (session bus)      |
| Exercise the full Transition → Effect pipeline        | ✅                    |
| GPU clock range reads (`gpu limits`, `gpu get`, `gpu reset`) | ✅ (since v2.13.0 — simulator seeds `OD_RANGE`) |
| GPU clock range writes (`gpu auto`, `gpu set`)         | ❌ (see §8 — `MockSysfs` can't model the driver's commit-and-read-back) |
| Polkit prompts                                       | ❌ (bypassed)         |
| Real udev AC plug events                             | ❌ (`tokio-udev` is Linux-only; macOS gets a no-op stub) |
| Real suspend/resume integration                      | ❌ (no logind on macOS) |
| Real systemd unit / `journalctl`                     | ❌                    |
| Production sysfs writes                              | ❌                    |

If you need any of the ❌ flows, you have to test on Linux. Most
state-machine work doesn't need them.

---

## 2. Prerequisites

| Tool                | Why                                                          | Install                                            |
|---------------------|--------------------------------------------------------------|----------------------------------------------------|
| `rustup`            | Toolchain manager. Project pins `1.85` via `rust-toolchain.toml`. | <https://rustup.rs>                            |
| Xcode CLT           | Linker, `cc`, `ar`. Required by every Rust build on macOS.   | `xcode-select --install`                           |
| Homebrew            | Convenient channel for `dbus`.                                | <https://brew.sh>                                  |
| `dbus`              | Daemon talks to the **session bus** in simulator mode.        | `brew install dbus`                                |
| `pkg-config`        | Helps build a couple of transitive crate-deps cleanly.        | `brew install pkg-config`                          |

`tokio-udev`, `libudev`, `polkit`, `systemd` are **not** required on
macOS — the workspace is cfg-gated so those deps are only compiled in
on `cfg(target_os = "linux")`.

### Toolchain pin

`rust-toolchain.toml` pins the project to `1.85`. When you `cd` into
the repo `rustup` installs that channel automatically. CI uses the
same pin.

```bash
rustup show          # confirms 1.85 active
cargo --version
```

---

## 3. Start the session D-Bus

macOS doesn't run a session bus by default. Start one once per shell
session (or via `launchctl` for persistence — see §3a).

```bash
brew services start dbus      # registers as a Homebrew service
# or, ephemeral:
eval "$(dbus-launch --sh-syntax)"
echo "$DBUS_SESSION_BUS_ADDRESS"   # confirm it's set
```

Both `hpd-daemon` (under `--features simulator`) and `hpdctl` look at
`HPD_SIMULATOR=1` to decide whether to bind to the session bus
instead of the system bus. Without `HPD_SIMULATOR` they will try the
system bus and fail (macOS has none).

### 3a. Persistent session bus via launchctl (optional)

```bash
brew services start dbus    # launchd loads org.freedesktop.dbus-session
# verify the env var the daemon will read:
launchctl getenv DBUS_SESSION_BUS_ADDRESS
```

If `DBUS_SESSION_BUS_ADDRESS` is empty in a fresh terminal but
`brew services list` shows `dbus started`, source the launchd env
into your shell rcfile:

```bash
echo 'export DBUS_SESSION_BUS_ADDRESS="$(launchctl getenv DBUS_SESSION_BUS_ADDRESS)"' >> ~/.zshrc
```

---

## 4. Run the simulator end-to-end

Two-terminal flow:

```bash
# terminal 1 — the simulated daemon
HPD_SIMULATOR=1 cargo run -p hpd-daemon --features hpd-daemon/simulator

# terminal 2 — the CLI client
HPD_SIMULATOR=1 cargo run -p hpd-cli -- status
HPD_SIMULATOR=1 cargo run -p hpd-cli -- tdp set 20
HPD_SIMULATOR=1 cargo run -p hpd-cli -- preset eco
HPD_SIMULATOR=1 cargo run -p hpd-cli -- monitor
```

**What the simulator actually does:**

1. Skips DMI detection and pretends to be a ROG Ally X
   (`board_vendor = "ASUSTeK COMPUTER INC."`, `board_name = "RC72L"`).
2. Builds a `MockSysfs` (backed by `tempfile::TempDir`) pre-populated
   with the ASUS firmware-attribute files the backend expects:
   `ppt_pl{1,2,3}_*`, `platform_profile{,_choices}`,
   `charge_control_end_threshold`. Initial values:
   - SPL min/max/current = 7 / 35 / 15 W
   - SPPT max = 43 W, current = 15 W
   - FPPT max = 55 W, current = 15 W
   - Profile = `balanced`, choices = `quiet balanced performance`
   - Charge end threshold = 80%
3. Wires the daemon's D-Bus server to the **session bus** at the
   same name (`dev.cirodev.hpd.PowerDaemon1`).
4. Activates `hpd-dbus`'s `simulator` feature, which short-circuits
   `polkit::check` to always return `true`. There is no
   `PolicyKit1.Authority` to talk to on macOS.
5. The netlink monitor compiles to a no-op stub on non-Linux, so the
   daemon stays alive but never receives AC events.

Verbose logging:

```bash
RUST_LOG=hpd=debug HPD_SIMULATOR=1 \
    cargo run -p hpd-daemon --features hpd-daemon/simulator
```

---

## 5. Manual D-Bus calls against the simulator

`dbus-send` ships with Homebrew's `dbus`. It works against the
session bus the same way it does against the system bus on Linux —
just drop the `--system` flag.

```bash
# read a property
dbus-send --session --print-reply \
    --dest=dev.cirodev.hpd.PowerDaemon1 \
    /dev/cirodev/hpd/PowerDaemon1 \
    org.freedesktop.DBus.Properties.Get \
    string:"dev.cirodev.hpd.PowerDaemon1" \
    string:"current_spl"

# call a setter (no polkit prompt — simulator bypass)
dbus-send --session --print-reply \
    --dest=dev.cirodev.hpd.PowerDaemon1 \
    /dev/cirodev/hpd/PowerDaemon1 \
    dev.cirodev.hpd.PowerDaemon1.SetSpl \
    uint32:18

# watch PropertiesChanged signals
dbus-monitor --session \
    "interface='org.freedesktop.DBus.Properties'"
```

`hpdctl` is just a convenience wrapper around these calls. If
something looks wrong in the CLI output, dropping down to
`dbus-send` confirms whether the issue is in the daemon's reply or
in the CLI's rendering.

---

## 6. Tests

The workspace test suite is fully cross-platform — every gate runs
on macOS the same way it does on Linux.

```bash
cargo test --workspace
# → the full suite passes
```

`MockSysfs` is the same fixture the simulator uses, so the unit and
integration tests exercise the exact same backend code path you'd
hit at runtime.

CI also runs a dedicated `macos-simulator` job (see
`.github/workflows/ci.yml`) that builds the daemon with
`--features simulator` on macOS to catch any Linux-only assumption
that sneaks in.

---

## 7. Common pitfalls

- **`hpdctl` says "Cannot connect to D-Bus"** — no session bus is
  running, or `DBUS_SESSION_BUS_ADDRESS` isn't exported in this
  shell. See §3.
- **`hpd-daemon` exits with "Hardware not supported"** — you forgot
  `HPD_SIMULATOR=1`, or you didn't build with
  `--features simulator`. Both are required together on macOS.
- **`cargo build` fails on `libudev`** — you accidentally built with
  `--features vendor-asus` *and* something pulled `tokio-udev` into
  the macOS dependency graph. Should never happen with the default
  workspace `cfg(target_os = "linux")` gate; if it does, file an
  issue.
- **Polkit prompts never appear in the simulator** — that's by
  design. The `simulator` feature on `hpd-dbus` short-circuits
  the check. To exercise the real polkit path you have to test on
  Linux.
- **`tempfile` warnings about cleanup** — `MockSysfs` keeps its
  `TempDir` alive for the lifetime of the daemon process. If you
  Ctrl+C the daemon mid-run, the dir gets cleaned up; if the
  process is force-killed, leftover dirs under `/tmp/.tmpXXXXXX`
  may stay until reboot. Safe to delete.
- **Build is suspiciously slow on first run** — `cargo` is
  downloading the macOS-flavoured prebuilt deps. Subsequent builds
  are quick.

---

## 8. Limitations of the simulator (be explicit when reporting bugs)

The simulator covers most of the daemon's logic but cannot model:

- Real udev `power_supply` AC plug/unplug events (no-op stub on
  non-Linux). To test `Transition::AcPowerChanged`, write a unit
  test that pushes it into the executor directly, or run on Linux.
- logind `PrepareForSleep` signals. To test `Transition::SystemResumed`,
  same workaround.
- Hardware-write **rollback** under realistic latency — `MockSysfs`
  never fails, so the rollback path is exercised only by the
  dedicated unit tests in `hpd-core::executor::tests`.
- Polkit denial / auth-admin-keep timing. Bypassed entirely on macOS.
- **GPU clock *writes* (`gpu auto` / `gpu set`), unlike GPU clock
  *reads*.** Since `[2.13.0]`, the simulator seeds a real ROG Xbox Ally X
  `OD_RANGE` capture, so `gpu limits` / `gpu get` / `gpu reset` work
  against `HPD_SIMULATOR=1` — but `gpu auto` and `gpu set` still fail
  there. Quoting the `CHANGELOG.md` `[2.13.0]` entry on why: "`gpu
  auto`/`gpu set` still fail there: `pp_od_clk_voltage` is a command
  file on real hardware (the *driver* updates its own `OD_SCLK`/
  `OD_RANGE` report after a `s`/`c` write), but `MockSysfs` is a flat
  store, so the mandatory commit-and-read-back fails — modeling that
  stateful parse is future work (`hpd-backend-asus`'s own unit tests
  work around it locally with a `simulate_committed` test helper)." To
  exercise a real GPU-clock write, you need real Linux hardware with an
  amdgpu OverDrive interface.

When filing a bug found via the simulator, please indicate "found
under macOS simulator mode" so the maintainer knows which paths
need extra verification on real Linux hardware.

---

## 9. Where to next

- Architecture deep-dive: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- Per-crate docs: `crates/<name>/README.md`
- Contribution rules: [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md)
- Linux production-shape workflow: [`LINUX.md`](LINUX.md)
