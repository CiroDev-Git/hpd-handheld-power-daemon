#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Preflight diagnostic for hpd-handheld-power-daemon.
#
# Runs every prerequisite check install.sh assumes (Linux + x86_64,
# sudo, the full Rust toolchain at MSRV, systemd, D-Bus, polkit, a
# C linker for transitive build deps) and probes the DMI surface so
# the operator knows up front whether their handheld is one of the
# supported ASUS boards. Reports pass / warn / fail per check with
# remediation hints, then a final summary.
#
# Usage:
#   ./scripts/doctor.sh             # full report
#   ./scripts/doctor.sh --quiet     # only warnings + failures + summary
#   ./scripts/doctor.sh --strict    # warnings become failures
#
# Exit codes:
#   0   all clear (or only warnings without --strict)
#   1   at least one failure (or any warning under --strict)
#   2   wrong invocation (unknown flag)
#
# Intentionally standalone: no dependency on cargo, hpd binaries, or
# anything outside coreutils + bash so it can run on a fresh handheld
# install before any of the project's bits land on the system.

set -uo pipefail

MSRV="1.85.0"

QUIET=0
STRICT=0

usage() {
    cat <<EOF
Usage: $0 [--quiet] [--strict]

  --quiet    Only print warnings, failures, and the final summary.
  --strict   Treat warnings as failures (useful in CI).
  -h, --help Show this help.

Reports prerequisites for running ./install.sh on this host.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --quiet)   QUIET=1 ;;
        --strict)  STRICT=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown flag: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

# Color only when stdout is a TTY and tput is around. Stays empty on
# non-interactive runs (CI, install.sh capturing output) so log files
# don't fill with escape codes.
if [[ -t 1 ]] && command -v tput >/dev/null 2>&1 && [[ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]]; then
    C_GREEN="$(tput setaf 2)"
    C_YEL="$(tput setaf 3)"
    C_RED="$(tput setaf 1)"
    C_DIM="$(tput dim)"
    C_BOLD="$(tput bold)"
    C_RST="$(tput sgr0)"
else
    C_GREEN=""; C_YEL=""; C_RED=""; C_DIM=""; C_BOLD=""; C_RST=""
fi

PASS=0
WARN=0
FAIL=0

section() {
    (( QUIET )) && return 0
    printf "\n%s== %s ==%s\n" "$C_BOLD" "$1" "$C_RST"
}

ok() {
    PASS=$((PASS + 1))
    (( QUIET )) && return 0
    printf "  %s✓%s %s\n" "$C_GREEN" "$C_RST" "$1"
}

warn() {
    WARN=$((WARN + 1))
    printf "  %s!%s %s\n" "$C_YEL" "$C_RST" "$1"
    [[ -n "${2-}" ]] && printf "    %s→ %s%s\n" "$C_DIM" "$2" "$C_RST"
}

fail() {
    FAIL=$((FAIL + 1))
    printf "  %s✗%s %s\n" "$C_RED" "$C_RST" "$1"
    [[ -n "${2-}" ]] && printf "    %s→ %s%s\n" "$C_DIM" "$2" "$C_RST"
}

# Compare two dotted versions; return 0 iff $1 >= $2. Uses `sort -V`
# which is available in GNU coreutils (ships everywhere we target).
ver_ge() {
    [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]
}

# Read a single DMI attribute file, trimmed. Empty string if missing
# or unreadable (no error: not every host exposes every key).
read_dmi() {
    local f="/sys/class/dmi/id/$1"
    [[ -r "$f" ]] || { printf ''; return 0; }
    tr -d '\n' < "$f"
}

# ----------------------------------------------------------------------
# 1. Platform — OS, architecture, kernel
# ----------------------------------------------------------------------
section "Platform"

os="$(uname -s)"
if [[ "$os" == "Linux" ]]; then
    ok "Operating system: Linux ($(uname -r))"
else
    fail "Operating system: $os" \
         "hpd is a Linux daemon. On macOS / other hosts use the simulator: cargo run -p hpd-daemon --features simulator."
fi

arch="$(uname -m)"
if [[ "$arch" == "x86_64" ]]; then
    ok "Architecture: x86_64"
else
    fail "Architecture: $arch (only x86_64 is supported today)" \
         "Supported handhelds (ROG Ally family) are all x86_64. Other targets aren't built or tested."
fi

# ----------------------------------------------------------------------
# 2. Distro — informational; install.sh works on any systemd distro
# ----------------------------------------------------------------------
section "Distribution"

if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    distro_id="${ID:-unknown}"
    distro_like="${ID_LIKE:-}"
    distro_name="${PRETTY_NAME:-$distro_id}"

    case " $distro_id $distro_like " in
        *" arch "*|*" cachyos "*|*" endeavouros "*|*" manjaro "*)
            ok "Detected distro: $distro_name (Arch family)" ;;
        *" fedora "*|*" rhel "*|*" centos "*)
            ok "Detected distro: $distro_name (Fedora family)" ;;
        *" debian "*|*" ubuntu "*)
            ok "Detected distro: $distro_name (Debian family)" ;;
        *)
            warn "Detected distro: $distro_name — not in the tested list" \
                 "install.sh should still work on any systemd-based distro; report issues if it doesn't." ;;
    esac
