<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# On-device test plan — custom fan curves (ROG Xbox Ally X)

Comprehensive validation for the fan-curve feature on real hardware.
Written for the **ROG Xbox Ally X (`RC73XA`)** on CachyOS, but applies to
any ASUS handheld exposing `asus_custom_fan_curve`.

The goals: (1) confirm the curve is actually written and respected by the
EC, (2) confirm the integration with profile / TDP / suspend / AC / boot
behaves, (3) collect the temp↔RPM data needed to **calibrate the preset
values** (they are a safe starting point, not a final tune).

Most steps are read-only or trivially reversible (`cool reset`
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
hpdctl cool get                 # baseline (likely "balanced" after install)
bash ~/curve.sh                      # snapshot firmware/active points

hpdctl cool set aggressive
hpdctl cool get                 # -> aggressive
bash ~/curve.sh                      # points must match the aggressive table
```

| Check | Expect |
|---|---|
| 2.1 read-back | `pwm1` points match the aggressive CPU table (40/26 … 91/255) |
| 2.2 enable | `pwm1_enable` and `pwm2_enable` are **1** (custom active) after a set |
| 2.3 daemon log | `journalctl -u hpd -n20` shows no "Fan curve write failed" / read-back mismatch |
| 2.4 reset | `hpdctl cool reset` → `pwm*_enable` back to **2** (firmware auto) |

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

For each level, run a sustained load and sample temps + RPM. The repo
ships a ready sampler — `docs/dev/cooling-sample.fish` — that reads the
sensors straight from sysfs and prints the PEAK at the end:

```fish
# terminal A — sustained all-core load:
for i in (seq 8); yes >/dev/null & ; end
# terminal B — set a level, then sample (repeat per level):
hpdctl cool set silent     ; fish docs/dev/cooling-sample.fish
hpdctl cool set balanced   ; fish docs/dev/cooling-sample.fish
hpdctl cool set aggressive ; fish docs/dev/cooling-sample.fish
hpdctl cool reset          ; fish docs/dev/cooling-sample.fish   # firmware baseline
# stop the load:
kill (jobs -p)
```

| Preset | Command | Record |
|---|---|---|
| 3.1 silent | `hpdctl cool set silent` + load | peak CPU °C, peak RPM, noise (subjective) |
| 3.2 balanced | `hpdctl cool set balanced` + load | same |
| 3.3 aggressive | `hpdctl cool set aggressive` + load | same |
| 3.4 firmware | `hpdctl cool reset` + load | same (the conservative baseline) |

**What we want to learn:**
- Does `balanced` keep the CPU meaningfully cooler than firmware
  (target: avoid sustained 87 °C)? By how many °C / dB?
- Is `aggressive` too loud, and does it actually flatten screen/back heat?
- The PWM→RPM mapping per band (so the duty values can be re-pointed).

Record the table; it drives the calibration commit.

---

## 4. ⚠️ The curve must survive a profile change (preservation)

Two things: **(a)** does an *external* profile change reset the EC's
curve (the hypothesis behind the re-assert), and **(b)** does `hpd` keep
the curve alive across its own profile changes?

**(a) Probe the EC directly, bypassing `hpd`** (the raw profile is no
longer a CLI command, so write the sysfs node by hand):

```bash
hpdctl cool set aggressive
bash ~/curve.sh                                          # enable=1, aggressive
echo balanced | sudo tee /sys/firmware/acpi/platform_profile
sleep 1
bash ~/curve.sh                                          # did pwm*_enable flip to 2?
```

**(b) `hpd`'s own path keeps profile + curve consistent:**

```bash
hpdctl cool set aggressive    # performance profile + aggressive curve
hpdctl cool set silent        # hpd writes power-saver + silent
bash ~/curve.sh               # enable=1 with the SILENT points (not dropped)
hpdctl cool auto; hpdctl tdp set 8; sleep 1; bash ~/curve.sh   # curve followed the inferred profile
```

| Check | Expect |
|---|---|
| 4.1 (a) | if `pwm*_enable` flips to **2** after the external sysfs write → the EC **does** reset the curve on a profile change → this is exactly why `hpd` re-asserts. If it stays **1**, the re-assert is harmless insurance. |
| 4.2 (b) | after `hpd`'s own profile changes (`cool set`, or `cool auto` + `tdp set`), the curve is always `enable=1` with the expected points |
| 4.3 | `journalctl -u hpd` shows an `ApplyFanCurve` right after each `ApplyPlatformProfile` |

> This is the integration that makes the feature usable with the default
> auto-cooling: TDP changes nudge the profile constantly, and the curve
> must not evaporate.

---

## 5. Resume from suspend re-applies the curve

The original bug: fans blast at 100 % after waking. Fix: re-apply on resume.

```bash
hpdctl cool set aggressive
systemctl suspend                    # or close the lid / power button
# … wake the device …
sleep 2
bash ~/curve.sh                      # enable=1, aggressive points restored?
hpdctl cool get                 # "aggressive"
hpdctl status                        # fans sane, not pinned at max
```

| Check | Expect |
|---|---|
| 5.1 | after resume, `pwm*_enable = 1` and points = aggressive |
| 5.2 | fans are NOT stuck at full speed |
| 5.3 journal | a `SystemResumed` → `ApplyFanCurve` sequence after wake |

---

## 6. Profile ↔ curve coupling (the default)

`fan_curve_follows_profile` is **on by default**, so a cooling level
moves the platform profile and the fan curve together. Verify both move:

```bash
# `cool set` moves both at once:
hpdctl cool set silent     ; cat /sys/firmware/acpi/platform_profile ; hpdctl cool get  # power-saver / silent
hpdctl cool set aggressive ; cat /sys/firmware/acpi/platform_profile ; hpdctl cool get  # performance / aggressive

# auto mode: the curve follows the TDP-inferred profile
hpdctl cool auto
hpdctl tdp set 8  ; sleep 1 ; cat /sys/firmware/acpi/platform_profile ; hpdctl cool get  # low  → power-saver / silent
hpdctl tdp set 30 ; sleep 1 ; cat /sys/firmware/acpi/platform_profile ; hpdctl cool get  # high → performance / aggressive
```

| Check | Expect |
|---|---|
| 6.1 | `cool set <level>` moves both the `platform_profile` and the curve |
| 6.2 | in auto mode, a TDP change that crosses a threshold re-infers the profile *and* the curve together |
| 6.3 (advanced) | with `fan_curve_follows_profile = false` (config), a raw D-Bus `set_profile` leaves the curve unchanged |

---

## 7. AC plug/unplug chain

With auto-cooling + follows on, plugging AC ramps TDP→max→performance→aggressive.

| Step | Action | Expect |
|---|---|---|
| 7.1 | ensure `cool auto` + `fan_curve_follows_profile=true` (default) | — |
| 7.2 | plug AC | TDP→max, profile→performance, curve→aggressive (`hpdctl status`) |
| 7.3 | unplug AC | TDP restores DC target, profile/curve follow back down |
| 7.4 | with follows **off** + a manual curve set | AC plug changes TDP/profile but the manual curve is **preserved** (re-asserted, not changed) |

---

## 8. Persistence across restart & reboot

```bash
hpdctl cool set aggressive
sudo systemctl restart hpd
hpdctl cool get                 # still aggressive (from state.toml)
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
| 9.1 | as a `wheel` user (device owner), `hpdctl cool set balanced` | **no password prompt** (49-hpd.rules grant) |
| 9.2 | over SSH as the same wheel user | still no prompt (the rule keys on group, not session tier) |
| 9.3 | as a non-wheel user | prompted to authenticate (`auth_admin_keep`); cached ~5 min after |
| 9.4 | `pkaction --action-id dev.cirodev.hpd.set-fan-curve` | action is registered |

---

## 10. Edge cases & failure handling

| Step | Action | Expect |
|---|---|---|
| 10.1 | `hpdctl cool set turbo` | clean error: "unknown fan curve preset 'turbo'", no state change |
| 10.2 | `hpdctl cool reset` when already auto | no-op, no error |
| 10.3 | `hpdctl cool set balanced` ×2 | second is a no-op (already active), no redundant write |
| 10.4 | kill the daemon mid-session (`sudo pkill -9 hpd-daemon`) while a curve is active | fans keep following the last curve (EC-mediated); `systemctl start hpd` re-applies |
| 10.5 | `journalctl -u hpd -p warning` after the full run | no unexpected warnings/errors |

---

## 11. ⚖️ Does `platform_profile` still do anything? (decides if we keep it)

Once `hpd` controls the TDP (SPL/SPPT/FPPT) *and* the fan curve directly,
the ACPI `platform_profile` (driven by **amd_pmf**) might be redundant —
or it might silently **gate** the real power the chip is allowed to draw.
This isolation test settles it.

> **Result on ROG Xbox Ally X (RC73XA), 2026-05-29 — DECISIVE: keep it.**
> Same all-core load, same `hpdctl tdp set 30`, same fan curve, only the
> profile changed: `power-saver` → CPU **59 °C**, `performance` → CPU
> **95 °C** (a **36 °C** swing). The profile gates the real power draw, so
> it is the dominant performance/thermal lever — **not** removable. `hpd`
> couples it to the fan curve under `cool`.

No config change is needed — Test B varies the profile directly and the
fan curve is irrelevant to the power measurement.

**Test A — does writing the profile move the limit `hpd` set? (quick)**

```fish
set PPT /sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value
hpdctl tdp set 15; sleep 1
echo "after tdp 15:        "(cat $PPT)
echo power-saver | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 1; echo "after power-saver:   "(cat $PPT)
echo performance | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 1; echo "after performance:   "(cat $PPT)
```

On RC73XA all three read `15`: the asus-armoury attribute is untouched, so
Test A alone is **inconclusive** (amd_pmf gates power via the SMU, not via
this file). Test B is the decisive one.

**Test B — does the requested TDP actually take effect? (decisive)**

```fish
# Terminal 1 — sustained all-core load (kill to stop):
for i in (seq 8); yes >/dev/null & ; end
# or, if installed: stress-ng --cpu 0 --timeout 120s

# Terminal 2 — fixed high TDP, compare sustained CPU temp per profile:
hpdctl tdp set 30
echo power-saver | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 25; hpdctl status | grep Temps
echo performance | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 25; hpdctl status | grep Temps

# stop the load:
kill (jobs -p)
```

With the fans fixed, a hotter chip under identical load means more power is
getting through. If `ryzenadj` is installed, read the real power directly
instead of inferring from temperature:

```fish
echo power-saver | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 20; ryzenadj -i | grep -Ei 'STAPM|PPT'
echo performance | sudo tee /sys/firmware/acpi/platform_profile >/dev/null;  sleep 20; ryzenadj -i | grep -Ei 'STAPM|PPT'
```

### Verdict

- **performance clearly hotter / higher-power** than power-saver under
  identical load + fans → the profile **gates** the real power → **keep
  it**; the daemon couples it to `cool` and must never remove it.
  *(This is what RC73XA showed: 59 °C vs 95 °C.)*
- **identical under both** → the profile is vestigial once TDP + curve are
  controlled → remove the `PlatformProfile` capability + `SetProfile`.

---

## Results template

Copy, fill, and attach to the PR:

```
Device: ROG Xbox Ally X RC73XA   Kernel: ____   hpd: feat/fan-curves @ ____

§1 telemetry matches sysfs ......... PASS / FAIL  notes:
§2 write + read-back ............... PASS / FAIL  pwm_enable on set = ___ (expect 1)
§2 audible ramp up/down ............ PASS / FAIL
§4 curve survives profile change ... PASS / FAIL  enable after fan profile = ___
§5 curve restored on resume ........ PASS / FAIL
§6 follows_profile ................. PASS / FAIL
§7 AC chain ........................ PASS / FAIL
§8 persistence + first-boot default  PASS / FAIL
§9 polkit (wheel passwordless) ..... PASS / FAIL
§10 edge cases ..................... PASS / FAIL
§11 platform_profile A (ppt moves?). YES / NO
§11 platform_profile B (gates TDP?). YES / NO    → verdict: remove / pin / keep

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
