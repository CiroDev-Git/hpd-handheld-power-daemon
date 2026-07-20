<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Known power-limit enforcement gaps

> Devices where hpd writes a power envelope correctly (the value round-trips
> back from sysfs unchanged) but the firmware/EC does not actually hold the
> hardware to it. Born from an on-device investigation (2026-07-18, ROG Xbox
> Ally X / RC73XA, CachyOS) triggered by a user report: TDP set to 7W,
> `EnableGpuAutoFollow` on, measured SoC power sustained at 25-30W during
> real gameplay (Kingdom Come: Deliverance).

## Why this list exists

Architectural rule 1 (`CLAUDE.md`) is that hpd is the sole authority over
TDP/fans/charge and never fakes a reading. That rule covers hpd's own
correctness — it says nothing about whether the *firmware* actually honours
a value hpd correctly wrote. This doc is where that second, narrower kind of
gap gets tracked once confirmed, so it isn't rediscovered from scratch on
every report of "my TDP doesn't feel like it's applying."

A device belongs on this list only after ruling out every daemon-side and
OS-side cause first (see "Investigation checklist" below) — most reports of
"TDP isn't working" turn out to be one of those, not a firmware gap.

## Confirmed gaps

### ROG Xbox Ally X (RC73XA) — a `platform_profile` write silently drops the PPT limits — **ROOT CAUSE FOUND, daemon-mitigated**

**Status: solved.** The original report (2026-07-18: TDP set to 7W, measured
25-30W sustained during real gameplay) looked like a mysterious,
intermittent, lowest-tier-only enforcement failure. A controlled perf
campaign on the same device (2026-07-19) found the real trigger, made it
deterministic, and the daemon now mitigates it. The full curing commit adds
`reassert_envelope_after_profile` in `hpd-core::reducer` — see its doc
comment for the code-side summary.

- **Root cause**: writing the ACPI `platform_profile` (an actual value
  change, not a same-value rewrite — the reducer dedupes those) makes the
  EC **silently drop the previously-written SPL/SPPT/FPPT limits**. The
  sysfs attributes still read back the old values (the driver's view is
  stale), but the chip runs at the new profile's own defaults. Nothing
  about the tier matters — the original "only at 7W" framing was
  coincidental (a low TDP just makes the gap *visible*, because the
  profile defaults are far above it).
- **Deterministic repro** (campaign runs B05-B09, 15W/13W tiers,
  `stress-ng` + `glmark2` load):
  - B05: profile already `performance`, no write → enforced correctly.
  - B06: `performance → balanced` → 21-25W sustained vs 19W FPPT for the
    whole 4-min run; CPU score *rose* to 16.9k vs 14.6k at the same
    nominal TDP — the score itself proves the extra watts were real.
  - B07: `balanced → eco` → clobber masked (eco's own EPP bias keeps the
    draw below the ceiling anyway).
  - B08: `eco → performance`, TDP 13W → **21-34W sustained for 4 full
    minutes**; GPU scored ~8.7k, identical to an unconstrained 28-35W run.
  - B09: no profile change, fresh `tdp set` → enforced correctly again —
    a fresh envelope write **re-establishes enforcement**.
- **Daemon-side mitigation** (this repo, same pattern as the existing
  fan-curve re-assert for the EC's curve-drop-on-profile-write quirk):
  1. `SetProfile` re-asserts the active power envelope (and the managed
     fan curve, as before) immediately after the profile effect.
  2. Every composed effect list that carries both writes orders the
     profile **first**: boot/resume re-assert, the AC-lock's forced-max
     (`force_ac_max_performance`), and the unplug restore
     (`restore_dc_state`) — the last of which also re-asserts
     envelope/curve when **only** the profile differed from the snapshot
     ("equal in our state" ≠ "still enforced in the EC").
- **Still true / still worth reporting upstream**: the EC behaviour itself
  is a firmware quirk (the drop is silent; sysfs reads back stale values),
  and the same class of report exists on other ASUS models (ProArt P16,
  seerge/g-helper#4996). hpd now works around it, but the kernel
  `asus-armoury` driver (and Windows tooling) presumably have the same
  exposure — an upstream report with the deterministic repro above is
  worth filing.
- **Watchdog**: `GetTelemetry`'s `boost_ceiling_mw` key plus
  `hpdctl status`'s warning line (both added while investigating this)
  remain the passive tripwire — they are how the campaign caught the
  trigger in the act, and they stay valuable for any *future* gap.

## Investigation checklist (before adding a device here)

Rule out every one of these first — most "TDP doesn't apply" reports are
one of them, not a firmware gap:

1. **hpd's own write actually landed.** Read the backend's raw sysfs/WMI
   attribute directly (for the ASUS backend:
   `/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl{1,2,3}_*/current_value`)
   and compare against `hpdctl status`'s `CurrentSpl`. A mismatch here is an
   hpd bug, not a firmware gap — file it as one.
2. **No competing power daemon.** `hpdctl status`'s health block, or
   `GetPowerConflicts()` directly.
3. **Not AC-locked.** `AcLocked` forces max performance by design (see
   `CLAUDE.md`'s "AC = maximum performance" section) — that is not a bug of
   any kind.
4. **No recent relevant update.** Check `pacman`/distro package log (or
   equivalent) for a kernel, `linux-firmware`, `fwupd`/BIOS flash, or hpd
   update around when the symptom first appeared, and cross-reference each
   against its own changelog for anything power-related.
5. **BIOS is current**, or at least confirmed to already include any
   ASUS-published power-management fix for that model.
6. **The measurement source is credible.** Prefer the `amdgpu` hwmon's
   `power1_input` (what `hpdctl status`/`GetTelemetry`'s `soc_power_mw`
   reads) over a third-party overlay, which may read whole-system battery
   draw instead of SoC package power — see `TelemetryPanel`'s own doc
   comment in the plugin repo on that exact confusion.

Only once all six are ruled out does a sustained, reproducible gap between
`soc_power_mw` and `boost_ceiling_mw` belong on this list.
