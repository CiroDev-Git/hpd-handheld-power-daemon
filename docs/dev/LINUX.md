# Development on Linux

> The full development loop on a Linux host, including running
> against real sysfs on the target hardware.
>
> macOS workflow lives at [`MACOS.md`](MACOS.md).
> Architecture context: [`../ARCHITECTURE.md`](../ARCHITECTURE.md).
> Contribution rules: [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md).

---

## 1. Prerequisites

| Tool         | Why                                                       | Recommended source                                  |
|--------------|-----------------------------------------------------------|-----------------------------------------------------|
| `rustup`     | Toolchain manager. Project pins `1.85` via `rust-toolchain.toml`. | <https://rustup.rs>                          |
| `git`        | Source control.                                           | distro package.                                     |
| `systemd`    | Service supervision. Daemon ships a unit only for systemd.| pre-installed on target distros.                    |
| `dbus`       | IPC. The daemon binds to the **system bus** in production.| pre-installed on target distros.                    |
| `polkit`     | Authorization gate for D-Bus setters.                     | distro package (`polkit` / `polkit-1`).             |
| `pkg-config` | Required by `tokio-udev`'s libudev binding.               | distro package.                                     |
| `libudev`    | Linked by `tokio-udev`.                                   | distro package (`libudev-dev` / `systemd-devel`).   |

### Toolchain pin

The repo carries a `rust-toolchain.toml` pinning the channel to
`1.85`. When you `cd` into the repo, `rustup` automatically installs
that exact version (no manual `rustup install` needed). CI uses the
same pin, so if local builds pass, CI builds the same compiler.

```bash
rustup show           # confirms 1.85 active
cargo --version       # → cargo 1.85.x
```

### Distro hints

| Distro family       | One-liner to satisfy build deps                                      |
|---------------------|----------------------------------------------------------------------|
| Arch                | `sudo pacman -S --needed base-devel pkgconf systemd dbus polkit`     |
| Fedora              | `sudo dnf install -y gcc pkgconf-pkg-config systemd-devel dbus polkit` |
| Debian / Ubuntu     | `sudo apt install -y build-essential pkg-config libudev-dev dbus libpolkit-gobject-1-dev` |

---

## 2. Workspace overview

| Command                                                       | What it does                                                     |
|---------------------------------------------------------------|------------------------------------------------------------------|
| `cargo build`                                                 | Debug build of the whole workspace.                              |
| `cargo build --release`                                       | Release build (what `install.sh` ships).                         |
| `cargo test --workspace`                                      | Run every test in every crate (currently 58).                    |
| `cargo test -p hpd-core`                                      | Tests for a single crate.                                        |
| `cargo test -p hpd-core test_profile_inference`               | A single test by substring.                                      |
| `cargo clippy --workspace --all-targets -- -D warnings`       | Lint gate CI runs. **Run this before pushing.**                  |
| `cargo fmt --all -- --check`                                  | Formatting gate CI runs.                                         |
| `cargo fmt --all`                                             | Auto-format the workspace.                                       |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`  | Documentation gate CI runs.                                      |
| `cargo doc -p hpd-core --no-deps --open`                      | Open a single crate's rustdoc in your browser.                   |

### Feature matrix

The daemon has three meaningful feature combinations; CI builds all
of them. If you touch `Cargo.toml` or `cfg`-gated code, run them all.

```bash
cargo build -p hpd-daemon                                # default = vendor-asus
cargo build -p hpd-daemon --no-default-features          # no vendor (must still compile)
cargo build -p hpd-daemon --features simulator           # implies vendor-asus + hpd-dbus/simulator
```

---

## 3. Two ways to run the daemon

### 3a. Production-shape: real sysfs on the target handheld

This is what `install.sh` automates. Suitable when you're developing
on the actual ROG Ally / Ally X / Xbox Ally X.

```bash
./install.sh         # builds release, installs binaries+unit+policies, enables hpd.service
journalctl -fu hpd   # live logs from the daemon
hpdctl status        # one-shot dashboard
hpdctl monitor       # live dashboard (1 Hz)
./uninstall.sh       # remove without touching state/config
./uninstall.sh --purge   # also wipe /var/lib/hpd and /etc/hpd
```

What `install.sh` actually places:

| Path                                                          | Source in repo                            |
|---------------------------------------------------------------|-------------------------------------------|
| `/usr/local/bin/hpd-daemon`                                   | `target/release/hpd-daemon`               |
| `/usr/local/bin/hpdctl`                                       | `target/release/hpdctl`                   |
| `/etc/systemd/system/hpd.service`                             | `package/hpd.service`                     |
| `/etc/dbus-1/system.d/dev.cirodev.hpd.conf`                   | `package/dev.cirodev.hpd.conf`            |
| `/usr/share/polkit-1/actions/dev.cirodev.hpd.policy`          | `package/polkit/dev.cirodev.hpd.policy`   |
| `/usr/share/polkit-1/rules.d/49-hpd.rules`                    | `package/polkit/49-hpd.rules`             |
| `/etc/hpd/config.toml.example`                                | `package/hpd-example.toml`                |
| `/var/lib/hpd/` *(empty directory created at install time)*   | — (state file appears on first run)       |

> `install.sh` deliberately **never overwrites** an existing
> `/etc/hpd/config.toml`. Operators edit that file freely; the example
> at `config.toml.example` is the reference.

### 3b. Iterative dev loop: `cargo run` against system bus

For tighter dev cycles you can skip `install.sh` and run the binary
straight out of `target/`. You still need the polkit policy and D-Bus
policy installed at least once (the polkit Authority looks them up by
absolute path).

```bash
# one-time: install only the policy files (no binary copy, no service enable)
sudo install -Dm644 package/dev.cirodev.hpd.conf \
    /etc/dbus-1/system.d/dev.cirodev.hpd.conf
