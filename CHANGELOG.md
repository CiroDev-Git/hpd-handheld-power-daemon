# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each entry references the Audit lote that introduced the change. The audit
itself lives at [`docs/audit/AUDIT_V1.md`](docs/audit/AUDIT_V1.md) and the
remediation plan at [`docs/audit/REMEDIATION_PLAN_V1.md`](docs/audit/REMEDIATION_PLAN_V1.md).

---

## [Unreleased]

### Added

- **`missing_docs` lint enabled workspace-wide** — every public item
  carries a `///` doc comment and every module file opens with a
  `//!` block. CI runs with `-D warnings` so this is effectively an
  error in CI. Rustdoc inline documentation now exists across the 6
  crates that were missing it post-Lote-21: `hpd-error`,
  `hpd-netlink`, `hpd-backend-asus`, `hpd-dbus`, `hpd-cli`,
  `hpd-daemon`. Documentation coverage now matches the L-1→L4
  workspace layout.
  *(Lote 43 — Audit V2 Phase 3)*
- **Per-crate `README.md` for all 9 crates.** Each crate now ships a
  one-page README covering purpose, workspace layer, dependencies,
  a runnable example, and the `cargo doc` invocation that opens the
  generated rustdoc. The daemon README additionally documents the
  composition root's architecture diagram, signal handling, and the
  on-disk filesystem layout. Useful entry-point for contributors who
  want to navigate the workspace without opening every `lib.rs` first.
  *(Lote 44 — Audit V2 Phase 3)*
- **`docs/ARCHITECTURE.md` — global architecture document** (~550
  lines, 12 sections). Human-oriented walk-through covering the
  L-1→L4 workspace layout, the `Transition → reducer → Effect →
  Executor` pipeline (with ASCII diagrams of the data flow and
  rollback contract), the multi-runtime concurrency layout
  including the dedicated `std::thread` for `tokio-udev`'s `!Send`
  socket, the full lifecycle / signal matrix, the polkit fail-closed
  contract, the persistence and configuration models, a "where to
  look for things" lookup table, the recipes for adding a new
  vendor backend or D-Bus method, and a curated reading order for
  newcomers. The root README and crate READMEs now link here as the
  canonical design reference; `CLAUDE.md` remains the
  assistant-oriented variant.
  *(Lote 45 — Audit V2 Phase 3)*
- **`docs/dev/LINUX.md` — Linux development guide** (~300 lines, 11
  sections). End-to-end loop on a Linux host: toolchain pinning
  via `rust-toolchain.toml`, per-distro build-dep one-liners,
  workspace command reference, the full feature matrix CI runs,
  both running paths (production-shape `install.sh` walkthrough
  *and* iterative `cargo run` against the system bus with policy
  files installed), logging via `RUST_LOG`+`journalctl`, D-Bus
  introspection with `busctl`/`dbus-monitor`, polkit debugging
  with `pkaction`/`pkcheck`, manual suspend/resume + AC plug
  testing, filesystem layout reference, and 6 common pitfalls
  with diagnoses. Entry point for any contributor working on a
  Linux dev host.
  *(Lote 46 — Audit V2 Phase 4)*

---

## [1.0.0] — 2026-05-24

This release jumps directly from `0.1.0` to `1.0.0`. The intermediate
`0.2.0` trajectory the project briefly advertised was abandoned: the
cumulative breaking changes accumulated through the V1 remediation
lotes (7, 8, 9, 10, 11, 16, 18, 20) constitute a SemVer-major bump by
themselves, and the post-Lote-22 base is the right place to commit to
backwards compatibility. From `1.0.0` onward the public surface
(D-Bus interface `dev.cirodev.hpd.PowerDaemon1`, `hpdctl` subcommands,
on-disk state at `/var/lib/hpd/state.toml`, polkit action IDs in
`dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`) follows SemVer
strictly.

### ⚠ Breaking — operators / packagers