else
    warn "Cannot read /etc/os-release" "Distro detection skipped — install.sh proceeds blind."
fi

# ----------------------------------------------------------------------
# 3. Privilege — sudo is required by install.sh for every write
# ----------------------------------------------------------------------
section "Privilege"

if [[ "$EUID" -eq 0 ]]; then
    ok "Running as root"
elif command -v sudo >/dev/null 2>&1; then
    ok "Found sudo at $(command -v sudo)"
    # We deliberately do NOT call `sudo -n true` here: it can flood the
    # operator's terminal with auth-failure log lines and isn't useful
    # — install.sh will prompt for the password the normal way.
else
    fail "Neither root nor sudo available" \
         "Re-run as root, or install sudo and add your user to the sudoers group."
fi

# ----------------------------------------------------------------------
# 4. Rust toolchain — the most common reason install.sh fails today
# ----------------------------------------------------------------------
section "Rust toolchain (MSRV $MSRV)"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo not found in PATH" \
         "Install rustup (recommended, handles future MSRV bumps):
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain $MSRV
        source \"\$HOME/.cargo/env\"
    Or via your distro:
        Arch / CachyOS / EndeavourOS:  sudo pacman -S rustup && rustup default $MSRV
        Fedora:                         sudo dnf install rust cargo
        Debian / Ubuntu:                sudo apt install rustc cargo"
elif ! command -v rustc >/dev/null 2>&1; then
    fail "cargo present but rustc missing" \
         "Toolchain is broken; reinstall: rustup default $MSRV"
else
    rustc_ver="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [[ -z "$rustc_ver" ]]; then
        warn "Could not parse 'rustc --version' output" "Skipping MSRV check."
    elif ver_ge "$rustc_ver" "$MSRV"; then
        ok "rustc $rustc_ver (>= MSRV $MSRV)"
    else
        fail "rustc $rustc_ver is below MSRV $MSRV" \
             "If you use rustup: rustup default $MSRV  (or 'stable' if newer than $MSRV)"
    fi
fi

# ----------------------------------------------------------------------
# 5. System services — systemd, D-Bus, polkit
# ----------------------------------------------------------------------
section "System services"

if command -v systemctl >/dev/null 2>&1; then
    ok "systemctl found at $(command -v systemctl)"
    # The daemon registers a system bus name and is launched by systemd
    # in production. If pid 1 isn't systemd (containers, some chroots)
    # `enable --now` will refuse — warn rather than fail since the
    # operator may be intentionally inside such an environment.
    init_comm="$(ps -p 1 -o comm= 2>/dev/null | tr -d ' ')"
    if [[ "$init_comm" == "systemd" ]]; then
        ok "pid 1 is systemd"
    else
        warn "pid 1 is '$init_comm', not systemd" \
             "install.sh will install the unit but 'systemctl enable --now hpd.service' may fail outside a systemd init."
    fi
else
    fail "systemctl not found" \
         "hpd ships a systemd unit and depends on it for lifecycle management."
fi

if command -v dbus-daemon >/dev/null 2>&1 || [[ -d /usr/share/dbus-1/system.d ]]; then
    ok "D-Bus system bus is available"
else
    fail "D-Bus not detected" \
         "hpd exposes dev.cirodev.hpd.PowerDaemon1 on the system bus. Install dbus and restart."
fi

if command -v pkaction >/dev/null 2>&1 || [[ -d /usr/share/polkit-1/actions ]]; then
    ok "polkit is available"
else
    warn "polkit not detected" \
         "hpd installs a polkit policy and gates every privileged setter on it. Without polkit, all hpdctl writes will be refused."
fi

# The shipped polkit rule (49-hpd.rules) grants passwordless access to
# wheel-group members. Flag it if the operator who will own the device
# isn't in wheel — they would be prompted for an admin password (or hit
# AuthFailed without an auth agent) on every hpdctl write.
doctor_user="${SUDO_USER:-$USER}"
if id -nG "$doctor_user" 2>/dev/null | tr ' ' '\n' | grep -qx wheel; then
    ok "user '$doctor_user' is in the wheel group (passwordless hpdctl writes)"
else
    warn "user '$doctor_user' is not in the wheel group" \
         "hpd grants passwordless writes to wheel members via 49-hpd.rules. Add the user (sudo usermod -aG wheel $doctor_user; re-login) or expect a polkit admin prompt on every hpdctl write."
fi

# ----------------------------------------------------------------------
# 6. Build tooling — pkg-config + a C linker for transitive deps
# ----------------------------------------------------------------------
section "Build tooling"

if command -v pkg-config >/dev/null 2>&1 || command -v pkgconf >/dev/null 2>&1; then
    ok "pkg-config / pkgconf present"
