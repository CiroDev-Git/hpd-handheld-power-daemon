#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Fan characterization for hpd-handheld-power-daemon.
#
# Measures, for the current handheld, the real PWM-duty -> RPM response
# of each fan and the duty at which the fan turns OFF / hits its minimum
# spinning floor. hpd's Silent/Balanced/Aggressive presets are expressed
# in PWM duty (0-255), but the *audible* result depends on each unit's
# fan floor, which is not documented and varies. This builds the table
# needed to tune the presets from data instead of guesswork.
#
# How it works (and why it is safe):
#   * It programs FLAT curves (all 8 auto-points at the same duty) through
#     the EC-mediated `asus_custom_fan_curve` node — never raw PWM. The EC
#     keeps closing the loop, so a crash/kill leaves the fans on the last
#     curve, not frozen.
#   * It restores hpd's own curve on exit (any exit) by restarting hpd.
#   * It aborts immediately if CPU (Tctl) exceeds the safety ceiling, so a
#     low-duty step can never cook the chip.
#
# Run it IDLE (close games) for a clean floor reading.
#
# Usage:
#   sudo ./scripts/fan-characterize.sh                 # default sweep
#   sudo ./scripts/fan-characterize.sh --dump          # just print the
#                                                      # curve hpd has
#                                                      # programmed now
#   sudo SETTLE=10 CEILING=80 ./scripts/fan-characterize.sh
#
# Env knobs:
#   SETTLE   seconds to wait for RPM to settle per step   (default 8)
#   CEILING  abort if Tctl °C exceeds this                (default 85)
#   DUTIES   space-separated PWM steps to test    (default "0 20 40 60 80 100 128 160 200 255")

set -euo pipefail

SETTLE="${SETTLE:-8}"
CEILING="${CEILING:-85}"
DUTIES="${DUTIES:-0 20 40 60 80 100 128 160 200 255}"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }

hwmon_by_name() {
  local want="$1" d
  for d in /sys/class/hwmon/*; do
    [[ "$(cat "$d/name" 2>/dev/null)" == "$want" ]] && { echo "$d"; return 0; }
  done
  return 1
}

CURVE="$(hwmon_by_name asus_custom_fan_curve)" || { echo "no asus_custom_fan_curve node" >&2; exit 1; }
RPM="$(hwmon_by_name asus)"                     || { echo "no asus (RPM) node" >&2; exit 1; }
KTEMP="$(hwmon_by_name k10temp)"                || { echo "no k10temp node" >&2; exit 1; }

tctl()    { echo $(( $(cat "$KTEMP/temp1_input") / 1000 )); }
cpu_rpm() { cat "$RPM/fan1_input" 2>/dev/null || echo 0; }
gpu_rpm() { cat "$RPM/fan2_input" 2>/dev/null || echo 0; }

dump_curve() {
  local fan p t pw
  for fan in 1 2; do
    local label="CPU"; [[ $fan == 2 ]] && label="GPU"
    printf '  %s fan (pwm%s)  enable=%s\n' "$label" "$fan" "$(cat "$CURVE/pwm${fan}_enable")"
    for p in 1 2 3 4 5 6 7 8; do
      t="$(cat "$CURVE/pwm${fan}_auto_point${p}_temp" 2>/dev/null || echo '?')"
      pw="$(cat "$CURVE/pwm${fan}_auto_point${p}_pwm"  2>/dev/null || echo '?')"
      printf '     point%s  %3s°C -> pwm %3s  (%d%%)\n' "$p" "$t" "$pw" $(( pw * 100 / 255 ))
    done
  done
}

if [[ "${1:-}" == "--dump" ]]; then
  echo "== Fan curve hpd has programmed right now =="
  dump_curve
  echo
  echo "  live: Tctl $(tctl)°C   CPU $(cpu_rpm) RPM   GPU $(gpu_rpm) RPM"
  exit 0
fi

restore() {
  echo
  echo ">> Restoring hpd's fan curve (systemctl restart hpd)..."
  systemctl restart hpd 2>/dev/null || echo "   (could not restart hpd — run: sudo systemctl restart hpd)"
}
trap restore EXIT

# Flat-curve temps: strictly increasing so the EC accepts the points;
# the duty is identical at every point, so the fan runs at that duty
# regardless of temperature.
TEMPS=(30 40 50 60 70 80 85 90)

write_flat() {
  local duty="$1" fan p
  for fan in 1 2; do
    for p in 1 2 3 4 5 6 7 8; do
      echo "${TEMPS[$((p-1))]}" > "$CURVE/pwm${fan}_auto_point${p}_temp"
      echo "$duty"              > "$CURVE/pwm${fan}_auto_point${p}_pwm"
    done
    echo 1 > "$CURVE/pwm${fan}_enable"   # 1 = custom curve
  done
}

echo "== Fan characterization =="
echo "   settle=${SETTLE}s  ceiling=${CEILING}°C  duties: ${DUTIES}"
echo "   (run idle for a clean floor reading; Ctrl-C restores hpd)"
echo
printf '   %-6s %-8s %-9s %-9s %-6s\n' "PWM" "duty%" "CPU-RPM" "GPU-RPM" "Tctl"
printf '   %-6s %-8s %-9s %-9s %-6s\n' "----" "-----" "-------" "-------" "----"

for duty in $DUTIES; do
  write_flat "$duty"
  # settle, polling temp so we bail fast if it climbs
  for _ in $(seq "$SETTLE"); do
    sleep 1
    t="$(tctl)"
    if (( t > CEILING )); then
      echo
      echo "!! Tctl ${t}°C > ${CEILING}°C ceiling — aborting sweep."
      exit 1
    fi
  done
  printf '   %-6s %-8s %-9s %-9s %-6s\n' \
    "$duty" "$(( duty * 100 / 255 ))%" "$(cpu_rpm)" "$(gpu_rpm)" "$(tctl)°C"
done

echo
echo "Done. Paste the table back so we can tune the presets to your fans."
