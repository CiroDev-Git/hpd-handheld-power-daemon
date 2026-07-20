<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Performance baseline — ROG Xbox Ally X (RC73XA) / CachyOS / hpd 3.1.x

> The measured performance/power/thermal baseline behind MANUAL.md's
> "Where the watts actually go" section and the data-driven validation of
> the daemon's tunables (boost factors, GPU tier fractions, auto-cooling
> thresholds). Kept as the reference point for any future retuning
> proposal: bring numbers that beat these, measured the same way.
> This campaign is also what root-caused the profile-write PPT-clobber
> (see POWER-ENFORCEMENT-GAPS.md — runs B05-B09 here are that evidence).

Campaign: 2026-07-19/20. Phases A (30 runs) + B (9) + C (3) + R (2).
2,105 telemetry samples, 137 bench scores. Contaminated runs B06/B07/B08
(EC profile-clobber, see the daemon repo's POWER-ENFORCEMENT-GAPS.md)
excluded from every performance curve; kept as bug evidence only.
Methodology: 4-min runs, thermal-gated starts (<50°C), sustained window
= samples at t>=180s, scores = medians.

## 1. Core curves (battery, defaults, sustained)

| TDP | CPU bogo/s | CPU/W | GPU glmark2 | GPU/W | 7z MIPS | vkmark | mem |
|----:|-----------:|------:|------------:|------:|--------:|-------:|----:|
|  7W |  8,784 | **1,255** | 3,629 | 518 | 31,892 | 4,849 | 10,778 |
| 13W | 14,239 | 1,055 | 7,233 | **556** | — | — | — |
| 16W | 16,489 | 1,031 | 8,089 | 506 | — | — | — |
| 21W | 19,173 | 959 | 7,901 | 376 | 74,296 | **13,338** | **19,709** |
| 28W | 21,759 | 777 | 8,800 | 289 | — | — | — |
| 35W | 22,808 | 671 | 8,869 | 286 | 79,618 | 13,256 | 19,702 |

Key facts:
- **GPU saturates by 21W** in every API (glmark2/OpenGL ~8.8k ceiling,
  vkmark/Vulkan flat 21→35W, and GPU-only load physically cannot draw
  more than ~30-31W even when allowed 35).
- **GPU efficiency peaks at 13W** (556 score/W): 82% of max GPU perf at
  37% of max power.
- **Memory bandwidth saturates by 21W** (19,709 vs 19,702 — identical).
- **CPU keeps scaling to 35W but pays dearly**: 28→35W buys +4.8% for
  +21% power at 86°C sustained / 96°C peaks.
- Mixed loads scale smoothly; 21W delivers ~82-83% of both CPU and GPU
  vs the 35W mixed ceiling.

## 2. Idle / battery baseline

Whole-system idle draw is ~7.5-8W regardless of TDP setting (SoC idles
at 4-5W); the TDP knob costs nothing at idle. Battery-life envelope
(80Wh pack): ~10h idle-ish, ~4h gaming at 13W, ~3h at 21W, <2h at 35W.

## 3. Boost windows (SPPT/FPPT behaviour)

Boost is real and matches the configured rails: e.g. 35W tier boosts to
43-45W (SPPT 45/FPPT 55) before settling at 34W sustained; 21W tier
boosts to 23-24W (SPPT 24). At the 7W floor the CPU-only run boosted to
13W (the SPPT hardware floor) for the first ~45s. `sppt_factor`/
`fppt_factor` (1.15/1.25) are delivering exactly their design intent.

## 4. Interactions (phase B, clean runs)

- **Cooling level does not cost performance at 35W** (4-min windows):
  silent/balanced/aggressive all ~11.8-12.0k CPU, same GPU, same 34W
  sustained. But note: at 35W "silent" is not silent — 7,600 rpm vs
  8,000 (the curves converge near the safety floor at 96°C peaks).
  Cooling choice is a *noise* lever at low/mid TDP; at max TDP thermals
  override it. Long-run throttling under silent remains untested
  (phase D).
- **Power mode costs real performance at the same TDP**: eco scored
  12,831 vs performance 14,599 at 15W (-12%). (Balanced's number is
  unusable — that run is the clobber evidence.)
- **GPU auto-follow leaves nothing on the table**: firmware-auto vs
  managed at 28W identical (8,733 vs 8,800, noise). Consistent with the
  clock data: observed sustained GPU clocks never reached the tier
  ceilings (1,400 vs 1,865 @16W; 2,046 vs 2,440 @21W; ~2,500 vs 2,900
  @28-35W) — power, not the clock cap, is the binding constraint.

## 5. AC vs DC

Identical at equal TDP (2-4% differences, run-to-run noise), verified at
7/21/35W mixed — even at 14-20% battery charge. The battery subsystem
does not limit sustained performance on this device.

## 6. Enforcement integrity

Zero violations in all 41 clean runs (sustained W vs FPPT+10%).
The only violations in the whole campaign are the three profile-clobber
runs (B06: 21-25W vs 19W ceiling; B08: 21-34W vs 13W target, 38 flagged
samples) — root-caused and fixed daemon-side (reassert envelope after
profile writes).

## 7. Verdicts on daemon tunables (data-driven)

| Tunable | Current | Verdict |
|---|---|---|
| `sppt_factor`/`fppt_factor` | 1.15/1.25 | **Keep.** Boost windows measured exactly as designed at every tier. |
| GPU tier fractions | 0.55/0.80/1.0 | **Keep.** No ceiling ever binds under sustained load; firmware-vs-auto parity confirms zero cost. |
| Auto-cooling thresholds | silent ≤~16W / balanced / aggressive ≥~26W | **Keep.** Sustained temps per tier are sane (62°C top of silent tier, 66°C balanced, 75-86°C aggressive). |
| Preset mapping (eco 7 / balanced 21 / max 35) | — | **Keep, document.** 21W validated as an excellent default (82-100% of everything). 35W is a niche burst tier: +4.8% CPU, +1% GPU for +65% power and 96°C peaks. |

## 8. User-facing guidance worth documenting (MANUAL / plugin Help)

- GPU-bound gaming sweet spot: **13-16W** (82-91% of max GPU perf).
- 21W ("balanced") ≈ 83% of everything; going past it mostly buys heat.
- 35W: short bursts / CPU-bound edge cases only.
- Eco *power mode* costs ~12% real perf at identical TDP — it is a
  battery lever, not a free setting.
- The TDP setting costs nothing at idle; no need to lower it for
  reading/video sessions.

## 9. Phase D — 30-minute soaks (thermal equilibrium, auto cooling)

| Run | TDP | Early (min 1-5) | Late (min 25-30) | Verdict |
|---|---|---|---|---|
| D01 | 7W | 7W · 45°C CPU · 3,600 rpm | 7W · 44°C · 3,600 rpm | Dead flat. Zero fade over 36 bench iterations. |
| D02 | 21W | 24W (boost) · 69°C · 6,000 rpm | 21W · 66.5°C · 6,000 rpm | Equilibrium reached in ~5 min and *improves* slightly as boost expires. Bench scores settle -3% from the boost window, then rock-stable. |

- **No long-run throttling** at either tier with auto cooling: thermal
  equilibrium lands at 44°C (7W) / 66-67°C (21W) and holds.
- **Zero enforcement warnings in 358 soak samples** — a full clean hour
  under the 3.1.1 profile-clobber fix.
- **Measured battery drain under load**: 7%/31min at 7W (~9 h of
  sustained 7W gaming) and 21%/31min at 21W (~2.4 h). Refines §2's
  estimates with directly measured numbers.

## Open items

- Long-run **silent**-cooling soak at high TDP was not tested (phase B
  covered silent@28-35W only in 4-min windows; phase D soaked *auto*
  cooling). If a user reports fade under pinned-silent at high TDP,
  soak that combination before concluding anything.
- On-device verification of the profile-clobber fix: **done** (3.1.1,
  2026-07-20) — both trigger sequences replayed under load, zero
  warnings, peaks within normal boost windows.
