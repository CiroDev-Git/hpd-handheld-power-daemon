// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure state-transition function.

use tracing::info;

use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
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

    // "AC = maximum performance" lock: while plugged in with
    // `ac_max_performance` on, the power/cooling levers are pinned and user
    // writes are ignored (the battery charge threshold is exempt). This is
    // the reducer-level backstop; the D-Bus setters also reject up-front so
    // the caller gets an immediate error. AC / suspend / boot / `Sync*`
    // rollback / config-reload transitions are never gated.
    if state.is_ac_connected && config.ac_max_performance && is_locked_write(&transition) {
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

            let sppt_mw =
                ((spl.0 as f32 * config.sppt_factor) as u32).min(device_limits.sppt_max.0);
            let fppt_mw =
                ((spl.0 as f32 * config.fppt_factor) as u32).min(device_limits.fppt_max.0);

            let new_target = PowerEnvelopeTarget {
                spl,
                sppt: PowerMilliwatts(sppt_mw),
                fppt: Some(PowerMilliwatts(fppt_mw)),
            };

            validate_power_envelope(&new_target)?;

            return apply_power_target(state, new_target, device_limits, config);
        }

        // -----------------------------------------------------
        // MANUAL MODE (user define values)
        // -----------------------------------------------------
        Transition::SetEnvelope(new_target) => {
            validate_power_envelope(&new_target)?;

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
                // Snapshot the user's battery (DC) prefs first so unplug can
                // restore the full set (TDP + power mode + cooling), then
                // apply the AC policy.
                let snapshot = DcSnapshot {
                    power_target: state.power_target.clone(),
                    active_profile: state.active_profile.clone(),
                    active_fan_curve: state.active_fan_curve,
                    fan_follows_tdp: state.fan_follows_tdp,
                };
                let mut o = if config.ac_max_performance {
                    info!("Charger plugged: locking to maximum performance (Performance / Max / Aggressive)");
                    force_ac_max_performance(state, device_limits, config)
                } else {
                    info!(preset = %TdpPreset::Max, "Charger plugged: applying Max TDP preset");
                    reduce(
                        state,
                        Transition::SetPreset(TdpPreset::Max),
                        device_limits,
                        config,
                    )?
                };
                o.new_state.last_dc_state = Some(snapshot);
                o
            } else if let Some(snapshot) = state.last_dc_state.clone() {
                info!(
                    action = "restore_previous",
                    "Charger unplugged: restoring battery (DC) state"
                );
                restore_dc_state(state, &snapshot)
            } else {
                info!(preset = %TdpPreset::Balanced, "Charger unplugged: no saved DC state, applying default preset");
                // Reduce on an already-unplugged view so the AC lock (still
                // showing `is_ac_connected = true` on `state`) doesn't gate
                // this internal SetPreset into a no-op.
                let mut dc_view = state.clone();
                dc_view.is_ac_connected = false;
                reduce(
                    &dc_view,
                    Transition::SetPreset(TdpPreset::Balanced),
                    device_limits,
                    config,
                )?
            };

            output.new_state.is_ac_connected = is_plugged;

            // Persist even when power_target didn't actually change: we still
            // mutated last_dc_state / is_ac_connected and those must survive
            // a reboot. The force/legacy paths may not emit PersistState, so
            // top it up here if missing.
            if !output
                .effects
                .iter()
                .any(|e| matches!(e, Effect::PersistState))
            {
                output.effects.push(Effect::PersistState);
            }

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
            if state.is_ac_connected && config.ac_max_performance {
                info!("Boot/resume on AC: re-asserting maximum-performance lock");
                let mut output = force_ac_max_performance(state, device_limits, config);
                output
                    .effects
                    .push(Effect::ApplyChargeThreshold(state.charge_end_threshold));
                output.effects.push(Effect::PersistState);
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
            return Ok(ReducerOutput {
                new_state: state.clone(),
                effects,
            });
        }

        Transition::ResetFanCurve => {
            if new_state.active_fan_curve.is_some() {
                new_state.active_fan_curve = None;
                effects.push(Effect::ResetFanCurve);
                effects.push(Effect::PersistState);
            }
        }

        Transition::EnableFanAuto => {
            if !new_state.fan_follows_tdp {
                info!("Enabling auto cooling profile (follows TDP)");
                new_state.fan_follows_tdp = true;

                return reduce(
                    &new_state,
                    Transition::SetEnvelope(new_state.power_target.clone()),
                    device_limits,
                    config,
                );
            }
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
        // EPP here.
        if new_state.fan_follows_tdp {
            let inferred_curve =
                infer_fan_curve_from_spl(&new_target, device_limits, &config.profile_thresholds);
            let selection = FanCurveSelection::Preset(inferred_curve);

            if new_state.active_fan_curve != Some(selection) {
                new_state.active_fan_curve = Some(selection);
                effects.push(Effect::ApplyFanCurve(selection));
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
            | Transition::EnableFanAuto
            | Transition::ResetFanCurve
    )
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
    let spl = device_limits.spl_max;
    let sppt =
        PowerMilliwatts(((spl.0 as f32 * config.sppt_factor) as u32).min(device_limits.sppt_max.0));
    let fppt =
        PowerMilliwatts(((spl.0 as f32 * config.fppt_factor) as u32).min(device_limits.fppt_max.0));
    let target = PowerEnvelopeTarget {
        spl,
        sppt,
        fppt: Some(fppt),
    };
    let profile = ProfileName::Performance;
    let curve = FanCurveSelection::Preset(FanCurvePreset::Aggressive);

    let mut new_state = base.clone();
    new_state.power_target = target.clone();
    new_state.active_profile = profile.clone();
    new_state.active_fan_curve = Some(curve);
    new_state.fan_follows_tdp = false;

    let effects = vec![
        Effect::ApplyPowerEnvelope(target),
        Effect::ApplyPlatformProfile(profile),
        Effect::ApplyFanCurve(curve),
    ];
    ReducerOutput { new_state, effects }
}

/// Restore a previously-captured battery (DC) snapshot on AC unplug,
/// emitting only the `Apply*` effects for the rails that actually differ
/// from the current (forced-max) state. Always ends with `PersistState`.
fn restore_dc_state(current: &ProfileState, snapshot: &DcSnapshot) -> ReducerOutput {
    let mut new_state = current.clone();
    let mut effects = Vec::new();

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
    use hpd_capabilities::charge::DEFAULT_CHARGE_THRESHOLD;
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
            ac_locked: false,
        }
    }

    #[test]
    fn test_invariant_fppt_sppt_spl() {
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000), // Ally X ranges
            sppt_max: PowerMilliwatts(43000),
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
            sppt_max: PowerMilliwatts(43000),
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
            sppt_max: PowerMilliwatts(43000),
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
            sppt_max: PowerMilliwatts(43000),
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
            })
        );
        assert!(output.new_state.is_ac_connected);
    }

    fn setup_limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
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
    fn test_ac_plugged_legacy_mode_only_maxes_tdp() {
        // With ac_max_performance OFF, the historic behaviour: only the TDP
        // ramps to max; power mode + cooling are untouched, nothing is forced.
        let mut config = setup_config();
        config.ac_max_performance = false;
        let state = setup_state(); // Balanced profile, curve None

        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &config,
        )
        .unwrap();

        assert!(out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        // Profile + cooling left as they were (no forcing).
        assert_eq!(out.new_state.active_profile, ProfileName::Balanced);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyPlatformProfile(_))));
        // DC snapshot still captured for symmetric restore on unplug.
        assert!(out.new_state.last_dc_state.is_some());
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
    fn test_ac_unplugged_without_snapshot_applies_balanced_preset() {
        // Cold case: no DC memory (e.g. booted while plugged, then unplugged).
        // Fall back to the Balanced preset (midpoint of [7,35]W = 21W). The
        // AC lock must NOT gate this internal restore.
        let mut state = setup_state();
        state.is_ac_connected = true;
        state.last_dc_state = None;

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(21_000));
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
    fn test_enable_fan_auto_flips_flag_when_previously_disabled() {
        // Today the re-evaluation produces zero effects because the inferred
        // profile is only re-applied behind an envelope change in
        // apply_power_target; the contract for this transition is just
        // that the flag flips. Persistence of the flag itself is a separate
        // concern tracked in the audit.
        let mut state = setup_state();
        state.fan_follows_tdp = false;
        let out = reduce(
            &state,
            Transition::EnableFanAuto,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(out.new_state.fan_follows_tdp);
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
        assert_eq!(
            out.effects,
            vec![Effect::ResetFanCurve, Effect::PersistState]
        );
    }

    #[test]
    fn test_reset_fan_curve_is_no_op_when_already_auto() {
        let state = setup_state(); // active_fan_curve = None
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
            Transition::EnableFanAuto,
            Transition::ResetFanCurve,
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
    fn test_lock_inactive_when_feature_disabled() {
        // On AC but ac_max_performance off → writes apply normally (no lock).
        let state = locked_on_ac_state();
        let mut cfg = setup_config();
        cfg.ac_max_performance = false;
        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::PowerSaver),
            &setup_limits(),
            &cfg,
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
}