sudo install -Dm644 package/polkit/dev.cirodev.hpd.policy \
    /usr/share/polkit-1/actions/dev.cirodev.hpd.policy
sudo install -Dm644 package/polkit/49-hpd.rules \
    /usr/share/polkit-1/rules.d/49-hpd.rules
sudo systemctl try-reload-or-restart dbus.service

# dev loop:
sudo systemctl stop hpd.service 2>/dev/null || true
sudo RUST_LOG=hpd=debug cargo run -p hpd-daemon
# in another shell:
hpdctl status
```

`sudo` is required because the daemon writes to
`/sys/class/firmware-attributes/asus-armoury/attributes/...` and the
charge-threshold sysfs, which need `CAP_DAC_OVERRIDE` (root).

---

## 4. Logging

The daemon uses `tracing` + `tracing-subscriber` with an
`EnvFilter` driven by `RUST_LOG`. The shipped unit sets
`RUST_LOG=hpd=info,warn`; override at the command line for dev.

| Verbosity                                       | What to use                                |
|-------------------------------------------------|--------------------------------------------|
| Only warnings + errors                          | `RUST_LOG=warn cargo run -p hpd-daemon`    |
| Default unit setting                            | `RUST_LOG=hpd=info,warn …`                 |
| Verbose state-machine traces                    | `RUST_LOG=hpd=debug …`                     |
| Trace polkit + zbus internals too               | `RUST_LOG=hpd=trace,zbus=debug …`          |

When the daemon is running as a service:

```bash
journalctl -fu hpd                  # follow live
journalctl -u hpd --since '5 min ago'
journalctl -u hpd -p err            # errors only
```

---

## 5. D-Bus introspection & manual calls

The interface name is `dev.cirodev.hpd.PowerDaemon1`, object path
`/dev/cirodev/hpd/PowerDaemon1`, on the **system bus**.

```bash
# introspect the interface
busctl introspect dev.cirodev.hpd.PowerDaemon1 /dev/cirodev/hpd/PowerDaemon1

# read a property
busctl get-property dev.cirodev.hpd.PowerDaemon1 \
    /dev/cirodev/hpd/PowerDaemon1 \
    dev.cirodev.hpd.PowerDaemon1 \
    current_spl

# call a privileged setter (will trigger a polkit prompt)
busctl call dev.cirodev.hpd.PowerDaemon1 \
    /dev/cirodev/hpd/PowerDaemon1 \
    dev.cirodev.hpd.PowerDaemon1 \
    SetSpl u 15

# watch PropertiesChanged signals
dbus-monitor --system "interface='org.freedesktop.DBus.Properties'" \
    | grep -A 3 dev.cirodev.hpd
```

`hpdctl` is the human-friendly wrapper; `busctl`/`dbus-monitor` are
what you reach for when something looks off in the wire format.

---

## 6. Polkit interactions

The three action IDs and their **non-administrator** defaults:

| Action                          | Used by                              | Default rule       |
|---------------------------------|--------------------------------------|--------------------|
| `dev.cirodev.hpd.set-tdp`       | `set_spl`, `set_preset`              | `auth_admin`       |
| `dev.cirodev.hpd.set-charge`    | `set_charge_threshold`               | `auth_admin`       |
| `dev.cirodev.hpd.set-profile`   | `set_profile`, `set_fan_auto`        | `auth_admin_keep`  |

`wheel`-group members (the device owner) bypass the table above:
`package/polkit/49-hpd.rules` grants every `dev.cirodev.hpd.*` action
to `wheel` without a prompt, keyed on group membership rather than the
session's local/active classification. That classification is the usual
gotcha on dev hosts — a physically-local terminal often reports
`Remote=yes` (e.g. when you're SSH'd in, or your DM doesn't attach the
session to the seat), so the policy's `allow_active` tier never fires
and only the `wheel` rule lets you in without a password. Check your
session with `loginctl show-session "$XDG_SESSION_ID" -p Remote -p Active`.

Useful invocations:

```bash
# inspect the registered policy
pkaction --action-id dev.cirodev.hpd.set-tdp --verbose

