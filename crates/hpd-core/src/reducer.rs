// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure state-transition function.

use tracing::info;

use hpd_capabilities::charge::DEFAULT_CHARGE_THRESHOLD;
use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::gpu_clock::GpuClockSelection;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileName, RuntimeConfig, TdpPreset};
use hpd_capabilities::units::PowerMilliwatts;
use hpd_error::HpdError;

use crate::effect::Effect;
use crate::inference::infer_fan_curve_from_spl;
use crate::invariants::validate_power_envelope;
use crate::state::{DcSnapshot, ProfileState};
use crate::transition::Transition;

/// Combined output of a single [`reduce`] call: the post-transition
/// state and the ordered list of side-effects the executor must dispatch
/// to honour it.
pub struct ReducerOutput {
    /// State after the transition has been applied.
    pub new_state: ProfileState,
    /// Side-effects (hardware writes, persistence) to be dispatched by
    /// the executor in order.
    pub effects: Vec<Effect>,
}

/// Compute new state and required effects.
///
/// Pure function — no I/O, no async, no globals. `config` is the
/// currently-active `RuntimeConfig` (cooling thresholds + SPPT/FPPT
/// boost multipliers); callers re-pass it on every invocation so the
/// reducer never has to look at static state. The `Executor` owns the
/// authoritative copy and swaps it on `Transition::ConfigReload`.
pub fn reduce(
    state: &ProfileState,
    transition: Transition,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> Result<ReducerOutput, HpdError> {
    let mut new_state = state.clone();
    let mut effects = Vec::new();

    // "AC = maximum performance" lock: while plugged in with the
    // `ac_max_performance` preference on, the power/cooling levers are pinned
    // and user writes are ignored (the battery charge threshold is exempt,
    // and `SetAcMaxPerformance` itself is never gated — it is how you release
    // the lock). This is the reducer-level backstop; the D-Bus setters also
    // reject up-front so the caller gets an immediate error. AC / suspend /
    // boot / `Sync*` rollback / config-reload transitions are never gated.
    if state.is_ac_connected && state.ac_max_performance && is_locked_write(&transition) {
        info!(?transition, "Ignored: on AC, locked to maximum performance");
        return Ok(ReducerOutput { new_state, effects });
    }

    match transition {
        Transition::SetPreset(preset) => {
            let min_w = device_limits.spl_min.as_watts();
            let max_w = device_limits.spl_max.as_watts();

            let target_watts = match preset {
                TdpPreset::Eco => min_w,
                // saturating_add: defensive against pathological device_limits.
                TdpPreset::Balanced => min_w.saturating_add(max_w) / 2,
                TdpPreset::Max => max_w,
            };

            return reduce(
                state,
                Transition::SetSpl(target_watts),
                device_limits,
                config,
            );
        }

        // -----------------------------------------------------
        // SMART MODE (get boost automatically)
        // -----------------------------------------------------
        Transition::SetSpl(watts) => {
            // PowerMilliwatts::from_watts encapsulates the overflow check
            // that protects against wrap-around for huge user input.
            let spl = PowerMilliwatts::from_watts(watts)?;

            if spl < device_limits.spl_min || spl > device_limits.spl_max {
                return Err(HpdError::InvariantViolation(format!(
                    "SPL ({}W) out of hardware range",
                    watts
                )));
            }

            let new_target = derive_boosted_envelope(spl, device_limits, config);

            validate_power_envelope(&new_target, device_limits)?;

            return apply_power_target(state, new_target, device_limits, config);
        }

        // -----------------------------------------------------
        // MANUAL MODE (user define values)
        // -----------------------------------------------------
        Transition::SetEnvelope(new_target) => {
            validate_power_envelope(&new_target, device_limits)?;

            return apply_power_target(state, new_target, device_limits, config);
        }

        Transition::SetProfile(new_profile) => {
            // Manual power lever (ACPI platform_profile / EPP), decoupled
            // from cooling: it does NOT touch the fan curve selection or
            // the auto-cooling flag. We still re-assert the active fan
            // curve afterwards because writing platform_profile can make
            // the EC drop the custom curve back to firmware auto.
            if new_state.active_profile != new_profile {
                new_state.active_profile = new_profile.clone();
                effects.push(Effect::ApplyPlatformProfile(new_profile));
                reassert_curve_after_profile(&mut new_state, &mut effects);
                effects.push(Effect::PersistState);
            }
        }

        Transition::SetCoolingLevel(preset) => {
            // Cooling lever = fan curve only (decoupled from power). Set
            // the requested curve and latch manual mode; the platform
            // profile / power envelope are untouched.
            let selection = FanCurveSelection::Preset(preset);
            new_state.fan_follows_tdp = false;
            new_state.active_fan_curve = Some(selection);
            effects.push(Effect::ApplyFanCurve(selection));
            effects.push(Effect::PersistState);
        }

        Transition::SetCustomFanCurve { cpu, gpu } => {
            // Identical shape to `SetCoolingLevel`, just with an explicit
            // curve instead of a named preset.
            let selection = FanCurveSelection::Custom { cpu, gpu };
            new_state.fan_follows_tdp = false;
            new_state.active_fan_curve = Some(selection);
            effects.push(Effect::ApplyFanCurve(selection));
            effects.push(Effect::PersistState);
        }

        Transition::SetGpuClockRange { min_mhz, max_mhz } => {
            // Manual override, identical shape to `SetCustomFanCurve`.
            let selection = GpuClockSelection::Custom(hpd_capabilities::gpu_clock::GpuClockRange {
                min_mhz,
                max_mhz,
            });
            new_state.gpu_follows_tdp = false;
            new_state.active_gpu_clock = Some(selection);
            effects.push(Effect::ApplyGpuClockRange(selection));
            effects.push(Effect::PersistState);
        }

        Transition::EnableGpuAutoFollow => {
            // Mirrors EnableFanAuto: infer and apply immediately rather than
            // waiting for the next TDP change to happen to touch the
            // envelope.
            if !new_state.gpu_follows_tdp {
                info!("Enabling GPU clock auto-follow (follows TDP)");
                new_state.gpu_follows_tdp = true;

                let inferred_tier = infer_fan_curve_from_spl(
                    &new_state.power_target,
                    device_limits,
                    &config.profile_thresholds,
                );
                let selection = GpuClockSelection::Preset(inferred_tier);
                new_state.active_gpu_clock = Some(selection);
                effects.push(Effect::ApplyGpuClockRange(selection));
                effects.push(Effect::PersistState);
            }
        }

        Transition::ResetGpuClocks => {
            if new_state.active_gpu_clock.is_some() {
                new_state.active_gpu_clock = None;
                new_state.gpu_follows_tdp = false;
                effects.push(Effect::ResetGpuClocks);
                effects.push(Effect::PersistState);
            }
        }

        Transition::ChargeThresholdChanged(threshold) => {
            if new_state.charge_end_threshold != threshold {
                new_state.charge_end_threshold = threshold;
                effects.push(Effect::ApplyChargeThreshold(threshold));
                effects.push(Effect::PersistState);
            }
        }

        Transition::SyncPowerTarget(real_target) => {
            // Forced rollback. Kernel overrode (or rejected) the hpd config.
            // PropertiesChanged is emitted automatically by the daemon's
            // properties watcher when state_tx receives this new value.
            new_state.power_target = real_target;
        }

        Transition::SyncPlatformProfile(real_profile) => {
            // Mirror of SyncPowerTarget for the platform profile rail.
            // No PersistState here either: the executor reads the
            // authoritative value back from hardware, so a reboot
            // would re-read the same value and converge anyway.
            new_state.active_profile = real_profile;
        }

        Transition::SyncChargeThreshold(real_threshold) => {
            // Mirror of SyncPowerTarget for the charge end threshold.
            new_state.charge_end_threshold = real_threshold;
        }

        Transition::SyncFanCurve(real_selection) => {
            // Mirror of SyncPowerTarget for the fan curve: the executor
            // read the EC's actual selection back after a failed write, so
            // the reported level reflects reality (no PersistState — a
            // reboot re-reads + re-asserts anyway).
            new_state.active_fan_curve = real_selection;
        }

        Transition::SyncGpuClockRange(real_range) => {
            // Mirror of SyncFanCurve: the executor read the actual
            // committed range back after a failed write. Always `Custom`
            // (or `None`) — a rollback read-back can never be attributed
            // to a curated `Preset` tier, only ever the concrete MHz the
            // hardware reports.
            new_state.active_gpu_clock = real_range.map(GpuClockSelection::Custom);
        }

        Transition::AcPowerChanged(is_plugged) => {
            // Debounce: ignore no-op transitions. CRITICAL — without this a
            // duplicate plug event would re-snapshot the *forced-max* state
            // as the "DC" state, clobbering the user's real battery prefs.
            if state.is_ac_connected == is_plugged {
                return Ok(ReducerOutput {
                    new_state: state.clone(),
                    effects: vec![],
                });
            }

            let mut output = if is_plugged {
                if state.ac_max_performance {
                    // Lock ON: snapshot the user's battery (DC) prefs so unplug
                    // can restore the full set, then force maximum performance.
                    info!("Charger plugged: locking to maximum performance (Performance / Max / Aggressive)");
                    let snapshot = DcSnapshot {
                        power_target: state.power_target.clone(),
                        active_profile: state.active_profile.clone(),
                        active_fan_curve: state.active_fan_curve,
                        fan_follows_tdp: state.fan_follows_tdp,
                        active_gpu_clock: state.active_gpu_clock,
                        gpu_follows_tdp: state.gpu_follows_tdp,
                    };
                    let mut o = force_ac_max_performance(state, device_limits, config);
                    o.new_state.last_dc_state = Some(snapshot);
                    o.effects.push(Effect::PersistState);
                    o
                } else {
                    // Lock OFF: fully manual — plugging in changes nothing.
                    info!("Charger plugged: AC lock disabled, no change (manual mode)");
                    ReducerOutput {
                        new_state: state.clone(),
                        effects: vec![],
                    }
                }
            } else if let Some(snapshot) = state.last_dc_state.clone() {
                // Unplug with a saved snapshot (we forced something on AC):
                // restore the battery state and clear the snapshot.
                info!(
                    action = "restore_previous",
                    "Charger unplugged: restoring battery (DC) state"
                );
                restore_dc_state(state, &snapshot)
            } else if state.ac_max_performance {
                // Locked but no snapshot — the first unplug after a cold
                // install / boot that happened on AC. Synthesize quiet battery
                // defaults instead of leaving the forced-max levers in place:
                // Balanced TDP + re-engaged auto-cooling so the fan curve drops
                // from the forced Aggressive (otherwise the fans stay loud on
                // battery). Reduce on an already-unplugged view so the lock
                // doesn't gate the internal SetPreset.
                info!(preset = %TdpPreset::Balanced, "Charger unplugged: no saved DC state, applying quiet defaults (Balanced + auto cooling)");
                let mut dc_view = state.clone();
                dc_view.is_ac_connected = false;
                dc_view.fan_follows_tdp = true;
                reduce(
                    &dc_view,
                    Transition::SetPreset(TdpPreset::Balanced),
                    device_limits,
                    config,
                )?
            } else {
                // Lock OFF, nothing was ever forced: fully manual, no change.
                info!("Charger unplugged: AC lock disabled, no change (manual mode)");
                ReducerOutput {
                    new_state: state.clone(),
                    effects: vec![],
                }
            };

            output.new_state.is_ac_connected = is_plugged;
            return Ok(output);
        }

        Transition::SystemResumed => {
            // Used by both resume-from-suspend and the daemon's boot
            // re-assert: re-apply the full intended state to hardware so
            // the reported state always matches the device.
            //
            // On AC with the lock enabled, re-assert the maximum-performance
            // policy instead of the persisted levers — we may have booted or
            // resumed straight into AC (where no `AcPowerChanged` edge fires),
            // and the device should already be pinned + locked. The charge
            // threshold is always re-applied (it is exempt from the lock).
            if state.is_ac_connected && state.ac_max_performance {
                info!("Boot/resume on AC: re-asserting maximum-performance lock");
                let mut output = force_ac_max_performance(state, device_limits, config);
                output
                    .effects
                    .push(Effect::ApplyChargeThreshold(state.charge_end_threshold));
                output.effects.push(Effect::PersistState);
                return Ok(output);
            }

            // On battery (or lock off): if a battery snapshot exists, the
            // persisted power/cooling levers are the **stale forced-max** from
            // an AC-locked session that ended while still plugged in (then the
            // device was unplugged while off/asleep). Restore the snapshot so
            // we come back to the user's real battery state, not max — instead
            // of re-applying the persisted (forced-max) levers verbatim.
            if let Some(snapshot) = state.last_dc_state.clone() {
                info!("Boot/resume on battery: restoring saved battery state (persisted levers were forced-max on AC)");
                let mut output = restore_dc_state(state, &snapshot);
                output
                    .effects
                    .push(Effect::ApplyChargeThreshold(state.charge_end_threshold));
                return Ok(output);
            }

            info!("Re-applying full power/cooling state to hardware (boot/resume)");
            let mut effects = vec![
                Effect::ApplyPowerEnvelope(state.power_target.clone()),
                Effect::ApplyPlatformProfile(state.active_profile.clone()),
                Effect::ApplyChargeThreshold(state.charge_end_threshold),
            ];
            // Re-apply the managed fan curve: the EC can drop or reset
            // the custom curve across suspend (the "fans blast at 100%
            // on resume" bug). If we are not managing the curve
            // (`None`), leave firmware auto alone.
            if let Some(ref selection) = state.active_fan_curve {
                effects.push(Effect::ApplyFanCurve(*selection));
            }
            // GPU clock: only re-assert if the user has opted in
            // (`active_gpu_clock.is_some()`) — same guard as everywhere
            // else. Reset-then-reapply (not a bare re-apply, unlike the
            // fan curve above) because a crash between switching the DPM
            // to manual and committing a range is a real risk class this
            // capability has that the fan curve doesn't; re-asserting from
            // a known-clean firmware-auto baseline avoids inheriting a
            // half-written state.
            if let Some(selection) = state.active_gpu_clock {
                effects.push(Effect::ResetGpuClocks);
                effects.push(Effect::ApplyGpuClockRange(selection));
            }
            return Ok(ReducerOutput {
                new_state: state.clone(),
                effects,
            });
        }

        Transition::ResetFanCurve => {
            // Guard on `fan_follows_tdp` too, not just `active_fan_curve`:
            // handing control back to firmware must also stop the daemon
            // from re-inferring and silently re-applying a curve on the
            // next TDP change. Without this, a `fan_follows_tdp=true` +
            // `active_fan_curve=None` state (reachable at cold boot before
            // the first TDP-triggered inference, or by calling this twice)
            // would treat the button as a no-op even though auto-follow was
            // still live.
            if new_state.active_fan_curve.is_some() || new_state.fan_follows_tdp {
                new_state.active_fan_curve = None;
                new_state.fan_follows_tdp = false;
                effects.push(Effect::ResetFanCurve);
                effects.push(Effect::PersistState);
            }
        }

        Transition::RestoreDefaults => {
            // Bundles five existing per-lever transitions into one atomic
            // action, threading state through recursive reduce() calls
            // exactly like SetPreset -> SetSpl above, just chained.
            //
            // ORDER MATTERS: SetProfile must run before ResetFanCurve.
            // SetProfile's reassert_curve_after_profile() re-applies
            // whatever curve is active after writing platform_profile (the
            // EC can drop a custom curve on a profile write) — running it
            // after ResetFanCurve would silently re-arm a curve
            // ResetFanCurve just cleared.
            let mut cur = state.clone();

            let step = reduce(
                &cur,
                Transition::SetPreset(TdpPreset::Balanced),
                device_limits,
                config,
            )?;
            cur = step.new_state;
            effects.extend(step.effects);

            let step = reduce(
                &cur,
                Transition::SetProfile(ProfileName::Performance),
                device_limits,
                config,
            )?;
            cur = step.new_state;
            effects.extend(step.effects);

            let step = reduce(
                &cur,
                Transition::ChargeThresholdChanged(DEFAULT_CHARGE_THRESHOLD),
                device_limits,
                config,
            )?;
            cur = step.new_state;
            effects.extend(step.effects);

            let step = reduce(&cur, Transition::ResetFanCurve, device_limits, config)?;
            cur = step.new_state;
            effects.extend(step.effects);

            // GPU clock stays opt-in-forever: only reset it if the user had
            // already opted in — never opt a fresh user in here.
            if cur.active_gpu_clock.is_some() {
                let step = reduce(&cur, Transition::ResetGpuClocks, device_limits, config)?;
                cur = step.new_state;
                effects.extend(step.effects);
            }

            new_state = cur;

            // Each composed sub-reduce() may have pushed its own
            // Effect::PersistState (redundant-but-correct — its handler
            // re-reads the already-merged final state; state_tx.send()
            // happens once, before any effect dispatch, using this whole
            // arm's single combined output). Collapse to at most one, at
            // the end, and only if something actually changed — an
            // already-at-defaults state must still produce zero effects,
            // matching every individual reset's own no-op guard.
            let anything_changed = effects.iter().any(|e| matches!(e, Effect::PersistState));
            effects.retain(|e| !matches!(e, Effect::PersistState));
            if anything_changed {
                effects.push(Effect::PersistState);
            }
        }

        Transition::EnableFanAuto => {
            // Infer and apply the curve directly (mirrors SetCoolingLevel)
            // instead of recursing into SetEnvelope(power_target): that
            // recursion only emitted effects when the envelope "changed",
            // so re-engaging auto-cooling at an unchanged TDP silently left
            // the stale manual curve on the EC and never persisted the flag.
            if !new_state.fan_follows_tdp {
                info!("Enabling auto cooling profile (follows TDP)");
                new_state.fan_follows_tdp = true;

                let inferred_curve = infer_fan_curve_from_spl(
                    &new_state.power_target,
                    device_limits,
                    &config.profile_thresholds,
                );
                let selection = FanCurveSelection::Preset(inferred_curve);
                new_state.active_fan_curve = Some(selection);
                effects.push(Effect::ApplyFanCurve(selection));
                effects.push(Effect::PersistState);
            }
        }

        Transition::SetAcMaxPerformance(enabled) => {
            // Toggle the "lock to max on AC" preference, applied immediately.
            if new_state.ac_max_performance == enabled {
                return Ok(ReducerOutput { new_state, effects }); // no change
            }
            if state.is_ac_connected {
                if enabled {
                    // Turning the lock ON while plugged: snapshot the current
                    // (manual) state as the battery baseline if we have none,
                    // then force maximum performance.
                    info!("AC lock enabled while plugged: forcing maximum performance");
                    let mut o = force_ac_max_performance(state, device_limits, config);
                    o.new_state.ac_max_performance = true;
                    if o.new_state.last_dc_state.is_none() {
                        o.new_state.last_dc_state = Some(DcSnapshot {
                            power_target: state.power_target.clone(),
                            active_profile: state.active_profile.clone(),
                            active_fan_curve: state.active_fan_curve,
                            fan_follows_tdp: state.fan_follows_tdp,
                            active_gpu_clock: state.active_gpu_clock,
                            gpu_follows_tdp: state.gpu_follows_tdp,
                        });
                    }
                    o.effects.push(Effect::PersistState);
                    return Ok(o);
                }
                // Turning the lock OFF while plugged: restore the battery
                // snapshot (so you are not stranded at max), unlock, go manual.
                info!("AC lock disabled while plugged: restoring battery state, unlocking");
                let mut o = if let Some(snapshot) = state.last_dc_state.clone() {
                    restore_dc_state(state, &snapshot)
                } else {
                    ReducerOutput {
                        new_state: state.clone(),
                        effects: vec![Effect::PersistState],
                    }
                };
                o.new_state.ac_max_performance = false;
                return Ok(o);
            }
            // On battery: just store the preference; it applies on next plug.
            info!(
                enabled,
                "AC lock preference updated (on battery; applies on next plug)"
            );
            new_state.ac_max_performance = enabled;
            effects.push(Effect::PersistState);
        }

        // Intercepted by the Executor before reduce() is called; the
        // reducer treats it as a no-op so isolated calls (tests) are safe.
        Transition::ConfigReload(_) => {}

        Transition::Shutdown => {
            // Final flush before exit: persist state without mutating it.
            // The Executor breaks its run() loop after the resulting
            // PersistState effect has been dispatched.
            info!("Shutdown requested: persisting state before exit");
            effects.push(Effect::PersistState);
        }
    }

    Ok(ReducerOutput { new_state, effects })
}

fn apply_power_target(
    current_state: &ProfileState,
    new_target: PowerEnvelopeTarget,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> Result<ReducerOutput, HpdError> {
    let mut new_state = current_state.clone();
    let mut effects = Vec::new();

    if new_state.power_target != new_target {
        new_state.power_target = new_target.clone();
        effects.push(Effect::ApplyPowerEnvelope(new_target.clone()));

        // Auto-cooling: the fan curve (not the platform_profile) follows
        // the TDP. The platform_profile is a decoupled power lever that
        // defaults to Performance, so the SPL just written is the real
        // usable limit — we never clamp it down by inferring a PowerSaver
        // EPP here. Both `fan_follows_tdp` and `gpu_follows_tdp` are
        // independent flags sharing the SAME tier inference so a TDP
        // change can drive one, both, or neither depending on what the
        // user has opted into.
        if new_state.fan_follows_tdp || new_state.gpu_follows_tdp {
            let inferred_tier =
                infer_fan_curve_from_spl(&new_target, device_limits, &config.profile_thresholds);

            if new_state.fan_follows_tdp {
                let selection = FanCurveSelection::Preset(inferred_tier);
                if new_state.active_fan_curve != Some(selection) {
                    new_state.active_fan_curve = Some(selection);
                    effects.push(Effect::ApplyFanCurve(selection));
                }
            }

            if new_state.gpu_follows_tdp {
                let selection = GpuClockSelection::Preset(inferred_tier);
                if new_state.active_gpu_clock != Some(selection) {
                    new_state.active_gpu_clock = Some(selection);
                    effects.push(Effect::ApplyGpuClockRange(selection));
                }
            }
        }

        effects.push(Effect::PersistState);
    }

    Ok(ReducerOutput { new_state, effects })
}

/// Re-assert the *currently active* fan curve after a platform-profile
/// change. Call this whenever an `ApplyPlatformProfile` effect was just
/// emitted: writing the ACPI `platform_profile` can make the EC drop the
/// custom curve back to its automatic mode (the same failure mode as
/// suspend/resume), so we must re-write our curve afterwards or it is
/// silently lost.
///
/// The re-apply is intentionally unconditional (not gated on "selection
/// changed") precisely because the EC may have reset a curve that is, by
/// our bookkeeping, unchanged. A no-op when no curve is managed (`None`).
/// The caller owns the surrounding `PersistState`.
fn reassert_curve_after_profile(new_state: &mut ProfileState, effects: &mut Vec<Effect>) {
    if let Some(selection) = new_state.active_fan_curve {
        effects.push(Effect::ApplyFanCurve(selection));
    }
}

/// Whether a transition is a user-initiated power/cooling write that the
/// "AC = maximum performance" lock suppresses. The battery charge threshold
/// change, the AC / suspend / boot events, the internal `Sync*` rollbacks
/// and config reloads are deliberately NOT gated — only the levers a user
/// would adjust to trade performance for quiet/efficiency.
fn is_locked_write(transition: &Transition) -> bool {
    matches!(
        transition,
        Transition::SetSpl(_)
            | Transition::SetPreset(_)
            | Transition::SetEnvelope(_)
            | Transition::SetProfile(_)
            | Transition::SetCoolingLevel(_)
            | Transition::SetCustomFanCurve { .. }
            | Transition::EnableFanAuto
            | Transition::ResetFanCurve
            | Transition::SetGpuClockRange { .. }
            | Transition::EnableGpuAutoFollow
            | Transition::ResetGpuClocks
            | Transition::RestoreDefaults
    )
}

/// Derive the full power envelope for a given SPL by scaling SPPT/FPPT with
/// the configured boost factors, each clamped to its hardware `*_max` rail.
/// Single source of the "smart-mode" envelope maths shared by
/// `Transition::SetSpl` and [`force_ac_max_performance`].
///
/// Also floors SPPT at `max(spl, device_limits.sppt_min)` and FPPT at
/// `max(sppt, device_limits.fppt_min)`: `RuntimeConfig::sppt_factor` /
/// `fppt_factor` are operator-tunable (`config.toml`, hot-reloaded via
/// SIGHUP) and `RuntimeConfig::sanitized` only rejects clearly-broken values,
/// not every factor combination that could undershoot at a given SPL/rail
/// ceiling. Without the `spl`/`sppt` floor a legal-looking factor could still
/// produce `SPPT < SPL` or `FPPT < SPPT`, which `validate_power_envelope`
/// then rejects — silently failing *every* `SetSpl`/`SetPreset` (and the
/// AC-lock's own forced-max envelope) until the operator noticed the config
/// was bad. The `*_min` floor guards a **separate** failure mode found
/// on-device (2026-07-12): the ASUS ROG Xbox Ally X's SPPT/FPPT firmware
/// attributes report their own `min_value` (13W/19W) *above* SPL's `min_value`
/// (7W) — at a low SPL (e.g. the Eco preset), scaling by `sppt_factor` alone
/// can undershoot that hardware floor even though it still satisfies
/// `SPPT >= SPL`, and the write is rejected by the firmware with `EINVAL`.
fn derive_boosted_envelope(
    spl: PowerMilliwatts,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> PowerEnvelopeTarget {
    let sppt_raw =
        PowerMilliwatts(((spl.0 as f32 * config.sppt_factor) as u32).min(device_limits.sppt_max.0));
    let sppt = PowerMilliwatts(sppt_raw.0.max(spl.0).max(device_limits.sppt_min.0));
    let fppt_raw =
        PowerMilliwatts(((spl.0 as f32 * config.fppt_factor) as u32).min(device_limits.fppt_max.0));
    let fppt = PowerMilliwatts(fppt_raw.0.max(sppt.0).max(device_limits.fppt_min.0));
    PowerEnvelopeTarget {
        spl,
        sppt,
        fppt: Some(fppt),
    }
}

/// Build the forced "AC = maximum performance" state: TDP at the hardware
/// ceiling (with SPPT/FPPT derived via the boost factors), power mode
/// `Performance`, cooling curve `Aggressive`, and auto-cooling off (the
/// curve is pinned explicitly). Returns the new state plus the ordered
/// `Apply*` effects — power, then profile, then the fan curve **last**
/// (writing `platform_profile` can make the EC drop the custom curve, so it
/// must be re-written afterwards). Does **not** emit `PersistState`: callers
/// add it (and any charge re-apply) so the effect list stays composable.
fn force_ac_max_performance(
    base: &ProfileState,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> ReducerOutput {
    let target = derive_boosted_envelope(device_limits.spl_max, device_limits, config);
    let profile = ProfileName::Performance;
    let curve = FanCurveSelection::Preset(FanCurvePreset::Aggressive);

    let mut new_state = base.clone();
    new_state.power_target = target.clone();
    new_state.active_profile = profile.clone();
    new_state.active_fan_curve = Some(curve);
    new_state.fan_follows_tdp = false;

    let mut effects = vec![
        Effect::ApplyPowerEnvelope(target),
        Effect::ApplyPlatformProfile(profile),
        Effect::ApplyFanCurve(curve),
    ];

    // GPU clock: only pinned if the user already had SOME GPU-clock
    // management active (`active_gpu_clock.is_some()`). Its real default
    // is a PERMANENT `None` for anyone who never opted in — unlike the
    // fan curve, whose steady-state is never `None` — so this must NOT
    // mirror the unconditional fan-curve pin above: doing so would
    // silently auto-opt every fresh install into managed GPU clocks the
    // first time they plug in AC.
    if base.active_gpu_clock.is_some() {
        let gpu_selection = GpuClockSelection::Preset(FanCurvePreset::Aggressive);
        new_state.active_gpu_clock = Some(gpu_selection);
        new_state.gpu_follows_tdp = false;
        effects.push(Effect::ApplyGpuClockRange(gpu_selection));
    }

    ReducerOutput { new_state, effects }
}

/// Restore a previously-captured battery (DC) snapshot (on AC unplug, or
/// when the lock is toggled off while plugged), emitting only the `Apply*`
/// effects for the rails that actually differ from the current (forced-max)
/// state. Clears `last_dc_state` so a later unplug in manual mode does not
/// replay a stale snapshot. Always ends with `PersistState`.
fn restore_dc_state(current: &ProfileState, snapshot: &DcSnapshot) -> ReducerOutput {
    let mut new_state = current.clone();
    let mut effects = Vec::new();
    new_state.last_dc_state = None;

    if new_state.power_target != snapshot.power_target {
        new_state.power_target = snapshot.power_target.clone();
        effects.push(Effect::ApplyPowerEnvelope(snapshot.power_target.clone()));
    }
    if new_state.active_profile != snapshot.active_profile {
        new_state.active_profile = snapshot.active_profile.clone();
        effects.push(Effect::ApplyPlatformProfile(
            snapshot.active_profile.clone(),
        ));
    }
    if new_state.active_fan_curve != snapshot.active_fan_curve {
        new_state.active_fan_curve = snapshot.active_fan_curve;
        match snapshot.active_fan_curve {
            Some(selection) => effects.push(Effect::ApplyFanCurve(selection)),
            None => effects.push(Effect::ResetFanCurve),
        }
    }
    new_state.fan_follows_tdp = snapshot.fan_follows_tdp;
    if new_state.active_gpu_clock != snapshot.active_gpu_clock {
        new_state.active_gpu_clock = snapshot.active_gpu_clock;
        match snapshot.active_gpu_clock {
            Some(selection) => effects.push(Effect::ApplyGpuClockRange(selection)),
            None => effects.push(Effect::ResetGpuClocks),
        }
    }
    new_state.gpu_follows_tdp = snapshot.gpu_follows_tdp;
    effects.push(Effect::PersistState);

    ReducerOutput { new_state, effects }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
    use hpd_capabilities::profile::{ProfileName, RuntimeConfig};
    use hpd_capabilities::units::PowerMilliwatts;

    fn setup_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15000),
                sppt: PowerMilliwatts(15000),
                fppt: Some(PowerMilliwatts(15000)),
            },
            active_profile: ProfileName::Balanced,
            is_ac_connected: false,
            charge_end_threshold: DEFAULT_CHARGE_THRESHOLD,
            fan_follows_tdp: true,
            last_dc_state: None,
            active_fan_curve: None,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
            ac_max_performance: true,
            ac_locked: false,
        }
    }

    #[test]
    fn test_invariant_fppt_sppt_spl() {
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000), // Ally X ranges
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(53000),
        };
        let config = RuntimeConfig::DEFAULT;

        // Invalid attempt: SPPT lower than SPL
        let bad_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(20000),
            sppt: PowerMilliwatts(15000),
            fppt: Some(PowerMilliwatts(25000)),
        };

        let result = reduce(
            &state,
            Transition::SetEnvelope(bad_target),
            &limits,
            &config,
        );

        assert!(result.is_err(), "Must fail because SPPT < SPL");
    }

    #[test]
    fn test_fan_curve_inference_follows_tdp() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let state = setup_state(); // fan_follows_tdp = true, curve = None
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(53000),
        };
        let config = RuntimeConfig::DEFAULT;

        // ~30W is high in the 7..35 range (~82%), so auto-cooling selects
        // the Aggressive FAN CURVE.
        let high_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(30000),
            sppt: PowerMilliwatts(30000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        let output = reduce(
            &state,
            Transition::SetEnvelope(high_target),
            &limits,
            &config,
        )
        .unwrap();

        let aggressive = FanCurveSelection::Preset(FanCurvePreset::Aggressive);
        assert_eq!(output.new_state.active_fan_curve, Some(aggressive));
        assert!(output.effects.contains(&Effect::ApplyFanCurve(aggressive)));
        // Decoupled: the platform_profile is NOT inferred from TDP.
        assert_eq!(output.new_state.active_profile, state.active_profile);
        assert!(!output
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyPlatformProfile(_))));
    }

    #[test]
    fn test_set_spl_overflow_rejected() {
        // Regression for Audit §3.3 / Lote 5: `watts * 1000` could wrap
        // around for huge user input, producing a small value that would
        // spuriously pass the range check below it.
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(55000),
        };
        let config = RuntimeConfig::DEFAULT;

        let result = reduce(&state, Transition::SetSpl(u32::MAX), &limits, &config);

        assert!(
            matches!(result, Err(HpdError::InvariantViolation(_))),
            "u32::MAX watts must be rejected as overflow, got {:?}",
            result.err()
        );
    }

    #[test]
    fn test_ac_plugged_persists_dc_snapshot_even_when_envelope_unchanged() {
        // Regression for Audit §3.5 / Lote 6, updated for the AC-lock feature:
        // when the system is already at the max envelope and the charger is
        // plugged in, the power write may emit no envelope change — but we DO
        // capture last_dc_state and flip is_ac_connected, so PersistState MUST
        // still fire or those are lost on reboot.
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(55000),
        };
        let config = RuntimeConfig::DEFAULT; // ac_max_performance = true

        // Already at the forced-max envelope (35W SPL, sppt=40250, fppt=43750).
        let max_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        let state = ProfileState {
            power_target: max_target.clone(),
            active_profile: ProfileName::Performance,
            is_ac_connected: false,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_state: None,
            active_fan_curve: None,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
            ac_max_performance: true,
            ac_locked: false,
        };

        let output = reduce(&state, Transition::AcPowerChanged(true), &limits, &config)
            .expect("AcPowerChanged should succeed");

        assert!(
            output
                .effects
                .iter()
                .any(|e| matches!(e, Effect::PersistState)),
            "AcPowerChanged must emit PersistState even when the envelope is unchanged; got effects={:?}",
            output.effects
        );
        // The DC snapshot captures the pre-plug state verbatim.
        assert_eq!(
            output.new_state.last_dc_state,
            Some(DcSnapshot {
                power_target: max_target,
                active_profile: ProfileName::Performance,
                active_fan_curve: None,
                fan_follows_tdp: true,
                active_gpu_clock: None,
                gpu_follows_tdp: false,
            })
        );
        assert!(output.new_state.is_ac_connected);
    }

    fn setup_limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(55000),
        }
    }

    fn setup_config() -> RuntimeConfig {
        RuntimeConfig::DEFAULT
    }

    // ---------- AcPowerChanged ----------

    #[test]
    fn test_ac_changed_is_debounced_when_already_in_same_state() {
        // Redundant AcPowerChanged(false) on an already-DC state must yield zero
        // effects (no spurious re-apply / persistence).
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(
            out.effects.is_empty(),
            "expected no effects, got {:?}",
            out.effects
        );
        assert_eq!(out.new_state, state);
    }

    #[test]
    fn test_ac_plugged_forces_max_performance_and_snapshots_dc() {
        // Charger goes in (ac_max_performance default on): snapshot the DC
        // prefs and pin Performance / Max TDP / Aggressive.
        let state = setup_state(); // DC: Balanced, fan_follows_tdp=true, curve None
        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(out.new_state.is_ac_connected);
        // DC snapshot captured verbatim for restore on unplug.
        assert_eq!(
            out.new_state.last_dc_state,
            Some(DcSnapshot {
                power_target: state.power_target.clone(),
                active_profile: ProfileName::Balanced,
                active_fan_curve: None,
                fan_follows_tdp: true,
                active_gpu_clock: None,
                gpu_follows_tdp: false,
            })
        );
        // Forced to maximum performance on every lever.
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(!out.new_state.fan_follows_tdp);
        // Effects push all three levers + persist.
        assert!(out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyPowerEnvelope(_))));
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::Performance)));
        assert!(out
            .effects
            .contains(&Effect::ApplyFanCurve(FanCurveSelection::Preset(
                FanCurvePreset::Aggressive
            ))));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_ac_plugged_with_lock_off_is_fully_manual() {
        // With ac_max_performance OFF (the preference), plugging in changes
        // NOTHING — fully manual. No force, no max-TDP, no snapshot, no effects.
        let mut state = setup_state(); // Balanced profile, 15W, curve None
        state.ac_max_performance = false;
        let before = state.clone();

        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(out.new_state.is_ac_connected);
        // Everything else unchanged from the battery state.
        assert_eq!(out.new_state.power_target, before.power_target);
        assert_eq!(out.new_state.active_profile, before.active_profile);
        assert_eq!(out.new_state.active_fan_curve, before.active_fan_curve);
        assert!(out.new_state.last_dc_state.is_none());
        assert!(
            out.effects.is_empty(),
            "lock-off plug must produce no effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_ac_unplugged_restores_full_dc_snapshot() {
        // Unplug with a remembered DC snapshot: restore TDP + power mode +
        // cooling exactly as they were on battery.
        let saved = DcSnapshot {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(10_000),
                sppt: PowerMilliwatts(12_000),
                fppt: Some(PowerMilliwatts(14_000)),
            },
            active_profile: ProfileName::PowerSaver,
            active_fan_curve: Some(FanCurveSelection::Preset(FanCurvePreset::Silent)),
            fan_follows_tdp: false,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
        };
        let mut state = setup_state();
        // Currently locked at forced-max (as if just plugged).
        state.is_ac_connected = true;
        state.power_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        state.active_profile = ProfileName::Performance;
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive));
        state.fan_follows_tdp = false;
        state.last_dc_state = Some(saved.clone());

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(!out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target, saved.power_target);
        assert_eq!(out.new_state.active_profile, ProfileName::PowerSaver);
        assert_eq!(out.new_state.active_fan_curve, saved.active_fan_curve);
        assert!(!out.new_state.fan_follows_tdp);
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_ac_unplugged_without_snapshot_resets_to_quiet_defaults() {
        // First unplug after a cold install / boot that happened on AC: no DC
        // snapshot, and the levers are pinned at the forced max (Aggressive
        // curve, auto-cooling off). The unplug must land on quiet battery
        // defaults — Balanced TDP (midpoint of [7,35]W = 21W), auto-cooling
        // re-engaged, and the curve dropped to match (Balanced) instead of
        // leaving the fans roaring at Aggressive. The AC lock must NOT gate
        // this internal restore.
        let mut state = setup_state();
        state.is_ac_connected = true;
        state.last_dc_state = None;
        state.power_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        state.active_profile = ProfileName::Performance;
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive));
        state.fan_follows_tdp = false;

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(!out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(21_000));
        assert!(
            out.new_state.fan_follows_tdp,
            "auto-cooling must re-engage on the no-snapshot unplug"
        );
        // 21W on [7,35] → fraction 0.5 → Balanced curve (no longer Aggressive).
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Balanced)),
            "the forced Aggressive curve must drop to match the lower TDP"
        );
    }

    // ---------- SystemResumed ----------

    #[test]
    fn test_system_resumed_reapplies_envelope_profile_and_threshold() {
        // After resume the kernel may have lost our config. The reducer must
        // emit exactly the three re-apply effects without mutating state.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert_eq!(out.effects.len(), 3);
        assert!(out
            .effects
            .contains(&Effect::ApplyPowerEnvelope(state.power_target.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(state.active_profile.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyChargeThreshold(state.charge_end_threshold)));
    }

    #[test]
    fn test_system_resumed_reasserts_all_four_unconditionally_in_order() {
        // Boot-reassert contract: the daemon re-applies its whole state at
        // boot by sending SystemResumed, so it MUST push every knob to
        // hardware regardless of whether state already "has" the value (a
        // cold boot resets platform_profile/charge to firmware defaults
        // that the daemon would otherwise misreport), and the fan curve
        // MUST be applied after the platform profile (writing the profile
        // can reset the EC's custom curve).
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state();
        let sel = FanCurveSelection::Preset(FanCurvePreset::Balanced);
        state.active_fan_curve = Some(sel);
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state, "SystemResumed must not mutate state");
        assert!(out
            .effects
            .contains(&Effect::ApplyPowerEnvelope(state.power_target.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(state.active_profile.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyChargeThreshold(state.charge_end_threshold)));
        assert!(out.effects.contains(&Effect::ApplyFanCurve(sel)));
        let prof = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyPlatformProfile(_)))
            .unwrap();
        let curve = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyFanCurve(_)))
            .unwrap();
        assert!(
            curve > prof,
            "fan curve must be re-applied after the platform profile"
        );
    }

    // ---------- EnableFanAuto ----------

    #[test]
    fn test_enable_fan_auto_infers_and_applies_curve_and_persists() {
        // Regression for Audit §2.1 (2026-07): re-engaging auto-cooling must
        // land on the curve matching the *current* TDP immediately, not wait
        // for the next SetSpl/SetEnvelope to happen to change the envelope —
        // and it must persist, or the flag reverts to manual on restart.
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state(); // spl = 15_000mW
        state.fan_follows_tdp = false;
        let out = reduce(
            &state,
            Transition::EnableFanAuto,
            &setup_limits(), // [7_000, 35_000]mW
            &setup_config(), // low_frac=0.33, high_frac=0.67
        )
        .unwrap();
        assert!(out.new_state.fan_follows_tdp);
        // 15_000mW in [7_000, 35_000] -> fraction ~0.286 -> Silent.
        let silent = FanCurveSelection::Preset(FanCurvePreset::Silent);
        assert_eq!(out.new_state.active_fan_curve, Some(silent));
        assert!(out.effects.contains(&Effect::ApplyFanCurve(silent)));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_enable_fan_auto_is_no_op_when_already_enabled() {
        let state = setup_state(); // fan_follows_tdp = true
        let out = reduce(
            &state,
            Transition::EnableFanAuto,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- ChargeThresholdChanged ----------

    #[test]
    fn test_charge_threshold_changed_applies_and_persists() {
        let state = setup_state(); // threshold = 80
        let out = reduce(
            &state,
            Transition::ChargeThresholdChanged(60),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.charge_end_threshold, 60);
        assert_eq!(
            out.effects,
            vec![Effect::ApplyChargeThreshold(60), Effect::PersistState]
        );
    }

    #[test]
    fn test_charge_threshold_changed_is_no_op_when_unchanged() {
        let state = setup_state(); // threshold = 80
        let out = reduce(
            &state,
            Transition::ChargeThresholdChanged(80),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- SetSpl boundaries ----------

    #[test]
    fn test_set_spl_just_below_min_is_rejected() {
        let state = setup_state();
        let result = reduce(
            &state,
            Transition::SetSpl(6),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_spl_just_above_max_is_rejected() {
        let state = setup_state();
        let result = reduce(
            &state,
            Transition::SetSpl(36),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_spl_at_min_boundary_is_accepted() {
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SetSpl(7),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(7_000));
    }

    #[test]
    fn test_set_spl_at_max_boundary_is_accepted() {
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SetSpl(35),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
    }

    #[test]
    fn test_set_spl_near_hw_ceiling_floors_sppt_fppt_at_spl() {
        // Regression for Audit §2.3 (2026-07): `derive_boosted_envelope`'s
        // clamp-to-`*_max` can put SPPT/FPPT *below* SPL on hardware whose
        // boost rails don't leave much headroom above `spl_max`, even with
        // an entirely valid (>= 1.0) boost factor — e.g. sppt_max = 36W is
        // only 1W above spl_max = 35W, so 35W * 1.15 = 40.25W clamps down to
        // 36W, which is still >= 35W here but a tighter rail would dip below
        // it. Without the floor this would fail validate_power_envelope's
        // `SPPT >= SPL` invariant and reject *every* SetSpl near the
        // ceiling. Use a pathological rail (sppt_max == spl_max) to force
        // the floor to actually bind.
        let state = setup_state();
        let tight_limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7_000),
            spl_max: PowerMilliwatts(35_000),
            sppt_min: PowerMilliwatts(7_000),
            sppt_max: PowerMilliwatts(35_000), // no headroom above SPL
            fppt_min: PowerMilliwatts(7_000),
            fppt_max: PowerMilliwatts(35_000),
        };
        let out = reduce(
            &state,
            Transition::SetSpl(35),
            &tight_limits,
            &setup_config(), // sppt_factor=1.15, fppt_factor=1.25 (both >= 1.0)
        )
        .expect("floored envelope must satisfy FPPT >= SPPT >= SPL, not be rejected");
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        assert_eq!(out.new_state.power_target.sppt, PowerMilliwatts(35_000));
        assert_eq!(
            out.new_state.power_target.fppt,
            Some(PowerMilliwatts(35_000))
        );
    }

    #[test]
    fn test_set_spl_at_low_end_floors_sppt_fppt_at_hardware_minimum() {
        // Regression found on-device (2026-07-12) on the ROG Xbox Ally X
        // (RC73XA): `ppt_pl2_sppt`/`ppt_pl3_fppt` report a `min_value`
        // (13W/19W) *above* `ppt_pl1_spl`'s (7W). At a low SPL like the Eco
        // preset, `sppt_factor` alone (7W * 1.15 ≈ 8W) undershoots that
        // hardware floor even though `SPPT >= SPL` still holds — the write
        // was rejected by the firmware with `EINVAL`. The derived envelope
        // must floor SPPT/FPPT at the hardware minimum too, not just at
        // SPL/SPPT.
        let state = setup_state();
        let rc73xa_limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7_000),
            spl_max: PowerMilliwatts(35_000),
            sppt_min: PowerMilliwatts(13_000),
            sppt_max: PowerMilliwatts(45_000),
            fppt_min: PowerMilliwatts(19_000),
            fppt_max: PowerMilliwatts(55_000),
        };
        let out = reduce(
            &state,
            Transition::SetSpl(7), // Eco preset's SPL
            &rc73xa_limits,
            &setup_config(), // sppt_factor=1.15, fppt_factor=1.25
        )
        .expect("a low SPL must not be rejected because of the boost-rail hardware floor");
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(7_000));
        assert!(
            out.new_state.power_target.sppt.0 >= rc73xa_limits.sppt_min.0,
            "SPPT ({}) must be at least the hardware minimum ({})",
            out.new_state.power_target.sppt.0,
            rc73xa_limits.sppt_min.0
        );
        let fppt = out.new_state.power_target.fppt.expect("fppt must be set");
        assert!(
            fppt.0 >= rc73xa_limits.fppt_min.0,
            "FPPT ({}) must be at least the hardware minimum ({})",
            fppt.0,
            rc73xa_limits.fppt_min.0
        );
    }

    // ---------- SetEnvelope invariant (FPPT >= SPPT) ----------

    #[test]
    fn test_set_envelope_rejects_fppt_below_sppt() {
        // Complement to test_invariant_fppt_sppt_spl (SPPT < SPL): this covers
        // the second invariant — FPPT < SPPT — in validate_power_envelope.
        let state = setup_state();
        let bad = PowerEnvelopeTarget {
            spl: PowerMilliwatts(15_000),
            sppt: PowerMilliwatts(20_000),
            fppt: Some(PowerMilliwatts(18_000)),
        };
        let result = reduce(
            &state,
            Transition::SetEnvelope(bad),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_envelope_rejects_spl_out_of_hardware_range() {
        // SetEnvelope (manual mode) previously ran no hardware-range check
        // at all — only SetSpl did. A manually-supplied SPL below spl_min
        // must now be rejected here too, not left to fail at the backend.
        let state = setup_state();
        let bad = PowerEnvelopeTarget {
            spl: PowerMilliwatts(3_000), // below setup_limits()'s spl_min of 7000
            sppt: PowerMilliwatts(10_000),
            fppt: Some(PowerMilliwatts(12_000)),
        };
        let result = reduce(
            &state,
            Transition::SetEnvelope(bad),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_envelope_rejects_sppt_below_hardware_minimum() {
        // Regression found on-device (2026-07-12): a manually-supplied
        // envelope that satisfies SPPT >= SPL can still undershoot the
        // hardware's own SPPT floor (RC73XA: 13W) — must be rejected here,
        // not surfaced as an opaque backend I/O error.
        let state = setup_state();
        let rc73xa_limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7_000),
            spl_max: PowerMilliwatts(35_000),
            sppt_min: PowerMilliwatts(13_000),
            sppt_max: PowerMilliwatts(45_000),
            fppt_min: PowerMilliwatts(19_000),
            fppt_max: PowerMilliwatts(55_000),
        };
        let bad = PowerEnvelopeTarget {
            spl: PowerMilliwatts(7_000),
            sppt: PowerMilliwatts(8_000), // >= SPL, but < the hardware's 13W floor
            fppt: Some(PowerMilliwatts(20_000)),
        };
        let result = reduce(
            &state,
            Transition::SetEnvelope(bad),
            &rc73xa_limits,
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_envelope_rejects_fppt_below_hardware_minimum() {
        let state = setup_state();
        let rc73xa_limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7_000),
            spl_max: PowerMilliwatts(35_000),
            sppt_min: PowerMilliwatts(13_000),
            sppt_max: PowerMilliwatts(45_000),
            fppt_min: PowerMilliwatts(19_000),
            fppt_max: PowerMilliwatts(55_000),
        };
        let bad = PowerEnvelopeTarget {
            spl: PowerMilliwatts(7_000),
            sppt: PowerMilliwatts(13_000),
            fppt: Some(PowerMilliwatts(15_000)), // >= SPPT, but < the hardware's 19W floor
        };
        let result = reduce(
            &state,
            Transition::SetEnvelope(bad),
            &rc73xa_limits,
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    // ---------- Sync* (rollback paths) ----------

    #[test]
    fn test_sync_power_target_overwrites_state_without_side_effects() {
        // Rollback: trust the kernel's view, mutate in-memory state, but do
        // NOT emit ApplyPowerEnvelope (that would loop the executor).
        let state = setup_state();
        let real = PowerEnvelopeTarget {
            spl: PowerMilliwatts(12_000),
            sppt: PowerMilliwatts(13_800),
            fppt: Some(PowerMilliwatts(15_000)),
        };
        let out = reduce(
            &state,
            Transition::SyncPowerTarget(real.clone()),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target, real);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_sync_platform_profile_overwrites_state_without_side_effects() {
        // Mirror of SyncPowerTarget for the platform profile rail
        // (Lote 38 / Audit V2 §4.5.1).
        let mut state = setup_state();
        state.active_profile = ProfileName::Performance;
        let out = reduce(
            &state,
            Transition::SyncPlatformProfile(ProfileName::PowerSaver),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_profile, ProfileName::PowerSaver);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_sync_charge_threshold_overwrites_state_without_side_effects() {
        // Mirror of SyncPowerTarget for the charge end threshold
        // (Lote 38 / Audit V2 §4.5.1).
        let state = setup_state();
        assert_ne!(state.charge_end_threshold, 65);
        let out = reduce(
            &state,
            Transition::SyncChargeThreshold(65),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.charge_end_threshold, 65);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    // ---------- Shutdown ----------

    #[test]
    fn test_shutdown_emits_only_persist_state_and_leaves_state_untouched() {
        // Last-chance flush before exit: the reducer must NOT mutate
        // ProfileState (we want exactly what was already in memory to
        // hit the disk) and must emit a single PersistState effect that
        // the Executor will dispatch before breaking its run() loop.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::Shutdown,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(
            out.new_state, state,
            "Shutdown must not mutate ProfileState"
        );
        assert_eq!(out.effects, vec![Effect::PersistState]);
    }

    // ---------- Fan curve ----------

    #[test]
    fn test_reset_fan_curve_clears_and_emits_reset() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state();
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Silent));
        let out = reduce(
            &state,
            Transition::ResetFanCurve,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_fan_curve, None);
        assert!(
            !out.new_state.fan_follows_tdp,
            "reset must also disengage auto-follow, or the next TDP change \
             silently re-infers and re-applies a curve, undoing the reset"
        );
        assert_eq!(
            out.effects,
            vec![Effect::ResetFanCurve, Effect::PersistState]
        );
    }

    #[test]
    fn test_reset_fan_curve_is_no_op_when_already_fully_auto() {
        let mut state = setup_state(); // active_fan_curve = None
        state.fan_follows_tdp = false;
        let out = reduce(
            &state,
            Transition::ResetFanCurve,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn test_reset_fan_curve_disengages_auto_follow_even_without_an_active_curve() {
        // Regression: on-device bug found 2026-07-12. Cold boot (or any path
        // that leaves `fan_follows_tdp=true` with no `active_fan_curve` yet)
        // made "Reset to firmware" a silent no-op, since the old guard only
        // checked `active_fan_curve.is_some()`. The button must still
        // disengage auto-follow so a later TDP change can't silently
        // re-apply a curve the user just asked to hand back to firmware.
        let state = setup_state(); // fan_follows_tdp = true, active_fan_curve = None
        let out = reduce(
            &state,
            Transition::ResetFanCurve,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out.new_state.fan_follows_tdp);
        assert_eq!(out.new_state.active_fan_curve, None);
        assert_eq!(
            out.effects,
            vec![Effect::ResetFanCurve, Effect::PersistState],
            "must actually write the reset to hardware and persist, not no-op"
        );
    }

    #[test]
    fn test_system_resumed_reapplies_fan_curve_when_managed() {
        // Regression for the suspend/resume bug: a managed curve must be
        // re-applied on resume (the EC can reset it across suspend),
        // adding a 4th effect on top of the envelope/profile/charge trio.
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let sel = FanCurveSelection::Preset(FanCurvePreset::Balanced);
        let mut state = setup_state();
        state.active_fan_curve = Some(sel);
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert_eq!(out.effects.len(), 4);
        assert!(out.effects.contains(&Effect::ApplyFanCurve(sel)));
    }

    #[test]
    fn test_set_profile_is_decoupled_from_fan_curve() {
        // Decoupled model: SetProfile changes only the platform_profile.
        // It never switches the fan curve to a "matching" preset, and it
        // must not flip the auto-cooling mode. With no managed curve there
        // is nothing to re-assert.
        let state = setup_state(); // active_profile = Balanced, curve = None
        let config = setup_config();

        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::Performance),
            &setup_limits(),
            &config,
        )
        .unwrap();

        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve, None,
            "SetProfile must not touch the fan curve"
        );
        assert_eq!(
            out.new_state.fan_follows_tdp, state.fan_follows_tdp,
            "SetProfile must not flip auto-cooling"
        );
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyFanCurve(_))));
    }

    #[test]
    fn test_set_cooling_level_sets_fan_curve_only() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state(); // Balanced profile, fan_follows_tdp = true
        state.fan_follows_tdp = true;
        let out = reduce(
            &state,
            Transition::SetCoolingLevel(FanCurvePreset::Aggressive),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        // Cooling lever = fan curve only; mode latches manual.
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(!out.new_state.fan_follows_tdp);
        assert!(out
            .effects
            .contains(&Effect::ApplyFanCurve(FanCurveSelection::Preset(
                FanCurvePreset::Aggressive
            ))));
        // Power is decoupled: the platform profile is left untouched.
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyPlatformProfile(_))));
    }

    /// An 8-point curve for `SetCustomFanCurve` tests — its exact shape
    /// doesn't matter here, since the reducer trusts the D-Bus layer to
    /// have already validated it (see `hpd-capabilities::fan_curve`).
    fn sample_custom_curve() -> hpd_capabilities::fan_curve::FanCurve {
        use hpd_capabilities::fan_curve::FanCurvePoint;
        hpd_capabilities::fan_curve::FanCurve::new([
            FanCurvePoint::new(45, 20),
            FanCurvePoint::new(54, 40),
            FanCurvePoint::new(62, 60),
            FanCurvePoint::new(69, 80),
            FanCurvePoint::new(75, 100),
            FanCurvePoint::new(80, 120),
            FanCurvePoint::new(85, 150),
            FanCurvePoint::new(92, 200),
        ])
    }

    #[test]
    fn test_set_custom_fan_curve_sets_fan_curve_only() {
        let mut state = setup_state(); // Balanced profile, fan_follows_tdp = true
        state.fan_follows_tdp = true;
        let cpu = sample_custom_curve();
        let gpu = sample_custom_curve();
        let out = reduce(
            &state,
            Transition::SetCustomFanCurve { cpu, gpu },
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        // Cooling lever = fan curve only; mode latches manual, exactly
        // like SetCoolingLevel.
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Custom { cpu, gpu })
        );
        assert!(!out.new_state.fan_follows_tdp);
        assert!(out
            .effects
            .contains(&Effect::ApplyFanCurve(FanCurveSelection::Custom {
                cpu,
                gpu
            })));
        // Power is decoupled: the platform profile is left untouched.
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyPlatformProfile(_))));
    }

    #[test]
    fn test_profile_change_reasserts_active_curve() {
        // Preservation: writing the platform profile can make the EC drop
        // the custom curve, so a profile change must re-apply the active
        // curve UNCHANGED (it is re-asserted, never re-inferred).
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let sel = FanCurveSelection::Preset(FanCurvePreset::Silent);
        let mut state = setup_state();
        state.active_fan_curve = Some(sel);
        let config = setup_config();
        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::Performance),
            &setup_limits(),
            &config,
        )
        .unwrap();
        // Curve selection is preserved, not changed to a matching preset.
        assert_eq!(out.new_state.active_fan_curve, Some(sel));
        // But it IS re-applied to the hardware after the profile write.
        assert!(out.effects.contains(&Effect::ApplyFanCurve(sel)));
        // Ordering: the curve re-apply comes after the profile write.
        let profile_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyPlatformProfile(_)))
            .unwrap();
        let curve_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyFanCurve(_)))
            .unwrap();
        assert!(
            curve_idx > profile_idx,
            "curve must be re-applied AFTER the profile"
        );
    }

    // ---------- ConfigReload (no-op at the reducer layer) ----------

    #[test]
    fn test_config_reload_is_a_no_op_in_the_reducer() {
        // The Executor intercepts ConfigReload before reduce() to mutate
        // its own RuntimeConfig. Calling reduce() with it directly must
        // therefore touch neither state nor effects — otherwise an
        // integration that bypasses the Executor (e.g. a future
        // synchronous test harness) would silently corrupt state.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::ConfigReload(RuntimeConfig::DEFAULT),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- AC = maximum-performance lock ----------

    /// Helper: a state that is on AC and therefore subject to the lock when
    /// `ac_max_performance` is on. Mirrors what the executor produces right
    /// after a plug edge (Performance / Max / Aggressive).
    fn locked_on_ac_state() -> ProfileState {
        let mut s = setup_state();
        s.is_ac_connected = true;
        s.power_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        s.active_profile = ProfileName::Performance;
        s.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive));
        s.fan_follows_tdp = false;
        s
    }

    #[test]
    fn test_lock_ignores_power_and_cooling_writes_on_ac() {
        // Every user power/cooling write is a no-op while locked.
        let state = locked_on_ac_state();
        let cfg = setup_config(); // ac_max_performance = true
        let writes = [
            Transition::SetSpl(10),
            Transition::SetPreset(TdpPreset::Eco),
            Transition::SetEnvelope(PowerEnvelopeTarget {
                spl: PowerMilliwatts(8_000),
                sppt: PowerMilliwatts(9_000),
                fppt: Some(PowerMilliwatts(10_000)),
            }),
            Transition::SetProfile(ProfileName::PowerSaver),
            Transition::SetCoolingLevel(FanCurvePreset::Silent),
            Transition::SetCustomFanCurve {
                cpu: sample_custom_curve(),
                gpu: sample_custom_curve(),
            },
            Transition::EnableFanAuto,
            Transition::ResetFanCurve,
            Transition::SetGpuClockRange {
                min_mhz: 600,
                max_mhz: 1_800,
            },
            Transition::EnableGpuAutoFollow,
            Transition::ResetGpuClocks,
            Transition::RestoreDefaults,
        ];
        for t in writes {
            let label = format!("{t:?}");
            let out = reduce(&state, t, &setup_limits(), &cfg).unwrap();
            assert!(
                out.effects.is_empty(),
                "{label} must produce no effects while locked, got {:?}",
                out.effects
            );
            assert_eq!(
                out.new_state, state,
                "{label} must not mutate state while locked"
            );
        }
    }

    #[test]
    fn test_lock_allows_charge_threshold_on_ac() {
        // The battery charge threshold is exempt from the lock.
        let state = locked_on_ac_state();
        let out = reduce(
            &state,
            Transition::ChargeThresholdChanged(70),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.charge_end_threshold, 70);
        assert!(out.effects.contains(&Effect::ApplyChargeThreshold(70)));
    }

    #[test]
    fn test_lock_inactive_when_preference_disabled() {
        // On AC but the ac_max_performance preference is off → writes apply
        // normally (no lock).
        let mut state = locked_on_ac_state();
        state.ac_max_performance = false;
        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::PowerSaver),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_profile, ProfileName::PowerSaver);
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::PowerSaver)));
    }

    #[test]
    fn test_system_resumed_on_ac_forces_max_and_charge() {
        // Boot/resume straight into AC (no plug edge) must re-assert the
        // forced-max policy plus re-apply the charge threshold.
        let mut state = setup_state(); // Balanced, curve None, fan_follows true
        state.is_ac_connected = true;
        state.charge_end_threshold = 75;

        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(out.effects.contains(&Effect::ApplyChargeThreshold(75)));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_cold_install_on_ac_then_first_unplug() {
        // End-to-end of the "installed while plugged in" scenario: a fresh
        // install has no DC snapshot and boots on AC. The boot re-assert
        // (SystemResumed) forces max + lock; the very first unplug lands on
        // quiet battery defaults rather than the forced-max leftovers.
        let limits = setup_limits();
        let config = setup_config();

        // Cold initial state: hardware-read values, on AC, no snapshot.
        let mut cold = setup_state();
        cold.is_ac_connected = true;
        cold.last_dc_state = None;

        // 1. Boot re-assert forces maximum performance.
        let booted = reduce(&cold, Transition::SystemResumed, &limits, &config)
            .unwrap()
            .new_state;
        assert_eq!(booted.power_target.spl, PowerMilliwatts(35_000));
        assert_eq!(booted.active_profile, ProfileName::Performance);
        assert_eq!(
            booted.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(booted.is_ac_connected);
        assert!(
            booted.last_dc_state.is_none(),
            "boot re-assert does not fabricate a DC snapshot"
        );

        // 2. First unplug → quiet defaults (Balanced + auto-cooling).
        let unplugged = reduce(&booted, Transition::AcPowerChanged(false), &limits, &config)
            .unwrap()
            .new_state;
        assert!(!unplugged.is_ac_connected);
        assert_eq!(unplugged.power_target.spl, PowerMilliwatts(21_000));
        assert!(unplugged.fan_follows_tdp);
        assert_eq!(
            unplugged.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Balanced))
        );
    }

    #[test]
    fn test_system_resumed_on_battery_reapplies_persisted_state() {
        // On battery, SystemResumed re-applies the persisted levers verbatim
        // (no forcing), regardless of ac_max_performance.
        let mut state = setup_state();
        state.is_ac_connected = false;
        state.active_profile = ProfileName::Balanced;

        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert_eq!(out.new_state.power_target, state.power_target);
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::Balanced)));
    }

    #[test]
    fn test_system_resumed_on_battery_with_snapshot_restores_battery_state() {
        // Scenarios B & D: the device was shut down / suspended on AC (locked
        // at forced-max, with a battery snapshot saved), then unplugged while
        // off/asleep, then boots / resumes on battery. The persisted levers are
        // the stale forced-max — SystemResumed must restore the battery
        // snapshot instead of re-applying max on battery.
        let snap = DcSnapshot {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15_000),
                sppt: PowerMilliwatts(17_250),
                fppt: Some(PowerMilliwatts(18_750)),
            },
            active_profile: ProfileName::Balanced,
            active_fan_curve: None,
            fan_follows_tdp: true,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
        };
        // Persisted = forced-max from the AC session.
        let mut state = setup_state();
        state.is_ac_connected = false; // booted/resumed on battery
        state.power_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        state.active_profile = ProfileName::Performance;
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive));
        state.fan_follows_tdp = false;
        state.last_dc_state = Some(snap.clone());

        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        // Back to the battery state, not max.
        assert_eq!(out.new_state.power_target, snap.power_target);
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert_eq!(out.new_state.active_fan_curve, None);
        assert!(out.new_state.fan_follows_tdp);
        // Snapshot consumed; charge re-applied; persisted.
        assert!(out.new_state.last_dc_state.is_none());
        assert!(out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyChargeThreshold(_))));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    // ---------- SetAcMaxPerformance (the runtime toggle) ----------

    #[test]
    fn test_toggle_lock_off_while_plugged_restores_and_unlocks() {
        // Locked on AC with a saved battery snapshot. Toggling the lock OFF
        // restores the battery state, clears the snapshot, and unlocks.
        let snap = DcSnapshot {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(12_000),
                sppt: PowerMilliwatts(13_800),
                fppt: Some(PowerMilliwatts(15_000)),
            },
            active_profile: ProfileName::Balanced,
            active_fan_curve: Some(FanCurveSelection::Preset(FanCurvePreset::Silent)),
            fan_follows_tdp: true,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
        };
        let mut state = locked_on_ac_state(); // forced max, ac_max_performance = true
        state.last_dc_state = Some(snap.clone());

        let out = reduce(
            &state,
            Transition::SetAcMaxPerformance(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(!out.new_state.ac_max_performance);
        assert_eq!(out.new_state.power_target, snap.power_target);
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert_eq!(out.new_state.active_fan_curve, snap.active_fan_curve);
        assert!(out.new_state.last_dc_state.is_none(), "snapshot cleared");
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_toggle_lock_on_while_plugged_forces_and_snapshots() {
        // Manual on AC (lock off), then toggle ON: snapshot the current manual
        // state for unplug restore + force maximum performance.
        let mut state = setup_state();
        state.is_ac_connected = true;
        state.ac_max_performance = false;
        state.last_dc_state = None;
        let manual = state.clone();

        let out = reduce(
            &state,
            Transition::SetAcMaxPerformance(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(out.new_state.ac_max_performance);
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert_eq!(
            out.new_state.last_dc_state,
            Some(DcSnapshot {
                power_target: manual.power_target,
                active_profile: manual.active_profile,
                active_fan_curve: manual.active_fan_curve,
                fan_follows_tdp: manual.fan_follows_tdp,
                active_gpu_clock: manual.active_gpu_clock,
                gpu_follows_tdp: manual.gpu_follows_tdp,
            })
        );
    }

    #[test]
    fn test_toggle_lock_on_battery_just_stores_preference() {
        // On battery, toggling only stores the preference (no force / restore,
        // no hardware effects) — it applies on the next plug.
        let state = setup_state(); // on battery, ac_max_performance = true
        let before = state.clone();
        let out = reduce(
            &state,
            Transition::SetAcMaxPerformance(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out.new_state.ac_max_performance);
        assert_eq!(out.new_state.power_target, before.power_target);
        assert!(!out.effects.iter().any(|e| matches!(
            e,
            Effect::ApplyPowerEnvelope(_)
                | Effect::ApplyPlatformProfile(_)
                | Effect::ApplyFanCurve(_)
        )));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_toggle_lock_to_same_value_is_a_noop() {
        let state = locked_on_ac_state(); // ac_max_performance = true
        let out = reduce(
            &state,
            Transition::SetAcMaxPerformance(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- GPU clock range ----------

    #[test]
    fn test_set_gpu_clock_range_sets_custom_and_disables_follow() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
        let mut state = setup_state();
        state.gpu_follows_tdp = true;
        let out = reduce(
            &state,
            Transition::SetGpuClockRange {
                min_mhz: 600,
                max_mhz: 1_800,
            },
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        let expected = GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_800,
        });
        assert_eq!(out.new_state.active_gpu_clock, Some(expected));
        assert!(!out.new_state.gpu_follows_tdp);
        assert!(out.effects.contains(&Effect::ApplyGpuClockRange(expected)));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_enable_gpu_auto_follow_infers_and_applies_and_persists() {
        use hpd_capabilities::fan_curve::FanCurvePreset;
        use hpd_capabilities::gpu_clock::GpuClockSelection;
        let state = setup_state(); // spl = 15_000mW -> Silent tier (see EnableFanAuto test)
        let out = reduce(
            &state,
            Transition::EnableGpuAutoFollow,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(out.new_state.gpu_follows_tdp);
        let silent = GpuClockSelection::Preset(FanCurvePreset::Silent);
        assert_eq!(out.new_state.active_gpu_clock, Some(silent));
        assert!(out.effects.contains(&Effect::ApplyGpuClockRange(silent)));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_enable_gpu_auto_follow_is_no_op_when_already_enabled() {
        let mut state = setup_state();
        state.gpu_follows_tdp = true;
        let out = reduce(
            &state,
            Transition::EnableGpuAutoFollow,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn test_reset_gpu_clocks_clears_and_emits_reset() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
        let mut state = setup_state();
        state.active_gpu_clock = Some(GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_800,
        }));
        state.gpu_follows_tdp = false;
        let out = reduce(
            &state,
            Transition::ResetGpuClocks,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_gpu_clock, None);
        assert!(!out.new_state.gpu_follows_tdp);
        assert_eq!(
            out.effects,
            vec![Effect::ResetGpuClocks, Effect::PersistState]
        );
    }

    #[test]
    fn test_reset_gpu_clocks_is_no_op_when_already_auto() {
        let state = setup_state(); // active_gpu_clock = None
        let out = reduce(
            &state,
            Transition::ResetGpuClocks,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn test_restore_defaults_applies_all_levers_from_dirty_state() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};

        let mut state = setup_state(); // spl=15000mw, off the 21000mw balanced midpoint
        state.active_profile = ProfileName::PowerSaver;
        // setup_state()'s own charge default IS DEFAULT_CHARGE_THRESHOLD
        // (80) — override to something genuinely off-target so this test
        // still exercises the charge-reset effect.
        state.charge_end_threshold = 60;
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Silent));
        state.fan_follows_tdp = false;
        state.active_gpu_clock = Some(GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_800,
        }));
        state.gpu_follows_tdp = false;

        let out = reduce(
            &state,
            Transition::RestoreDefaults,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        // Balanced = midpoint of setup_limits()'s 7000..35000mw SPL range.
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(21_000));
        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(out.new_state.charge_end_threshold, DEFAULT_CHARGE_THRESHOLD);
        assert_eq!(out.new_state.active_fan_curve, None);
        assert!(!out.new_state.fan_follows_tdp);
        assert_eq!(out.new_state.active_gpu_clock, None);
        assert!(!out.new_state.gpu_follows_tdp);

        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::Performance)));
        assert!(out
            .effects
            .contains(&Effect::ApplyChargeThreshold(DEFAULT_CHARGE_THRESHOLD)));
        assert!(out.effects.contains(&Effect::ResetFanCurve));
        assert!(out.effects.contains(&Effect::ResetGpuClocks));
        assert_eq!(
            out.effects
                .iter()
                .filter(|e| matches!(e, Effect::PersistState))
                .count(),
            1,
            "must collapse redundant PersistState effects to exactly one, got {:?}",
            out.effects
        );
        assert_eq!(
            out.effects.last(),
            Some(&Effect::PersistState),
            "the single PersistState must be last, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_restore_defaults_is_a_full_no_op_when_already_at_defaults() {
        // Seed a state already at the 21W balanced midpoint via the same
        // SetSpl path SetPreset(Balanced) itself uses, so the envelope's
        // derived SPPT/FPPT match exactly (rather than hand-computing
        // them and risking a mismatch with RuntimeConfig::DEFAULT's boost
        // factors).
        let seeded = reduce(
            &setup_state(),
            Transition::SetSpl(21),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        let mut state = seeded.new_state;
        state.active_profile = ProfileName::Performance;
        state.charge_end_threshold = DEFAULT_CHARGE_THRESHOLD;
        state.active_fan_curve = None;
        state.fan_follows_tdp = false;
        state.active_gpu_clock = None;
        state.gpu_follows_tdp = false;

        let out = reduce(
            &state,
            Transition::RestoreDefaults,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert_eq!(out.new_state, state);
        assert!(
            out.effects.is_empty(),
            "an already-at-defaults state must produce zero effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_restore_defaults_resets_gpu_clock_when_opted_in() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};

        let mut state = setup_state();
        state.active_gpu_clock = Some(GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_800,
        }));

        let out = reduce(
            &state,
            Transition::RestoreDefaults,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(out.effects.contains(&Effect::ResetGpuClocks));
        assert_eq!(out.new_state.active_gpu_clock, None);
    }

    #[test]
    fn test_restore_defaults_does_not_opt_in_gpu_clock() {
        // active_gpu_clock = None (never touched) — RestoreDefaults must
        // never auto-opt the user in, mirroring ResetGpuClocks's own
        // no-op guard.
        let state = setup_state();
        assert_eq!(state.active_gpu_clock, None);

        let out = reduce(
            &state,
            Transition::RestoreDefaults,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ResetGpuClocks | Effect::ApplyGpuClockRange(_))));
        assert_eq!(out.new_state.active_gpu_clock, None);
    }

    #[test]
    fn test_sync_gpu_clock_range_overwrites_state_without_side_effects() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
        let state = setup_state();
        let real = GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_200,
        };
        let out = reduce(
            &state,
            Transition::SyncGpuClockRange(Some(real)),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(
            out.new_state.active_gpu_clock,
            Some(GpuClockSelection::Custom(real))
        );
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_gpu_clock_follows_tdp_independently_of_fan_curve() {
        // gpu_follows_tdp and fan_follows_tdp are independent flags: a TDP
        // change must drive whichever ones are on, sharing one inference
        // call, never assuming the other is also enabled.
        use hpd_capabilities::fan_curve::FanCurvePreset;
        use hpd_capabilities::gpu_clock::GpuClockSelection;
        let mut state = setup_state();
        state.fan_follows_tdp = false; // manual cooling
        state.gpu_follows_tdp = true; // auto GPU clock

        let high_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(30_000),
            sppt: PowerMilliwatts(30_000),
            fppt: Some(PowerMilliwatts(30_000)),
        };
        let out = reduce(
            &state,
            Transition::SetEnvelope(high_target),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        // GPU clock updates...
        let aggressive = GpuClockSelection::Preset(FanCurvePreset::Aggressive);
        assert_eq!(out.new_state.active_gpu_clock, Some(aggressive));
        assert!(out
            .effects
            .contains(&Effect::ApplyGpuClockRange(aggressive)));
        // ...but the fan curve (manual) is untouched.
        assert_eq!(out.new_state.active_fan_curve, None);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyFanCurve(_))));
    }

    #[test]
    fn test_lock_ignores_gpu_clock_writes_on_ac() {
        // Covered generically in test_lock_ignores_power_and_cooling_writes_on_ac,
        // this test asserts it precisely for a state that has GPU clock
        // management already active, so the lock's no-op path is exercised
        // against a non-trivial baseline too.
        use hpd_capabilities::gpu_clock::GpuClockSelection;
        let mut state = locked_on_ac_state();
        state.active_gpu_clock = Some(GpuClockSelection::Preset(FanCurvePreset::Aggressive));
        let out = reduce(
            &state,
            Transition::SetGpuClockRange {
                min_mhz: 600,
                max_mhz: 900,
            },
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- CRITICAL: GPU clock opt-in guard ----------
    //
    // GPU clock's real default is a PERMANENT `None` for anyone who never
    // opts in via EnableGpuAutoFollow/SetGpuClockRange — unlike the fan
    // curve, whose steady-state is never `None`. Every site that
    // unconditionally re-pins/reapplies the fan curve MUST guard the
    // matching GPU-clock effect on `active_gpu_clock.is_some()`, or a
    // fresh install would be silently auto-opted in the first time AC is
    // plugged in. These are the single most important tests in this file.

    #[test]
    fn test_ac_plug_does_not_touch_gpu_clock_when_never_opted_in() {
        let state = setup_state(); // active_gpu_clock = None (fresh install)
        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(
            out.new_state.active_gpu_clock, None,
            "plugging in AC must never auto-opt a user into managed GPU clocks"
        );
        assert!(!out.new_state.gpu_follows_tdp);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyGpuClockRange(_) | Effect::ResetGpuClocks)));
    }

    #[test]
    fn test_ac_plug_pins_gpu_clock_when_already_opted_in() {
        use hpd_capabilities::fan_curve::FanCurvePreset;
        use hpd_capabilities::gpu_clock::GpuClockSelection;
        let mut state = setup_state();
        state.gpu_follows_tdp = true;
        state.active_gpu_clock = Some(GpuClockSelection::Preset(FanCurvePreset::Silent));
        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        let aggressive = GpuClockSelection::Preset(FanCurvePreset::Aggressive);
        assert_eq!(out.new_state.active_gpu_clock, Some(aggressive));
        assert!(!out.new_state.gpu_follows_tdp, "pinned, not following");
        assert!(out
            .effects
            .contains(&Effect::ApplyGpuClockRange(aggressive)));
        // DC snapshot must capture the pre-plug GPU-clock state verbatim.
        assert_eq!(
            out.new_state
                .last_dc_state
                .as_ref()
                .unwrap()
                .active_gpu_clock,
            Some(GpuClockSelection::Preset(FanCurvePreset::Silent))
        );
        assert!(
            out.new_state
                .last_dc_state
                .as_ref()
                .unwrap()
                .gpu_follows_tdp
        );
    }

    #[test]
    fn test_ac_unplug_restores_gpu_clock_from_snapshot() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
        let gpu_selection = GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_000,
        });
        let saved = DcSnapshot {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(10_000),
                sppt: PowerMilliwatts(12_000),
                fppt: Some(PowerMilliwatts(14_000)),
            },
            active_profile: ProfileName::PowerSaver,
            active_fan_curve: Some(FanCurveSelection::Preset(FanCurvePreset::Silent)),
            fan_follows_tdp: false,
            active_gpu_clock: Some(gpu_selection),
            gpu_follows_tdp: false,
        };
        let mut state = locked_on_ac_state();
        state.last_dc_state = Some(saved.clone());

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        assert_eq!(out.new_state.active_gpu_clock, Some(gpu_selection));
        assert!(!out.new_state.gpu_follows_tdp);
        assert!(out
            .effects
            .contains(&Effect::ApplyGpuClockRange(gpu_selection)));
    }

    #[test]
    fn test_system_resumed_does_not_touch_gpu_clock_when_never_opted_in() {
        let state = setup_state(); // active_gpu_clock = None, on battery
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyGpuClockRange(_) | Effect::ResetGpuClocks)));
    }

    #[test]
    fn test_system_resumed_reset_then_reapplies_gpu_clock_when_managed() {
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
        let selection = GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_500,
        });
        let mut state = setup_state();
        state.active_gpu_clock = Some(selection);

        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();

        let reset_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ResetGpuClocks));
        let apply_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyGpuClockRange(s) if *s == selection));
        assert!(
            reset_idx.is_some() && apply_idx.is_some(),
            "expected both ResetGpuClocks and ApplyGpuClockRange, got {:?}",
            out.effects
        );
        assert!(
            apply_idx.unwrap() > reset_idx.unwrap(),
            "must reset before reapplying (crash-safety: known-clean baseline)"
        );
    }

    #[test]
    fn test_toggle_is_never_gated_by_the_lock() {
        // The toggle must work even while locked (it is how you release it).
        let state = locked_on_ac_state(); // locked
        let out = reduce(
            &state,
            Transition::SetAcMaxPerformance(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(
            !out.new_state.ac_max_performance,
            "toggle must not be gated"
        );
    }
}