- **State file location** — Persistent state moved from
  `/var/tmp/hpd_state.toml` (world-writable, symlink-race surface) to
  `/var/lib/hpd/state.toml`. Under systemd the path is resolved via the
  `STATE_DIRECTORY` env var injected by `StateDirectory=hpd`. There is
  no automatic migration: a fresh state file is created on first boot
  after upgrade.
  *(Lote 7 — Audit §7.4)*
- **systemd unit consolidation** — `dist/systemd/hpd.service` removed.
  `package/hpd.service` rewritten with full sandboxing
  (`ProtectSystem=strict`, `StateDirectory=hpd`, `PrivateNetwork`,
  `NoNewPrivileges`, `CapabilityBoundingSet=`,
  `SystemCallFilter=@system-service`, complete `ReadWritePaths` for
  all sysfs roots actually written). Packagers should rebuild against
  the new unit.
  *(Lote 7 — Audit §7.1)*

### ⚠ Breaking — D-Bus clients

- **`active_profile` property format** — Values are now stable
  lowercase kebab-case (`power-saver`, `balanced`, `performance`) or
  the raw custom string. Previously the daemon emitted Rust `Debug`
  output (`PowerSaver`, `Balanced`, `Performance`, `Custom("foo")`),
  which was an unstable internal representation. The new format
  roundtrips through `set_profile` and `ProfileName::FromStr`.
  *(Lote 9 — Audit §3.6)*
- **`PropertiesChanged` signals are now emitted.** Previously every
  property change was a silent in-memory update; D-Bus clients had
  to poll. They now receive
  `org.freedesktop.DBus.Properties.PropertiesChanged` whenever
  `current_spl`, `active_profile` or `charge_end_threshold` change.
  This may surface latent bugs in clients that previously assumed
  signals would never fire.
  *(Lote 10 — Audit §3.1)*
- **`set_preset` value set changed.** `silent`, `performance`, `turbo`
  are rejected with a clear error. Use `eco`, `balanced`, `max`
  instead. Reason: the old `performance` overloaded with
  `ProfileName::Performance` while meaning a different thing
  (midpoint TDP vs. max cooling profile). The rename is intentional
  and aliases were not kept so the confusion cannot resurface.
  *(Lote 11 — Audit §3.7)*

### ⚠ Breaking — CLI clients

- **`hpdctl preset turbo|silent|performance` is gone.** Use
  `hpdctl preset max|eco|balanced`. The CLI subcommand error message
  guides users to the new names.
  *(Lote 11 — Audit §3.7)*

### ⚠ Breaking — internal API (Rust)

- **`HpdError::Backend`** — Changed from struct variant
  `{ reason: String }` to tuple variant wrapping the new
  `BackendError` (`HpdError::Backend(BackendError)`). External Rust
  consumers (none today) would need to migrate `match` arms.
  *(Lote 8 — Audit §4.1)*
- **`HwBackend` trait surface — optional capability accessors.**
  Pre-1.0 `HwBackend` was a supertrait of `PowerEnvelope +
  ChargeControl + PlatformProfile + FanControl`, forcing every
  vendor to implement all four. It is now a standalone trait with
  one mandatory accessor (`fn power(&self) -> &dyn PowerEnvelope`)
  and three optional ones (`fn charge / profile / fan(&self) ->
  Option<&dyn …>`) defaulting to `None`. Vendors with partial
  hardware support (e.g. a future Steam Deck backend with no ACPI
  `platform_profile`) now implement only the accessors they can
  honour. ASUS continues to expose all four (`Some(...)` for each).
  External Rust consumers (none today) implementing `HwBackend`
  must migrate from "blanket impl over the four sub-traits" to
  "explicit accessors". The D-Bus / CLI / on-disk surfaces are
  unchanged. Side effect: the 11-method blanket-delegation block on
  `AsusBackend` (V1 §12.1 smell) disappears entirely.
  *(Lote 39 — Audit V1 §16.2 / V2 §4.18.2)*

### ⚠ Breaking — packagers / developers