# confirm you are in wheel (the passwordless grant keys on this)
id -nG | tr ' ' '\n' | grep -qx wheel && echo "in wheel" || echo "NOT in wheel"

# clear cached admin credentials (e.g. set-profile keeps them 5 min)
sudo systemctl restart polkit.service

# test from an unprivileged shell whether you would be allowed
pkcheck --action-id dev.cirodev.hpd.set-tdp --process $$ --allow-user-interaction
```

If a **non-`wheel`** user gets no prompt at all in a desktop session:
confirm a polkit agent is running (`pgrep polkit-` should show one).
KDE, GNOME, sway, Hyprland all ship one by default; tiling-WM users
sometimes need to start one manually
(e.g. `/usr/lib/polkit-kde-authentication-agent-1`). `wheel` members
need no agent — the rule authorizes them directly.

---

## 7. Suspend / resume testing

The daemon listens for `org.freedesktop.login1.Manager`'s
`PrepareForSleep(false)` signal (resume edge) and re-applies the
envelope + profile + charge threshold. Trigger one manually:

```bash
systemctl suspend
# laptop suspends, then wake it with the power button.
# in the journal you should see:
#   "System resumed: re-applying envelope + profile + charge threshold"
journalctl -u hpd --since '1 min ago' | grep -i resume
```

---

## 8. AC plug / unplug testing

`hpd-netlink` listens to udev `power_supply` events. Plug/unplug
the charger; the journal should show:

```
INFO hpd_netlink: ⚡ Hardware event detected: Charger connected = true
```

If you don't see it: the udev subsystem name might be unusual on
your distro. The crate matches on names containing `AC` or `ADP`
(case-insensitive). Custom kernel? Inspect with:

```bash
udevadm monitor --subsystem-match=power_supply --property
```

---

## 9. Filesystem layout reference

| Path                                        | Owner       | Purpose                                  |
|---------------------------------------------|-------------|------------------------------------------|
| `/etc/hpd/config.toml`                      | operator    | Operator config (optional, all defaults).|
| `/etc/hpd/config.toml.example`              | install.sh  | Reference template.                      |
| `/var/lib/hpd/state.toml`                   | daemon      | Persisted in-memory state (atomic write).|
| `/etc/systemd/system/hpd.service`           | install.sh  | Sandboxed unit.                          |
| `/etc/dbus-1/system.d/dev.cirodev.hpd.conf` | install.sh  | Bus-level send/receive policy.           |
| `/usr/share/polkit-1/actions/dev.cirodev.hpd.policy` | install.sh | Polkit action definitions (non-admin defaults). |
| `/usr/share/polkit-1/rules.d/49-hpd.rules`  | install.sh  | Polkit rule: `wheel` passwordless grant. |
| `/usr/local/bin/{hpd-daemon,hpdctl}`        | install.sh  | Shipped binaries.                        |

The systemd unit injects `STATE_DIRECTORY=/var/lib/hpd` and
`CONFIGURATION_DIRECTORY=/etc/hpd` at runtime; the daemon prefers
those over the configured `state_path` when running under systemd.

---

## 10. Common pitfalls

- **"Cannot connect to D-Bus"** when running `hpdctl` — the daemon
  isn't running, or you're trying to talk to the system bus from a
  user session without the polkit/D-Bus policy installed. Check
  `systemctl status hpd` and reinstall the policies if needed.
- **"Failed to write to /sys/..."** — daemon is running unprivileged
  (without root). Either run it under systemd (which always runs as
  root) or `sudo cargo run` during development.
- **`tokio-udev` link error during build** — missing `libudev-dev`.
  See the distro hints in §1.
- **Polkit prompt never appears** in a custom WM — for a non-`wheel`
  user this means no authentication agent is running; start one
  explicitly (see §6). For a `wheel` user no prompt is expected — the
  `49-hpd.rules` grant authorizes them directly.
- **`hpdctl` write fails with `AuthFailed` even though you are the
  owner** — your session is likely `Remote=yes` (common over SSH), so
  the policy's `allow_active` tier never applies. Make sure you are in
  the `wheel` group (`id -nG | grep -qw wheel`) and that
  `/usr/share/polkit-1/rules.d/49-hpd.rules` is installed; the rule
  grants `wheel` regardless of session locality. See §6.
- **State file appears to disappear after reboot** — you ran the
  daemon outside systemd, persistence went to the configured
  `state_path` instead of `STATE_DIRECTORY`. Check which path was
  active at startup with `RUST_LOG=hpd=debug`.
- **Tests pass locally but fail in CI** — almost always a `clippy`
  warning. CI uses `-D warnings`; run
  `cargo clippy --workspace --all-targets -- -D warnings` locally.

---

## 11. Where to next

- Architecture deep-dive: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- Per-crate docs: `crates/<name>/README.md`
- Contribution rules: [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md)
- macOS-side workflow (simulator): [`MACOS.md`](MACOS.md)