else
    warn "pkg-config not found" \
         "Some Rust crates probe system libraries via pkg-config at build time.
    Install: sudo pacman -S pkgconf  |  sudo dnf install pkgconf-pkg-config  |  sudo apt install pkg-config"
fi

if command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1; then
    ok "C linker available"
else
    warn "No C compiler / linker found (cc / gcc / clang)" \
         "Some Rust crates need a C linker. Install base build tools:
    Arch family:    sudo pacman -S base-devel
    Fedora:         sudo dnf groupinstall 'Development Tools'
    Debian:         sudo apt install build-essential"
fi

# ----------------------------------------------------------------------
# 7. Hardware — DMI probe vs hpd-backend-asus's supported board list
# ----------------------------------------------------------------------
section "Hardware (DMI)"

board_vendor="$(read_dmi board_vendor)"
board_name="$(read_dmi board_name)"
product_name="$(read_dmi product_name)"

if [[ -z "$board_vendor" && -z "$board_name" ]]; then
    warn "DMI attributes unreadable" \
         "Cannot tell what board this is. The daemon will probe again at startup."
elif [[ "$(echo "$board_vendor" | tr '[:upper:]' '[:lower:]')" == "asustek computer inc." ]]; then
    # Keep this list in sync with crates/hpd-backend-asus/src/detect.rs.
    case "$board_name" in
        RC71L)
            ok "ASUS ROG Ally detected (board $board_name)" ;;
        RC72L|RC72LA)
            ok "ASUS ROG Ally X detected (board $board_name)" ;;
        RC73XA)
            ok "ASUS ROG Xbox Ally X detected (board $board_name) — primary test target" ;;
        *)
            warn "ASUS board '$board_name' (product '$product_name') not in the supported set" \
                 "Supported boards: RC71L, RC72L, RC72LA, RC73XA. The daemon will run but no ASUS backend will own this hardware." ;;
    esac
else
    warn "Vendor '$board_vendor' is not ASUS (product '$product_name')" \
         "1.0 only ships the hpd-backend-asus backend. The daemon will start but report no hardware backend."
fi

# ----------------------------------------------------------------------
# 8. Existing install — clone vs AUR collisions
# ----------------------------------------------------------------------
section "Existing install"

has_local=0
has_aur=0
[[ -e /usr/local/bin/hpd-daemon || -e /usr/local/bin/hpdctl ]] && has_local=1
[[ -e /usr/bin/hpd-daemon       || -e /usr/bin/hpdctl       ]] && has_aur=1

if (( has_local && has_aur )); then
    fail "Both /usr/local/bin and /usr/bin contain hpd binaries" \
         "PATH order will shadow one with the other. Pick a single install path:
        - Keep the AUR install: ./uninstall.sh  (removes the /usr/local/bin copy)
        - Keep this clone:      sudo paru -R hpd-handheld-power-daemon{,-bin}  (or your AUR helper)"
elif (( has_aur )); then
    warn "AUR install detected at /usr/bin/hpd-daemon" \
         "install.sh will create a second copy under /usr/local/bin that shadows the AUR one. Uninstall the AUR package first if you want the clone to take over."
elif (( has_local )); then
    ok "Previous clone install at /usr/local/bin/ will be overwritten in place"
else
    ok "No existing hpd binaries on this system"
fi

if command -v systemctl >/dev/null 2>&1; then
    if systemctl is-active --quiet hpd.service 2>/dev/null; then
        warn "hpd.service is currently active" \
             "install.sh will stop it before swapping binaries, then re-enable it."
    fi
fi

if [[ -e /var/lib/hpd/state.toml ]]; then
    ok "Persisted state at /var/lib/hpd/state.toml found (will be reused)"
fi
if [[ -e /etc/hpd/config.toml ]]; then
    ok "Operator config at /etc/hpd/config.toml found (install.sh will NOT overwrite it)"
fi

# ----------------------------------------------------------------------
# Summary
# ----------------------------------------------------------------------
printf "\n%s== Summary ==%s\n" "$C_BOLD" "$C_RST"
printf "  %s%d ok%s, %s%d warning(s)%s, %s%d failure(s)%s\n" \
    "$C_GREEN" "$PASS" "$C_RST" \
    "$C_YEL"   "$WARN" "$C_RST" \
    "$C_RED"   "$FAIL" "$C_RST"

if (( FAIL > 0 )); then
    printf "  %s✗ Fix the failures above before running ./install.sh%s\n" "$C_RED" "$C_RST"
    exit 1
fi

if (( STRICT && WARN > 0 )); then
    printf "  %s✗ --strict: %d warning(s) treated as failures%s\n" "$C_RED" "$WARN" "$C_RST"
    exit 1
fi

if (( WARN > 0 )); then
    printf "  %s! Ready to install, but review the warnings above first.%s\n" "$C_YEL" "$C_RST"
else
    printf "  %s✓ System is ready: run ./install.sh%s\n" "$C_GREEN" "$C_RST"
fi
exit 0
