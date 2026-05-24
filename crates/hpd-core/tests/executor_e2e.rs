// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end pipeline tests for the L3 `Executor`.
//!
//! These exercise the full Transition → reducer → Effect → backend loop
//! against an in-memory `MockBackend`, including:
//!
//! * the happy path (apply + persist),
//! * the rollback path on hardware-write failure,
//! * the `watch::Receiver` propagation that hpd-dbus relies on for
//!   `PropertiesChanged`.
//!
//! Shutdown note: the Executor holds an internal clone of the transition
//! `mpsc::Sender` for self-injecting rollback events, so the channel can
//! never close while the executor is alive — `run()` is designed to live
//! for the lifetime of the process. Tests therefore drive the executor on
//! a `tokio::spawn`'d task and `abort()` it once the observable side effects
//! are in place; they never await `run()` to completion.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::{ProfileName, RuntimeConfig};
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
        sppt_max: PowerMilliwatts(43_000),
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
        last_dc_target: None,
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