- **Simulator mode is now a build-time feature.** Previously the
  `mock` Cargo feature on `hpd-sysfs` was always enabled by
  `hpd-daemon`, so production binaries shipped with the simulator
  code path linked in. Now the path is compiled in only with
  `cargo build -p hpd-daemon --features simulator`; the default
  feature set is `vendor-asus` only. `HPD_SIMULATOR=1` is a no-op
  on builds without the `simulator` feature. macOS / dev hosts
  must add `--features simulator`.
  *(Lote 16)*
- **System-bus policy widened; polkit now gates access.**
  `dev.cirodev.hpd.conf` previously only allowed `root` and members
  of the `wheel` group to send to the daemon. It now lets any user
  send method calls; per-action authorization is enforced by polkit
  via the new `dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`
  action IDs. Operators must ensure polkit is installed and that an
  auth agent (polkit-gnome, kde-polkit, `pkttyagent` for terminal
  use) is available, otherwise privileged calls will fail with
  `AuthFailed`. `install.sh` deploys the policy file to
  `/usr/share/polkit-1/actions/`.
  *(Lote 20)*

### Added

- **`auto_cooling` D-Bus property** on
  `dev.cirodev.hpd.PowerDaemon1`. Read-only `bool` backed by
  `ProfileState::fan_follows_tdp` — `true` when the daemon is
  inferring the cooling profile from the TDP envelope, `false`
  after an operator has called `set_profile` (until they call
  `set_fan_auto` to re-enable inference). Closes the audit
  finding that the mode was silently flipped by `set_profile`
  with no way for clients to observe it. `hpdctl status` now
  surfaces it as "Cooling Mode: auto (follows TDP) | manual".
  PropertiesChanged is emitted by the daemon's
  `spawn_properties_changed_emitter` task on every flip.
  *(Lote 42 — Audit V1 §3.8 / V2 §4.7.1)*
- **`spawn_properties_changed_emitter` task** in `hpd-daemon/main.rs`
  watches the executor's state channel and calls zbus's generated
  `<prop>_changed` notifiers for each property whose underlying field
  changed (per-field diff to avoid spam).
  *(Lote 10 — Audit §3.1)*
- **`hpd-error` crate** (workspace layer L-1) holding the canonical
  `HpdError`, `SysfsError` and the new structured `BackendError`. Only
  dependency is `thiserror`. License `GPL-3.0-or-later`. Public surface
  is doc-commented end-to-end.
  *(Lote 8 — Audit §4.1)*
- **`BackendError::ParseFailed { field, raw, reason }`** structured
  parse-error variant. Backends now produce typed parse errors instead
  of `format!`-stringified messages.
  *(Lote 8)*
- **`ProfileName: Display`** — symmetric with `FromStr`. Documented as
  the stable D-Bus contract.
  *(Lote 9 — Audit §3.6)*
- **`TdpPreset` enum** with `Eco | Balanced | Max` variants and
  `Display`/`FromStr` symmetric in kebab-case. Replaces the previous
  `SystemPreset` whose `Performance` variant collided semantically
  with `ProfileName::Performance` (the former meant midpoint TDP, the
  latter meant max cooling profile).
  *(Lote 11 — Audit §3.7)*
- **`ProfileName::FromStr`** now accepts ACPI-native aliases (`quiet`,
  `low-power`) and preserves unknown values as
  `ProfileName::Custom(...)` instead of erroring.
  *(Lote 9)*
- **`uninstall.sh`** mirror of `install.sh` with optional `--purge`
  flag to remove `/var/lib/hpd/`.
  *(Lote 7)*
- **5 new tests** in `hpd-capabilities/profile.rs` covering Display
  format, Display→FromStr roundtrip, alias acceptance, custom
  preservation, and empty rejection.
  *(Lote 9)*
- **`test_set_spl_overflow_rejected`** in `hpd-core/reducer.rs`
  guarding against `watts * 1000` wrap-around.
  *(Lote 5 — Audit §3.3)*
- **`test_ac_plugged_persists_last_dc_target_even_when_target_unchanged`**
  in `hpd-core/reducer.rs` guarding the `AcPowerChanged` persistence
  hole.
  *(Lote 6 — Audit §3.5)*
