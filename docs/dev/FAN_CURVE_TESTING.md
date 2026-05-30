<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# On-device test plan — custom fan curves (ROG Xbox Ally X)

Comprehensive validation for the fan-curve feature on real hardware.
Written for the **ROG Xbox Ally X (`RC73XA`)** on CachyOS, but applies to
any ASUS handheld exposing `asus_custom_fan_curve`.

The goals: (1) confirm the curve is actually written and respected by the
EC, (2) confirm the integration with profile / TDP / suspend / AC / boot
behaves, (3) collect the temp↔RPM data needed to **calibrate the preset
values** (they are a safe starting point, not a final tune).

Most steps are read-only or trivially reversible (`fan curve reset`
returns to firmware automatic). Nothing here writes raw PWM.

> Shell note: the daemon/CLI commands are shell-agnostic. The raw
> `/sys` inspection loops are written for **bash** — on fish, run them
> inside `bash <<'EOF' … EOF` (see the project notes), or paste the
> single-line variants.

---

## 0. Setup

```bash
# Build + install the feature branch
git switch feat/fan-curves
./install.sh                       # builds release, installs unit/policy/config
systemctl status hpd               # active (running)?
hpdctl --version

# Confirm the kernel exposes the curve interface and the sensors
for h in /sys/class/hwmon/hwmon*; do echo "$h -> $(cat $h/name)"; done
# Expect to see: asus, asus_custom_fan_curve, k10temp, amdgpu
```

If `asus_custom_fan_curve` is **absent**, the kernel/driver does not
support custom curves on this build — stop and report the kernel version
(`uname -r`) and `lsmod | grep asus`.

A helper to snapshot the live curve straight from sysfs (used throughout):

```bash
# save as ~/curve.sh ; run: bash ~/curve.sh
CFC=$(for h in /sys/class/hwmon/hwmon*; do \
  [ "$(cat $h/name 2>/dev/null)" = asus_custom_fan_curve ] && echo $h; done)
echo "node: $CFC"
for f in 1 2; do
  printf "pwm%s enable=%s : " "$f" "$(cat $CFC/pwm${f}_enable)"
  for p in 1 2 3 4 5 6 7 8; do
    printf "%s/%s " "$(cat $CFC/pwm${f}_auto_point${p}_temp)" \
                    "$(cat $CFC/pwm${f}_auto_point${p}_pwm)"
  done; echo
done
```

---

## 1. Telemetry sanity (status dashboard)

| Step | Command | Expect |
|---|---|---|
| 1.1 | `hpdctl status` | Shows **Fan Curve**, **Temps** (CPU/GPU °C), **Fans** (CPU/GPU RPM), all non-`n/a` |
| 1.2 | cross-check temps | dashboard CPU ≈ `cat /sys/class/hwmon/hwmonN/temp1_input` (k10temp) / 1000 |
| 1.3 | cross-check RPM | dashboard CPU/GPU ≈ `asus` node `fan1_input` / `fan2_input` (NOT `acpi_fan`) |
| 1.4 | `hpdctl monitor` | refreshes ~1 s; temps/RPM track load live |

✅ Pass = the dashboard numbers match raw sysfs. This also validates the
**RPM-read fix** (1.3 must read `asus`, not the `acpi_fan` decoy).

---

## 2. Curve write + read-back + `pwm_enable` semantics

This is the core correctness check — **does the EC actually accept and
respect our curve?**

```bash
hpdctl fan curve get                 # baseline (likely "balanced" after install)
bash ~/curve.sh                      # snapshot firmware/active points

hpdctl fan curve set aggressive
hpdctl fan curve get                 # -> aggressive
bash ~/curve.sh                      # points must match the aggressive table
```

| Check | Expect |
|---|---|
| 2.1 read-back | `pwm1` points match the aggressive CPU table (40/26 … 91/255) |
| 2.2 enable | `pwm1_enable` and `pwm2_enable` are **1** (custom active) after a set |
| 2.3 daemon log | `journalctl -u hpd -n20` shows no "Fan curve write failed" / read-back mismatch |
| 2.4 reset | `hpdctl fan curve reset` → `pwm*_enable` back to **2** (firmware auto) |

