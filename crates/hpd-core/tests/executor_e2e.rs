// SPDX-License-Identifier: GPL-3.0-or-later

// Integration tests; same opt-out as the in-crate `mod tests` blocks.
// The strict `[workspace.lints.clippy]` bar applies to production code.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end pipeline tests for the L3 `Executor`.
//!
//! These exercise the full Transition → reducer → Effect → backend loop
//! against an in-memory `MockBackend`, including:
//!
//! * the happy path (apply + persist),
//! * the rollback path on hardware-write failure (power / profile / charge),
//! * the `watch::Receiver` propagation that hpd-dbus relies on for
//!   `PropertiesChanged`,
//! * the executor-level interception of `Transition::ConfigReload`
//!   (the reducer treats it as a no-op; only the Executor swaps its
//!   runtime config),
//! * the `Transition::Shutdown` drain path that breaks the `run()`
//!   loop so the spawned task joins on its own.
//!
//! Lifetime note: the Executor holds an internal clone of the transition
//! `mpsc::Sender` for self-injecting rollback events, so the channel
//! never closes naturally while the executor is alive — `run()` only
//! returns when it processes a `Transition::Shutdown`. Most tests here
//! drive the executor on a `tokio::spawn`'d task and `abort()` it once
//! the observable side effects are in place; the shutdown test (added
//! in Lote 40) is the one exception and awaits the join handle so it
//! can pin down the "executor exits cleanly without abort" contract.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::{GpuClockFractions, ProfileName, ProfileThresholds, RuntimeConfig};
use hpd_capabilities::testing::{MockBackend, RecordedCall};
use hpd_capabilities::units::PowerMilliwatts;

use hpd_core::executor::Executor;
use hpd_core::persistence::StatePersister;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;

fn limits() -> PowerEnvelopeLimits {
    PowerEnvelopeLimits {
        spl_min: PowerMilliwatts(7_000),
        spl_max: PowerMilliwatts(35_000),
        sppt_min: PowerMilliwatts(7_000),
        sppt_max: PowerMilliwatts(43_000),
        fppt_min: PowerMilliwatts(7_000),
        fppt_max: PowerMilliwatts(55_000),
    }
}

fn config() -> RuntimeConfig {
    RuntimeConfig::DEFAULT
}

fn initial_state() -> ProfileState {
    ProfileState {
        power_target: PowerEnvelopeTarget {
            spl: PowerMilliwatts(15_000),
            sppt: PowerMilliwatts(15_000),
            fppt: Some(PowerMilliwatts(15_000)),
        },
        active_profile: ProfileName::Balanced,
        is_ac_connected: false,
        charge_end_threshold: 80,
        fan_follows_tdp: true,
        last_dc_state: None,
        active_fan_curve: None,
        active_gpu_clock: None,
        gpu_follows_tdp: false,
        ac_max_performance: true,
        ac_locked: false,
    }
}