- **Test fixture and assertion for canonical ASUS `ppt_pl3_fppt`
  attribute** in `hpd-backend-asus/power.rs`. Regression coverage for
  the silent-fallback bug.
  *(Lote 4 — Audit §3.2)*
- **`.gitignore`** entries for coverage (`*.profraw`, `*.profdata`,
  `coverage/`, `tarpaulin-report.*`), packaging output (`/dist/release/`,
  `*.deb`, `*.rpm`), logs, tmp artifacts, plus editor backups
  (`*~`, `*.bak`, `.#*`) and `.env*` files.
  *(Lotes 1, 17)*
- **`hpd-capabilities::testing::MockBackend`** — Arc-shared in-memory
  backend implementing all four L2 capability traits plus the blanket
  `HwBackend`. Records every write in `write_log` and can simulate
  hardware failure via `fail_writes`. Gated behind the new `testing`
  Cargo feature so production builds never link it.
  *(Lote 14)*
- **`crates/hpd-core/tests/executor_e2e.rs`** — three integration
  tests exercising the full Transition → reducer → Effect → backend
  pipeline: happy path with disk persistence, hardware-write rollback
  via `SyncPowerTarget` re-injection, and `watch::Receiver`
  propagation.
  *(Lote 14)*
- **15 reducer branch-coverage tests** in `hpd-core/src/reducer.rs`
  covering `AcPowerChanged` (debounce, plug, unplug with/without
  `last_dc_target`), `SystemResumed`, `EnableFanAuto`,
  `ChargeThresholdChanged`, `SetSpl` boundaries (min/max ±1),
  `SetEnvelope` FPPT-below-SPPT invariant, and `SyncPowerTarget`
  rollback.
  *(Lote 13)*
- **`PowerMilliwatts::from_watts` / `as_watts`** and the
  `MILLIWATTS_PER_WATT` constant — single source of truth for the
  W↔mW conversion previously inlined as `* 1000` / `/ 1000` across
  reducer, executor, daemon, D-Bus and ASUS backend.
  *(Lote 15)*
- **Domain constants** centralising the previously magic literals:
  `ProfileThresholds::DEFAULT` (0.33/0.67), `DEFAULT_CHARGE_THRESHOLD`
  (80), `SPPT_FACTOR` / `FPPT_FACTOR` (1.15 / 1.25),
  `TRANSITION_CHANNEL_CAPACITY` (32), ASUS-specific
  `ASUS_DEFAULT_SPPT_MAX_MW` / `ASUS_DEFAULT_FPPT_MAX_MW`, and a
  private `FanIndex { Cpu = 1, Gpu = 2 }` enum replacing bare
  `fan{1,2}_input` integers.
  *(Lote 15)*
- **Per-vendor Cargo features on `hpd-daemon`** — `vendor-asus`
  (default), `vendor-lenovo`, `vendor-valve`, and `simulator`. Each
  vendor flag gates one L1 backend crate via `dep:`. Production
  release builds no longer link Lenovo/Valve stubs or the MockSysfs
  path. `simulator` implies `vendor-asus` because the simulator
  currently only models ASUS firmware. With no vendor feature
  enabled the daemon still compiles and exits cleanly at startup.
  *(Lote 16)*
- **`README.md`, `LICENSE` (GPL-3.0), `.gitignore` expansion** —
  the repository is now presentable: hardware support matrix,
  install / usage / development sections, license recognised by
  GitHub.
  *(Lote 17)*
- **`DaemonConfig` + on-disk configuration** at
  `/etc/hpd/config.toml`. Every field is optional and defaults are
  applied per-field via `#[serde(default)]`, so partial / empty TOML
  files never break the daemon and adding fields never breaks
  existing configs. Missing or corrupt file → log + fall back to
  defaults (daemon survives). The new `package/hpd-example.toml` ships
  as `/etc/hpd/config.toml.example` to document the schema.
  *(Lote 18)*
- **`Transition::ConfigReload(RuntimeConfig)`** — reintroduced as a
  functional hot-reload pathway. The `RuntimeConfig` type
  (`hpd-capabilities::profile`) bundles the runtime-tunable subset
  (`profile_thresholds`, `sppt_factor`, `fppt_factor`) that the
  reducer consumes on every transition. The Executor intercepts
  `ConfigReload` before `reduce()` and swaps its own copy
  atomically; the next transition uses the new values.
  *(Lote 18)*