> ⚠️ **`pwm_enable` assumption to confirm here:** we assume `1 = custom`,
> `2 = automatic`. If after `set` the enable is not `1`, or the points do
> not stick, capture `bash ~/curve.sh` output and the journal — the
> backend's enable constants may need adjusting for this firmware.

**Physical confirmation:** set `aggressive`, then `silent`, and *listen* —
the fans should audibly ramp up then down within a few seconds. If the
audible behaviour does not change even though read-back is correct, the
EC may be ignoring the curve (firmware quirk) — record it.

---

## 3. Per-preset behaviour under load (calibration data)

Goal: collect the temp↔RPM response so we can finalize the preset
values. Use a sustained load (a game, or `stress-ng --cpu 0 --timeout
120s` if available).

For each preset, run the load and sample every ~10 s for ~2 min:

```bash
# one-line sampler (bash): timestamp, CPU°C, GPU°C, CPU rpm, GPU rpm
while true; do hpdctl status | awk '/Temps|Fans/{printf "%s ",$0} END{print ""}'; sleep 10; done
```

| Preset | Command | Record |
|---|---|---|
| 3.1 silent | `hpdctl fan curve set silent` + load | peak CPU °C, peak RPM, noise (subjective) |
| 3.2 balanced | `hpdctl fan curve set balanced` + load | same |
| 3.3 aggressive | `hpdctl fan curve set aggressive` + load | same |
| 3.4 firmware | `hpdctl fan curve reset` + load | same (the conservative baseline) |

**What we want to learn:**
- Does `balanced` keep the CPU meaningfully cooler than firmware
  (target: avoid sustained 87 °C)? By how many °C / dB?
- Is `aggressive` too loud, and does it actually flatten screen/back heat?
- The PWM→RPM mapping per band (so the duty values can be re-pointed).

Record the table; it drives the calibration commit.

---

## 4. ⚠️ Profile change must NOT drop the curve (preservation)

The critical integration test. Hypothesis: writing the platform profile
resets the EC's custom curve. The daemon should re-assert it.

```bash
hpdctl fan curve set aggressive
bash ~/curve.sh                      # pwm*_enable = 1, aggressive points

# Force a profile change directly:
hpdctl fan set performance
sleep 1
bash ~/curve.sh                      # pwm*_enable STILL 1, points STILL aggressive?
hpdctl fan curve get                 # still "aggressive"
```

| Check | Expect |
|---|---|
| 4.1 | after `fan set performance`, `pwm*_enable` is still **1** and points unchanged |
| 4.2 | `journalctl` shows an `ApplyFanCurve` re-apply right after the profile write |
| 4.3 TDP-driven | `hpdctl fan auto` then `hpdctl tdp set <high>` (crosses a profile threshold) → curve still active |

✅ Pass = the curve survives profile changes. **If 4.1 fails even with the
re-assert** (enable flips to 2 *and stays* 2), the firmware reset is
happening *after* our re-write — capture timing from the journal; we may
need to re-apply with a small delay or re-order the effects.

> This is the integration that makes the feature usable with the default
> `fan_follows_tdp` auto-cooling: TDP changes nudge the profile constantly,
> and the curve must not evaporate.

---

## 5. Resume from suspend re-applies the curve

The original bug: fans blast at 100 % after waking. Fix: re-apply on resume.

```bash
hpdctl fan curve set aggressive
systemctl suspend                    # or close the lid / power button
# … wake the device …
sleep 2
bash ~/curve.sh                      # enable=1, aggressive points restored?
hpdctl fan curve get                 # "aggressive"
hpdctl status                        # fans sane, not pinned at max
```

| Check | Expect |
|---|---|
| 5.1 | after resume, `pwm*_enable = 1` and points = aggressive |
| 5.2 | fans are NOT stuck at full speed |
| 5.3 journal | a `SystemResumed` → `ApplyFanCurve` sequence after wake |

---

## 6. `fan_curve_follows_profile` (hybrid mode)

```bash
sudoedit /etc/hpd/config.toml        # set: fan_curve_follows_profile = true
systemctl reload hpd                 # SIGHUP hot-reload (no restart)

hpdctl fan set power-saver  ; hpdctl fan curve get   # -> silent
hpdctl fan set balanced     ; hpdctl fan curve get   # -> balanced
hpdctl fan set performance  ; hpdctl fan curve get   # -> aggressive
```

