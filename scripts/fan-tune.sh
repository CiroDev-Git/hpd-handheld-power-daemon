#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Live fan-curve tuning helper for hpd-handheld-power-daemon.
#
# Applies a candidate 8-point curve directly to the EC (both fans) so a
# curve can be validated in a real game before it is baked into the L1
# backend presets. Reads are unprivileged; the EC writes need root.
#
# Validation protocol (isolates fan cooling from the power/profile knob):
#   1. Pin power high & constant:  hpdctl cool set aggressive  (Performance)
#   2. sudo ./fan-tune.sh apply "<8 points>"   # override the EC curve
#   3. Run the game; watch ./fan-tune.sh monitor
#   4. sudo ./fan-tune.sh restore              # hand control back to hpd
#
# Curve spec: eight space-separated TEMP:PWM points, non-decreasing in
# both, e.g. "40:45 48:90 55:135 62:180 68:220 74:255 82:255 90:255".
#
# Usage:
#   sudo ./fan-tune.sh apply "t:p t:p t:p t:p t:p t:p t:p t:p"
#   sudo ./fan-tune.sh restore
#        ./fan-tune.sh monitor [seconds]   # telemetry loop (no root)
#        ./fan-tune.sh dump                # show curve programmed now
set -euo pipefail

hw() { for d in /sys/class/hwmon/*; do [ "$(cat "$d/name" 2>/dev/null)" = "$1" ] && { echo "$d"; return; }; done; }

CURVE="$(hw asus_custom_fan_curve || true)"
RPM="$(hw asus || true)"; K="$(hw k10temp || true)"; G="$(hw amdgpu || true)"

case "${1:-}" in
  apply)
    [ "$EUID" -eq 0 ] || { echo "apply needs root: sudo $0 apply \"...\"" >&2; exit 1; }
    [ -n "$CURVE" ] || { echo "no asus_custom_fan_curve node" >&2; exit 1; }
    spec="${2:?need 8 TEMP:PWM points}"
    read -ra PTS <<< "$spec"
    [ "${#PTS[@]}" -eq 8 ] || { echo "need exactly 8 points, got ${#PTS[@]}" >&2; exit 1; }
    prevt=-1; prevp=-1
    for pt in "${PTS[@]}"; do
      t="${pt%%:*}"; p="${pt##*:}"
      [[ "$t" =~ ^[0-9]+$ && "$p" =~ ^[0-9]+$ ]] || { echo "bad point '$pt'" >&2; exit 1; }
      { [ "$t" -ge "$prevt" ] && [ "$p" -ge "$prevp" ]; } || { echo "non-monotonic at '$pt'" >&2; exit 1; }
      [ "$p" -le 255 ] || { echo "pwm>255 at '$pt'" >&2; exit 1; }
      prevt="$t"; prevp="$p"
    done
    for fan in 1 2; do
      idx=1
      for pt in "${PTS[@]}"; do
        echo "${pt%%:*}" > "$CURVE/pwm${fan}_auto_point${idx}_temp"
        echo "${pt##*:}" > "$CURVE/pwm${fan}_auto_point${idx}_pwm"
        idx=$((idx+1))
      done
      echo 1 > "$CURVE/pwm${fan}_enable"
    done
    echo "applied to CPU+GPU fans: $spec"
    ;;
  restore)
    [ "$EUID" -eq 0 ] || { echo "restore needs root: sudo $0 restore" >&2; exit 1; }
    systemctl restart hpd && echo "hpd restarted — fan curve back under hpd control"
    ;;
  dump)
    [ -n "$CURVE" ] || { echo "no curve node" >&2; exit 1; }
    for fan in 1 2; do
      lbl=CPU; [ "$fan" = 2 ] && lbl=GPU
      echo "$lbl (pwm$fan) enable=$(cat "$CURVE/pwm${fan}_enable")"
      for p in 1 2 3 4 5 6 7 8; do
        printf '  p%s %3s°C -> %3s\n' "$p" \
          "$(cat "$CURVE/pwm${fan}_auto_point${p}_temp")" \
          "$(cat "$CURVE/pwm${fan}_auto_point${p}_pwm")"
      done
    done
    ;;
  monitor)
    secs="${2:-600}"; end=$(( $(date +%s) + secs ))
    while [ "$(date +%s)" -lt "$end" ]; do
      printf 'Tctl=%s°C edge=%s°C SoC=%sW CPUfan=%s GPUfan=%s\n' \
        $(($(cat "$K/temp1_input")/1000)) \
        $(($(cat "$G/temp1_input")/1000)) \
        $(($(cat "$G/power1_input" 2>/dev/null || echo 0)/1000000)) \
        "$(cat "$RPM/fan1_input")" "$(cat "$RPM/fan2_input" 2>/dev/null || echo 0)"
      sleep 3
    done
    ;;
  *)
    echo "usage: $0 {apply \"t:p x8\" | restore | dump | monitor [secs]}" >&2; exit 1;;
esac