- **SIGHUP handler in `hpd-daemon`** — re-reads
  `/etc/hpd/config.toml` and pushes a `ConfigReload` transition.
  Mapped from `systemctl reload hpd` via `ExecReload=/bin/kill -HUP
  $MAINPID` in the unit file. `ConfigurationDirectory=hpd` also
  added.
  *(Lote 18)*
- **Graceful shutdown** — `hpd-daemon` now listens for both SIGINT
  (Ctrl+C) and SIGTERM (systemd). On either signal it sends the new
  `Transition::Shutdown` to the executor, which flushes the
  in-memory state to disk via `PersistState` and then breaks its
  `run()` loop. The daemon then awaits the executor (5s timeout
  guard, well below systemd's 90s `TimeoutStopSec`) and closes the
  D-Bus connection before returning. Previously the runtime simply
  dropped on Ctrl+C and any pending state mutation was lost.
  *(Lote 19)*
- **Polkit per-action authorization.** The D-Bus interface now
  delegates access control to polkit instead of the coarse
  `group="wheel"` policy that used to live in
  `dev.cirodev.hpd.conf`. Three action IDs are declared in
  `package/polkit/dev.cirodev.hpd.policy`:
  `dev.cirodev.hpd.{set-tdp,set-charge,set-profile}`. TDP and
  charge changes require `auth_admin` (prompt on every call);
  cooling-profile changes use `auth_admin_keep` (5-minute cache).
  Property reads remain unauthenticated. The check is **fail-closed**
  — any error talking to polkit results in `AuthFailed` — and is
  short-circuited to `true` under `#[cfg(feature = "simulator")]`
  so session-bus dev hosts remain usable. Implementation is a small
  manual call to `org.freedesktop.PolicyKit1.Authority` over zbus;
  no extra dependency added.
  *(Lote 20)*
- **`#![warn(missing_docs)]` on `hpd-capabilities`.** Every public
  trait, struct, enum, variant, field and method in the L2 capability
  crate now carries a `///` doc comment, and the lint is enforced at
  the crate root so future additions can't slip in undocumented.
  `cargo doc -p hpd-capabilities --no-deps` renders without warnings.
  `hpd-core` and `hpd-sysfs` got the same documentation pass at item
  level (module-level `//!` headers + `///` on every pub item) but
  without enabling the lint yet — a follow-up will turn it on once
  the existing items are stable.
  *(Lote 21)*
- **GitHub Actions CI** (`.github/workflows/ci.yml`). Two jobs:
  `build-test` on Ubuntu runs `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `cargo build --workspace --release`;
  `build-macos-simulator` on macOS verifies the
  `cargo build -p hpd-daemon -p hpd-cli --features hpd-daemon/simulator`
  path keeps working. Both jobs cache cargo artefacts via
  `Swatinem/rust-cache` and cancel stale runs on the same ref.
  README gains the CI + license badges.
  *(Lote 22)*
- **`rust-toolchain.toml`** pinning the channel to `stable` with
  `rustfmt` and `clippy` components, so contributor toolchains stay
  in lockstep with CI.
  *(Lote 22)*

### Changed (Lote 22 follow-on)

- **Lint policy refactored**: `clippy::unwrap_used`,
  `clippy::expect_used`, and `clippy::panic` are no longer applied
  globally via `RUSTFLAGS` in `.cargo/config.toml`. They now live as
  `#![cfg_attr(not(test), warn(...))]` attributes on each crate's
  `lib.rs` / `main.rs`, so production code is held to the strict
  bar while test modules can keep their idiomatic `.unwrap()` /
  `.expect()` / `panic!` without per-site `#[allow]` boilerplate.
  This is what unblocks the `-D warnings` invocation in CI.
- **`Transition::SystemResumed` reducer arm** rewritten to build its
  `Vec<Effect>` with the `vec![]` macro instead of `push`-after-`new`
  (clippy::vec_init_then_push).
- **`hpd-daemon` netlink thread** now logs and exits the worker
  thread on `tokio::runtime::Builder::build()` failure instead of
  `.expect("Failed to build local tokio runtime for netlink")`. The
  main daemon stays up; AC plug events are simply missed.
- **`hpd-sysfs::mock::testing`** module gets an inner
  `#![allow(clippy::unwrap_used, clippy::expect_used)]` matching the
  pattern already in `hpd-capabilities::testing`, plus a `Default`
  impl on `MockSysfs` to satisfy `clippy::new_without_default`.
- **`hpd-netlink`** unused `error` / `debug` imports moved inside
  the Linux-only submodule so the macOS build no longer warns.

### Changed

- **Profile inference is now done in a single place.** The reducer
  (via `apply_target_and_profile` -> `infer_profile_from_spl`) is the
  sole authority; the post-reduce inference block in the executor was
  redundant (and worse, used a slightly different rule for the
  degenerate `range == 0` case). The `Executor::infer_profile_for_tdp`
  method and its caller were removed; `inference::infer_profile_from_spl`
  now documents the `range == 0` -> `Balanced` fallback.
  *(Lote 12 — Audit §3.4)*
- **Daemon logging** — Migrated from `FmtSubscriber::with_max_level`
  to `tracing-subscriber` with `EnvFilter`. Now honours `RUST_LOG`;
  default is `hpd=info,warn`. systemd unit now sets
  `Environment="RUST_LOG=hpd=info,warn"`.
  *(Lote 3 — Audit §13.6)*
- **Reducer logging** — All `println!` statements in
  `hpd-core/reducer.rs` replaced by `tracing::info!` with structured
  key-value fields (`preset=...`, `action=...`).
  *(Lote 3 — Audit §5.1)*
- **`info!` calls in daemon** restructured with `key=value` fields
  (`vendor=...`, `board=...`, `spl_min_w=...`).
  *(Lote 3 — Audit §17.7)*
- **All comments and error messages** translated to English. No
  remaining Spanish characters or Spanish-derived typos
  (`Convertion`, `pluged`, `Smaal`, `kenel`, `Avisamos`, `Debería`,
  …) anywhere under `crates/`, `install.sh`, `Cargo.toml` or
  `.cargo/`. `rg "[áéíóúñ¿¡]"` returns empty.
  *(Lote 2 — Audit §10)*
- **`install.sh`** switched to `set -euo pipefail`, uses
  `install -D` with explicit modes, pre-creates `/var/lib/hpd` 0700,
  uses `try-reload-or-restart` for D-Bus, and prints the canonical
  paths at the end.
  *(Lote 7 — Audit §17.3, §17.4)*
- **ASUS backend** code paths cleaned of repeated
  `map_err(|e| HpdError::Backend { reason: format!(...) })`. `?`
  is now used directly for sysfs failures (via `#[from]`) and
  `BackendError::ParseFailed` for typed parse errors. ~30 LOC of
  ceremony removed.
  *(Lote 8 — Audit §12.2)*
- **ASUS attribute names** factored into constants `ATTR_SPL`,
  `ATTR_SPPT`, `ATTR_FPPT` with a comment pointing at the upstream
  kernel driver and the verified board (`RC73XA`).
  *(Lote 4)*

### Fixed

- **ASUS `ppt_fppt` vs `ppt_pl3_fppt` mismatch** — `get_limits()`
  was reading the non-existent attribute `ppt_fppt/max_value` and
  silently falling back to a hard-coded `53000` mW. The daemon
  therefore capped FPPT 2 W below the real hardware limit. Now reads
  the canonical `ppt_pl3_fppt/max_value` (verified `55` W on ROG
  Xbox Ally X / board `RC73XA`).
  *(Lote 4 — Audit §3.2)* — **CRITICAL**
- **`SetSpl` overflow** — `watts * 1000` could wrap around in release
  builds for huge `u32` inputs (e.g. `u32::MAX` from a malformed
  D-Bus call), producing a small wrapped value that spuriously
  passed the subsequent range check. Now uses `checked_mul` and
  returns `HpdError::InvariantViolation` on overflow.
  *(Lote 5 — Audit §3.3)*
- **`AcPowerChanged` persistence hole** — When the system was
  already at the Turbo target (e.g. stale boot state or repeated AC
  events), `apply_target_and_profile` skipped `Effect::PersistState`
  and the mutated `last_dc_target` / `is_ac_connected` would be
  lost on the next reboot. Now always emits `PersistState` if the
  inner reduce did not.
  *(Lote 6 — Audit §3.5)*
- **`SetPreset::Performance` overflow defense** — midpoint
  `(min_w + max_w) / 2` now uses `saturating_add` for resilience
  against pathological `device_limits`.
  *(Lote 5)*

### Removed

- **`hpd-backend-lenovo` placeholder crate.** It only implemented
  `PowerEnvelope` (returning `FeatureUnsupported`), never implemented
  `HwBackend`, and was never wired into the daemon's vendor cascade —
  enabling `vendor-lenovo` produced a daemon that would refuse to
  detect any hardware. Shipping it as a public `1.0.0` crate would
  lock the project to honour a contract it never delivered. The
  matching `vendor-lenovo` Cargo feature on `hpd-daemon` is also
  removed. Reintroduce as a real backend in a 1.x minor when an
  implementation lands.
  *(Lote 26 — Audit V2 §4.16.1)*
- **`hpd-backend-valve` placeholder crate.** Same shape as the
  Lenovo crate above: stub implementation, unwired in `main.rs`,
  shipping as `1.0.0` would have committed the project to an
  interface contract it never delivered. The matching `vendor-valve`
  Cargo feature on `hpd-daemon` is also removed. Reintroduce as a
  real Steam Deck backend in a 1.x minor.
  *(Lote 27 — Audit V2 §4.16.2)*
- **`SystemPreset` enum** and the `silent` / `performance` / `turbo`
  string aliases that mapped to it. Replaced by `TdpPreset` (see
  Added). No backwards-compat aliases kept.
  *(Lote 11 — Audit §3.7)*
- **`Effect::EmitDbusPropertiesChanged`** variant and all its push
  sites in the reducer + the no-op match arm in the executor. The
  daemon now emits PropertiesChanged via a dedicated watcher task,
  so the synthetic effect is dead weight. The manual deduplication
  block in `AcPowerChanged` (`if !output.effects.contains(...)`)
  also disappears.
  *(Lote 10 — Audit §3.10)*
- **Dead types and methods**
  - `BatteryPercent` unit (unused). *(Lote 1)*
  - `HpdError::DbusClient(String)` variant (unused). *(Lote 1)*
  - `SysfsIo::is_writable` method and its `Real`/`Mock` impls
    (defined but never called). *(Lote 1)*
  - `Transition::ConfigReload` variant and its reducer arm. Reintroduced
    in Lote 18 with a real `RuntimeConfig` payload and a SIGHUP-driven
    hot-reload pathway.
    *(Lote 1)*
- **Empty `/src/` directory** at the repository root (leftover from
  an earlier `cargo init`). *(Lote 1)*
- **Duplicate broken systemd unit** `dist/systemd/hpd.service` —
  pointed to a non-existent binary path and had incomplete
  `ReadWritePaths`. *(Lote 7)*
- **Duplicate `SysfsError` type** — both `hpd-sysfs/src/error.rs`
  and the inner `SysfsError` of `hpd-capabilities/src/error.rs`
  removed in favour of a single source in `hpd-error`.
  *(Lote 8)*
- **`thiserror` dependency** from `hpd-capabilities` (no longer used
  there after the error types moved out). *(Lote 8)*

---

## [0.1.0] — 2026-05-19

Initial working set. Functional ASUS backend (TDP / charge / fan /
profile), D-Bus interface, CLI (`hpdctl`), realtime monitor,
suspend/resume handling, AC plug/unplug detection via udev. Lenovo
and Valve backends are placeholders.

See `git log --before=2026-05-22` for commit-level history of this
release.