| Check | Expect |
|---|---|
| 6.1 | each profile change swaps the matching curve preset |
| 6.2 | with follows on, `fan curve reset` is overridden on the next profile change |
| 6.3 | revert config + `systemctl reload hpd` → manual mode restored |

---

## 7. AC plug/unplug chain

With auto-cooling + follows on, plugging AC ramps TDP→max→performance→aggressive.

| Step | Action | Expect |
|---|---|---|
| 7.1 | ensure `fan auto` + `fan_curve_follows_profile=true` | — |
| 7.2 | plug AC | TDP→max, profile→performance, curve→aggressive (`hpdctl status`) |
| 7.3 | unplug AC | TDP restores DC target, profile/curve follow back down |
| 7.4 | with follows **off** + a manual curve set | AC plug changes TDP/profile but the manual curve is **preserved** (re-asserted, not changed) |

---

## 8. Persistence across restart & reboot

```bash
hpdctl fan curve set aggressive
sudo systemctl restart hpd
hpdctl fan curve get                 # still aggressive (from state.toml)
bash ~/curve.sh                      # re-written to EC on startup (enable=1)
grep -i fan /var/lib/hpd/state.toml  # active_fan_curve persisted
```

| Check | Expect |
|---|---|
| 8.1 | active curve survives a daemon restart and is re-applied to the EC |
| 8.2 | survives a full **reboot** |
| 8.3 first-boot default | wipe state (`sudo rm /var/lib/hpd/state.toml`), restart → curve = `balanced` (the configured default) applied to the EC |
| 8.4 | set `default_fan_curve = ""` in config, wipe state, restart → curve = `auto` (firmware), nothing written |

---

## 9. Authorization (polkit)

| Step | Action | Expect |
|---|---|---|
| 9.1 | as a `wheel` user (device owner), `hpdctl fan curve set balanced` | **no password prompt** (49-hpd.rules grant) |
| 9.2 | over SSH as the same wheel user | still no prompt (the rule keys on group, not session tier) |
| 9.3 | as a non-wheel user | prompted to authenticate (`auth_admin_keep`); cached ~5 min after |
| 9.4 | `pkaction --action-id dev.cirodev.hpd.set-fan-curve` | action is registered |

---

## 10. Edge cases & failure handling

| Step | Action | Expect |
|---|---|---|
| 10.1 | `hpdctl fan curve set turbo` | clean error: "unknown fan curve preset 'turbo'", no state change |
| 10.2 | `hpdctl fan curve reset` when already auto | no-op, no error |
| 10.3 | `hpdctl fan curve set balanced` ×2 | second is a no-op (already active), no redundant write |
| 10.4 | kill the daemon mid-session (`sudo pkill -9 hpd-daemon`) while a curve is active | fans keep following the last curve (EC-mediated); `systemctl start hpd` re-applies |
| 10.5 | `journalctl -u hpd -p warning` after the full run | no unexpected warnings/errors |

---

## Results template

Copy, fill, and attach to the PR:

```
Device: ROG Xbox Ally X RC73XA   Kernel: ____   hpd: feat/fan-curves @ ____

§1 telemetry matches sysfs ......... PASS / FAIL  notes:
§2 write + read-back ............... PASS / FAIL  pwm_enable on set = ___ (expect 1)
§2 audible ramp up/down ............ PASS / FAIL
§4 curve survives profile change ... PASS / FAIL  enable after fan set = ___
§5 curve restored on resume ........ PASS / FAIL
§6 follows_profile ................. PASS / FAIL
§7 AC chain ........................ PASS / FAIL
§8 persistence + first-boot default  PASS / FAIL
§9 polkit (wheel passwordless) ..... PASS / FAIL
§10 edge cases ..................... PASS / FAIL

Calibration data (peak under sustained load):
            CPU°C  GPU°C  CPU rpm  GPU rpm  noise
silent      ___    ___    ___      ___      ___
balanced    ___    ___    ___      ___      ___
aggressive  ___    ___    ___      ___      ___
firmware    ___    ___    ___      ___      ___

Verdict on presets: too quiet / about right / too loud — proposed tweaks:
```

The calibration table is what turns the current "safe starting point"
presets into final, measured values.