/// Per-process-unique temp path, cleared at test entry so re-runs don't see
/// stale persisted state.
fn fresh_temp_path(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("hpd_e2e_{}_{}.toml", label, std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Wait (up to 2s) for the next observed state matching `pred`.
async fn wait_state<F>(state_rx: &mut watch::Receiver<ProfileState>, pred: F) -> ProfileState
where
    F: Fn(&ProfileState) -> bool,
{
    timeout(Duration::from_secs(2), async {
        loop {
            {
                let snapshot = state_rx.borrow();
                if pred(&snapshot) {
                    return snapshot.clone();
                }
            }
            if state_rx.changed().await.is_err() {
                panic!("executor exited before state predicate matched");
            }
        }
    })
    .await
    .expect("state predicate never matched within 2s")
}

/// Poll `check` every 20ms up to `timeout_ms`. Side-effects (backend writes,
/// file persistence) happen AFTER the watch::send in the executor, so the
/// test needs to wait for them separately once the state has settled.
async fn wait_until<F: FnMut() -> bool>(mut check: F, timeout_ms: u64, descr: &str) {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timeout waiting for {}", descr);
}

#[tokio::test]
async fn test_executor_applies_envelope_and_persists() {
    // SetSpl flows through reducer → ApplyPowerEnvelope (backend write) +
    // PersistState (disk). Verify both observable outputs: the backend's
    // recorded call log and the on-disk TOML file.
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    let backend_handle = backend.clone();

    let path = fresh_temp_path("apply_envelope");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    tx.send(Transition::SetSpl(20)).await.unwrap();
    let final_state = wait_state(&mut state_rx, |s| {
        s.power_target.spl == PowerMilliwatts(20_000)
    })
    .await;
    assert_eq!(final_state.power_target.spl, PowerMilliwatts(20_000));

    wait_until(
        || {
            backend_handle.calls().iter().any(|c| {
                matches!(
                    c,
                    RecordedCall::SetTarget(t) if t.spl == PowerMilliwatts(20_000)
                )
            })
        },
        1_000,
        "SetTarget(spl=20000) on backend",
    )
    .await;
    wait_until(|| path.exists(), 1_000, "persisted state file").await;

    let persisted = StatePersister::new(&path)
        .load()
        .await
        .expect("PersistState effect should have written the TOML state file");
    assert_eq!(persisted.power_target.spl, PowerMilliwatts(20_000));

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_restore_defaults_dispatches_composed_effects() {
    // A dirty starting state, off every RestoreDefaults target. Verifies
    // the real executor dispatch loop (not just the pure reducer) drives
    // the composed transition through to the backend for every write
    // MockBackend can observe (power, profile, charge — it has no
    // fan-curve/GPU-clock capability, so those two are only verifiable
    // via the resulting state, exactly as the reducer-level tests already
    // cover in detail).
    let mut dirty = initial_state();
    dirty.active_profile = ProfileName::PowerSaver;
    dirty.charge_end_threshold = 60;
    dirty.active_gpu_clock = None; // MockBackend has no GPU-clock capability

    let backend = MockBackend::new(dirty.power_target.clone(), limits());
    let backend_handle = backend.clone();

    let path = fresh_temp_path("restore_defaults");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        dirty,
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    tx.send(Transition::RestoreDefaults).await.unwrap();
    let final_state = wait_state(&mut state_rx, |s| {
        s.active_profile == ProfileName::Performance
    })
    .await;

    assert_eq!(final_state.power_target.spl, PowerMilliwatts(21_000));
    assert_eq!(final_state.active_profile, ProfileName::Performance);
    assert_eq!(final_state.charge_end_threshold, 80);
    // Cooling target is hpd-managed auto (fan_follows_tdp), not
    // firmware-auto — the 21000mw SPL (fraction 0.5 of limits()'s range)
    // infers the Balanced tier. See reducer.rs's RestoreDefaults tests for
    // the exhaustive pure-function coverage of this.
    assert_eq!(
        final_state.active_fan_curve,
        Some(FanCurveSelection::Preset(FanCurvePreset::Balanced))
    );
    assert!(final_state.fan_follows_tdp);

    wait_until(
        || {
            let calls = backend_handle.calls();
            calls.iter().any(
                |c| matches!(c, RecordedCall::SetTarget(t) if t.spl == PowerMilliwatts(21_000)),
            ) && calls
                .iter()
                .any(|c| matches!(c, RecordedCall::SetProfile(ProfileName::Performance)))
                && calls
                    .iter()
                    .any(|c| matches!(c, RecordedCall::SetChargeThreshold(80)))
        },
        1_000,
        "SetTarget(21000)/SetProfile(Performance)/SetChargeThreshold(80) on backend",
    )
    .await;
    wait_until(|| path.exists(), 1_000, "persisted state file").await;

    let persisted = StatePersister::new(&path)
        .load()
        .await
        .expect("PersistState effect should have written the TOML state file");
    assert_eq!(persisted.active_profile, ProfileName::Performance);
    assert_eq!(persisted.charge_end_threshold, 80);

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_rolls_back_on_hardware_failure() {
    // When set_target fails, the executor must read the real hardware value
    // (via get_target) and reinject SyncPowerTarget. The visible end state
    // on the watch::Receiver must equal the hardware reality, NOT the
    // requested-but-rejected value.
    let hw_real = PowerEnvelopeTarget {
        spl: PowerMilliwatts(10_000),
        sppt: PowerMilliwatts(11_500),
        fppt: Some(PowerMilliwatts(12_500)),
    };
    let backend = MockBackend::new(hw_real.clone(), limits());
    backend.fail_writes.store(true, Ordering::SeqCst);

    let path = fresh_temp_path("rollback");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    // User asks for 20W; backend refuses; executor rolls back to hw_real.
    tx.send(Transition::SetSpl(20)).await.unwrap();
    let rolled_back = wait_state(&mut state_rx, |s| s.power_target == hw_real).await;
    assert_eq!(rolled_back.power_target, hw_real);

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_rolls_back_on_profile_write_failure() {
    // Lote 38 / Audit V2 §4.5.1 regression. Before the uniform-rollback
    // refactor, a failed ApplyPlatformProfile silently left state diverged
    // from hardware. With the new contract the executor reads the
    // kernel-reported profile back and re-injects SyncPlatformProfile,
    // so the watch::Receiver converges on the *hardware reality*, not
    // the requested-but-rejected value.
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    // Override the mock's stored profile to `Balanced` (different from
    // both the test's initial state and the requested Performance, so
    // we can tell rollback by inspection alone).
    *backend
        .profile
        .lock()
        .expect("mock mutex never poisoned in tests") = ProfileName::Balanced;
    backend.fail_writes.store(true, Ordering::SeqCst);

    let path = fresh_temp_path("profile_rollback");
    let persister = StatePersister::new(&path);

    // Start the executor with a profile different from the kernel's
    // (Balanced), so the rollback edge is observable.
    let mut start_state = initial_state();
    start_state.active_profile = ProfileName::PowerSaver;

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        start_state,
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    // User asks for Performance; backend refuses; executor rolls back
    // to the kernel-reported `Balanced`.
    tx.send(Transition::SetProfile(ProfileName::Performance))
        .await
        .unwrap();
    let rolled_back =
        wait_state(&mut state_rx, |s| s.active_profile == ProfileName::Balanced).await;
    assert_eq!(rolled_back.active_profile, ProfileName::Balanced);

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_rolls_back_on_charge_threshold_write_failure() {
    // Mirror of the profile-rollback test for the charge end threshold
    // (Lote 38 / Audit V2 §4.5.1).
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    // Override the mock's stored charge threshold to 75 (different from
    // both the initial state's 80 and the requested 60).
    *backend
        .charge
        .lock()
        .expect("mock mutex never poisoned in tests") = 75;
    backend.fail_writes.store(true, Ordering::SeqCst);

    let path = fresh_temp_path("charge_rollback");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(), // charge_end_threshold = 80
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    // User requests 60%; backend refuses; executor rolls back to 75.
    tx.send(Transition::ChargeThresholdChanged(60))
        .await
        .unwrap();
    let rolled_back = wait_state(&mut state_rx, |s| s.charge_end_threshold == 75).await;
    assert_eq!(rolled_back.charge_end_threshold, 75);

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_propagates_state_to_watch_receiver() {
    // The watch::Receiver is the channel hpd-dbus subscribes to for
    // emitting PropertiesChanged. Verify accepted transitions surface there.
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());

    let path = fresh_temp_path("propagate");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    tx.send(Transition::ChargeThresholdChanged(70))
        .await
        .unwrap();
    let updated = wait_state(&mut state_rx, |s| s.charge_end_threshold == 70).await;
    assert_eq!(updated.charge_end_threshold, 70);

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_config_reload_swaps_runtime_config() {
    // Lote 40 / Audit V2 §4.5.2. The Executor intercepts
    // `Transition::ConfigReload(new_config)` BEFORE reduce() is
    // called and atomically swaps its own RuntimeConfig. The next
    // transition through the reducer must use the new values.
    // Coverage gap before Lote 40: the reducer-level test only
    // covers the no-op contract; nothing verified the executor
    // actually performs the swap.
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    let path = fresh_temp_path("config_reload");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(), // RuntimeConfig::DEFAULT (sppt_factor = 1.15, fppt_factor = 1.25)
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    // Park the state at a known SPL so the post-reload SetSpl is a
    // real envelope change (apply_power_target no-ops when the
    // target is unchanged).
    tx.send(Transition::SetSpl(15)).await.unwrap();
    wait_state(&mut state_rx, |s| {
        s.power_target.spl == PowerMilliwatts(15_000)
    })
    .await;

    // Hot-swap the runtime config: both factors flipped to a clean
    // power-of-two value so the post-reload arithmetic is exact in
    // f32 (15 * 1.15 has f32 rounding noise; 20 * 2.0 doesn't).
    let new_runtime = RuntimeConfig {
        profile_thresholds: ProfileThresholds::DEFAULT,
        sppt_factor: 2.0,
        fppt_factor: 2.0,
        gpu_clock_fractions: GpuClockFractions::DEFAULT,
    };
    tx.send(Transition::ConfigReload(new_runtime))
        .await
        .unwrap();

    // Next SetSpl must use the NEW factors. 20W * 2.0 = 40W = 40000mW
    // — well under sppt_max (43000) and fppt_max (55000), so no clamp.
    // Under the OLD sppt_factor (1.15) the post-reload sppt would be
    // 20000 * 1.15 ≈ 23000, which would fail the assertion below.
    tx.send(Transition::SetSpl(20)).await.unwrap();
    let reloaded = wait_state(&mut state_rx, |s| {
        s.power_target.spl == PowerMilliwatts(20_000)
    })
    .await;
    assert_eq!(
        reloaded.power_target.sppt,
        PowerMilliwatts(40_000),
        "ConfigReload should have swapped sppt_factor to 2.0; expected 20W * 2.0 = 40000mW"
    );
    assert_eq!(
        reloaded.power_target.fppt,
        Some(PowerMilliwatts(40_000)),
        "ConfigReload should have swapped fppt_factor to 2.0; expected 20W * 2.0 = 40000mW"
    );

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_executor_shutdown_persists_state_and_exits_cleanly() {
    // Lote 40 / Audit V2 §4.5.2. The Executor processes
    // `Transition::Shutdown` through the reducer (which emits
    // PersistState), then breaks its `run()` loop. Two contracts
    // the reducer-level test cannot pin down:
    //
    //   1. The spawned `run()` task joins on its own — the test does
    //      NOT call `abort()`.
    //   2. The on-disk state file matches the last successful
    //      mutation written *before* Shutdown was sent (the post-
    //      shutdown PersistState writes the same `ProfileState` that
    //      `state_tx` already held).
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    let path = fresh_temp_path("shutdown");
    let persister = StatePersister::new(&path);

    let (tx, rx) = mpsc::channel(32);
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    // Seed the state with something distinct from initial_state()'s
    // 15W so the post-shutdown TOML round-trip is observably
    // different from "the persister just dumped its boot state".
    tx.send(Transition::SetSpl(22)).await.unwrap();
    wait_state(&mut state_rx, |s| {
        s.power_target.spl == PowerMilliwatts(22_000)
    })
    .await;
    wait_until(
        || path.exists(),
        1_000,
        "first PersistState writes the file",
    )
    .await;

    // Drop our own sender so the executor's transition_rx can fully
    // close after Shutdown is processed — keeps the test honest
    // about "the task exits without external abort". The Executor's
    // internal_tx clone still keeps the channel half-alive, but
    // Shutdown's explicit `break` in run() is what actually breaks
    // the loop. Demonstrating that contract is the whole point.
    tx.send(Transition::Shutdown).await.unwrap();
    drop(tx);

    // The task must drain and exit on its own. 5s mirrors the
    // hpd-daemon main.rs ceiling (well below systemd's 90s
    // TimeoutStopSec).
    let join = timeout(Duration::from_secs(5), exec_handle)
        .await
        .expect("Executor should drain within 5s of Shutdown");
    join.expect("Executor task must not panic during shutdown drain");

    // The on-disk state matches the last accepted mutation. Reading
    // it back through a fresh StatePersister proves the bytes hit
    // disk before run() returned, not just that the in-memory
    // ProfileState was right.
    let persisted = StatePersister::new(&path)
        .load()
        .await
        .expect("Shutdown must persist state to disk before the run() loop breaks");
    assert_eq!(persisted.power_target.spl, PowerMilliwatts(22_000));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_resume_rereads_ac_state_from_hardware() {
    // Scenario C: the in-memory state says "on battery", but the hardware
    // (MockBackend) reports AC connected — as if the charger was plugged while
    // the device was suspended and the udev event was missed. SystemResumed
    // must re-read the real AC from hardware and apply the AC policy (force
    // max + lock), not trust the stale in-memory value.
    let backend = MockBackend::new(initial_state().power_target.clone(), limits());
    backend.ac_connected.store(true, Ordering::SeqCst); // hardware: on AC

    let path = fresh_temp_path("resume_ac");
    let persister = StatePersister::new(&path);
    let (tx, rx) = mpsc::channel(32);
    // initial_state() is on battery (is_ac_connected = false), lock on.
    let (executor, mut state_rx) = Executor::new(
        backend,
        initial_state(),
        limits(),
        config(),
        rx,
        tx.clone(),
        persister,
    );
    let exec_handle = tokio::spawn(executor.run());

    tx.send(Transition::SystemResumed).await.unwrap();
    let settled = wait_state(&mut state_rx, |s| {
        s.is_ac_connected && s.power_target.spl == PowerMilliwatts(35_000)
    })
    .await;

    assert!(
        settled.is_ac_connected,
        "AC re-read from hardware on resume"
    );
    assert!(settled.ac_locked, "locked on AC after the re-read");
    assert_eq!(settled.power_target.spl, PowerMilliwatts(35_000));

    exec_handle.abort();
    let _ = std::fs::remove_file(&path);
}
