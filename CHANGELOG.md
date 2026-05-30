# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each entry references the Audit lote that introduced the change. The
underlying audit and remediation plans are maintained internally and are
not part of the published repository.

---

## [Unreleased]

### Changed

- **Unified cooling into a single lever.** Cooling is now one concept:
  `hpdctl cool set silent|balanced|aggressive` programs the platform
  profile *and* the matching fan curve together, and `hpdctl cool auto`
  lets the daemon pick the level from the TDP. The status dashboard
  collapses the former three lines (cooling profile, cooling mode, fan
  curve) into one `Cooling: <level> (auto|manual)`.
  - `fan_curve_follows_profile` now defaults to **`true`** so the profile
    and curve always move together; set it to `false` to drive the curve
    independently (advanced).
  - **The `fan` CLI namespace was removed entirely.** All cooling is now
    under `cool` (`set` / `auto` / `reset` / `get` / `curve`). The raw
    platform profile and fan curve stay available over D-Bus
    (`set_profile` / `set_fan_curve`) for advanced/decoupled use. New
    D-Bus method `SetCoolingLevel`.
  - `hpdctl cool curve` draws the active fan curve (temperature → speed),
    backed by a new D-Bus `GetFanCurve` method.
  - Fan-curve presets validated on the ROG Xbox Ally X (RC73XA):
    `silent` ≈58 °C, `balanced` ≈68 °C, `aggressive` ≈95 °C (fans maxed)
    under a sustained all-core load — `balanced` solves the original
    ~87 °C firmware behaviour.
  - Added a full user manual ([`docs/MANUAL.md`](docs/MANUAL.md) /
    [`docs/MANUAL-es.md`](docs/MANUAL-es.md)) and a Spanish cooling
    explainer.

### Added

- **Custom fan curves (ASUS / ROG Xbox Ally X)** — the daemon can now
  program the EC-mediated custom fan curve exposed by the
  `asus_custom_fan_curve` hwmon, instead of only selecting an ACPI
  platform profile. The firmware's default curve is defined only up to
  ~62°C and tops out near 22% duty, so the chip runs hot under sustained
  load; the new curves extend a monotonic ramp out to ~92°C.
  - Three named presets — `silent`, `balanced`, `aggressive` —
    calibrated against the ROG Xbox Ally X (RC73XA). Curves are written
    as EC-mediated auto-points (never raw PWM), so the embedded
    controller keeps running the last curve even if the daemon stops.
  - New CLI: `hpdctl fan curve set <preset>`, `hpdctl fan curve get`,
    `hpdctl fan curve reset` (back to firmware automatic).
  - New D-Bus methods `SetFanCurve`, `ResetFanCurve` and a read-only
    `fan_curve` property on `dev.cirodev.hpd.PowerDaemon1`.
  - New polkit action `dev.cirodev.hpd.set-fan-curve` (`auth_admin_keep`;
    `wheel` members are granted it passwordless by `49-hpd.rules`).
  - New config keys: `default_fan_curve` (preset applied on first boot,
    defaults to `balanced`) and `fan_curve_follows_profile` (when true,
    a platform-profile change also swaps the matching curve).
  - The active curve is re-applied on resume from suspend, fixing the
    bug where the fans could blast at full speed after wake.
  - The active curve is also re-asserted after any platform-profile
    change, because writing the ACPI profile can make the EC drop the
    custom curve — so TDP auto-follow no longer silently loses it.
  - New L2 capability `FanCurveControl` + `fan_curve()` accessor on
    `HwBackend` (additive; existing backends default to `None`).
- **Live power, fan & temperature telemetry** — `hpdctl status` /
  `monitor` now show the **actual SoC power draw** (vs the configured TDP
  cap), CPU/GPU temperatures and CPU/GPU fan RPM, alongside the active
  fan curve, via the D-Bus `GetThermalStatus` method (now a 5-tuple
  including `soc_power_mw`). This revives the previously-unsurfaced
  `FanControl` read path and adds a `ThermalSensors` capability (CPU
  `k10temp` Tctl + GPU `amdgpu` edge + SoC power from `amdgpu`
  `power1_input`, located by hwmon name). Seeing actual power makes the
  manual-clamp case visible: a low cooling level holds the draw well
  below a high TDP cap.

### Fixed

- **Fan-RPM read targeted the wrong hwmon** — the reader scanned
  `/sys/class/hwmon` by lowest index and could latch onto the unrelated
  `acpi_fan` node (which also exposes a `fan1_input`) instead of the
  `asus` node. It now resolves the sensor node by its `name` attribute,
  which is stable across boots and driver-load order.

---

## [1.0.0] — 2026-05-28

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
  action IDs. `wheel`-group members (the device owner) are granted
  these actions without a prompt by `package/polkit/49-hpd.rules` (see
  *Fixed* below); non-`wheel` callers hit the `auth_admin` defaults and
  need a polkit auth agent (polkit-gnome, kde-polkit, or `pkttyagent`
  for terminal use), otherwise their privileged calls fail with
  `AuthFailed`. `install.sh` deploys the policy file to
  `/usr/share/polkit-1/actions/` and the rule to
  `/usr/share/polkit-1/rules.d/`.
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

- **`/usr/share/hpd/VERSION` sidecar shipped by `install.sh`** —
  single-line text file (`X.Y.Z`) written at install time by
  extracting the workspace `Cargo.toml` `version`. Consumed by
  external clients (e.g. `hpd-decky-plugin`) that need the installed
  daemon version without parsing `journalctl` or requiring
  `systemd-journal` group membership. `uninstall.sh` removes it and
  the empty `/usr/share/hpd` directory. No code path inside the daemon
  reads this file; it is purely a consumer-facing affordance.
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
- **`docs/dev/MACOS.md` — macOS development guide** (~250 lines, 9
  sections). Simulator-first workflow for Mac dev hosts. Includes
  an explicit "what works / what doesn't" matrix (no real sysfs,
  no udev, no logind, no polkit), Homebrew + Xcode CLT
  prerequisites, two recipes for starting the session D-Bus
  (`brew services start dbus` and `dbus-launch`), an end-to-end
  two-terminal walkthrough of the simulator including the exact
  `MockSysfs` seed values the daemon pre-populates, manual D-Bus
  calls with `dbus-send --session`, the limits of what the
  simulator can model (rollback, polkit denial, AC events), and
  6 common pitfalls. Catches the macOS contributor before they
  trip on `HPD_SIMULATOR=1` + `--features simulator` having to
  be passed together.
  *(Lote 47 — Audit V2 Phase 4)*
- **`CONTRIBUTING.md` — contribution guide** (~370 lines, 12
  sections). The contract between contributors and maintainers:
  scope (welcome vs. out-of-scope contributions), prerequisites,
  the four local gates CI enforces (`fmt`/`clippy`/`test`/`doc`)
  with the workspace.lints rules they translate into, hard rules
  (no `unsafe_code`, no `.unwrap()`/`.expect()`/`panic!` in
  production code, pure reducer, polkit-before-enqueue, SPDX
  headers, `missing_docs`), commit conventions (imperative
  subject ≤70 chars, body wrapped at 72, audit-lote tag,
  co-author trailer, atomic commits), CHANGELOG hygiene
  (Keep-a-Changelog format, breaking-by-audience subsection,
  release rename ritual), the SemVer policy on the public
  surface (D-Bus interface, CLI, on-disk state, polkit actions,
  config), short-form recipes for adding a D-Bus method or
  vendor backend cross-linking the full version in
  `docs/ARCHITECTURE.md`, a copy-pasteable PR checklist, review
  process, security disclosure channel, and code of conduct. The
  root README now points contributors here as the entry-point.
  *(Lote 48 — Audit V2 Phase 4)*
- **`docs/release/` — release pipeline design + runbook** (3 files,
  ~870 lines total). Three companion documents establishing the
  GitHub-native release model:
  - `PIPELINE.md` (~310 lines) — the *why*: three environments
    (QA = main CI, STG = `vX.Y.Z-rc.N` draft Release, PROD =
    `vX.Y.Z` public Release), tag conventions, artifact contents
    (tarball + checksums + optional GPG sig), per-environment
    workflow behaviour, GPG signing as opt-in via repo secrets,
    AUR distribution model, immutable-release rollback policy,
    permissions model, explicit non-goals (no nightlies, no
    .deb/.rpm in v1.0, no containers, no release bot), and an
    end-to-end ASCII diagram.
  - `VERSIONING.md` (~175 lines) — the *bump rules*: strict
    SemVer-2.0 from v1.0.0 onward, exact definition of "the
    public surface", a top-to-bottom decision matrix mapping
    every change category to MAJOR/MINOR/PATCH, the project's
    deliberate "no deprecation aliases" policy with rationale,
    pre-release suffix grammar, and four worked examples
    (including hypotheticals from the project's own surface).
  - `RELEASE_CHECKLIST.md` (~385 lines) — the maintainer's
    literal command-by-command runbook: prerequisites + repo
    secrets, day-of pre-release sanity (all four CI gates +
    feature matrix + supply-chain), version pick walking
    VERSIONING.md, bump ritual across `Cargo.toml` +
    `Cargo.lock` + `CHANGELOG.md`, annotated-tag creation with
    HEREDOC message template, `release.yml` watch step, AUR
    manual fallback for both source and binary packages,
    post-release housekeeping (re-open `[Unreleased]`, announce,
    48-hour bug watch), recovery recipes for four common
    failure modes, and a time budget table.
  Cross-linked from `CONTRIBUTING.md` and the root `README.md`.
  *(Lote 49 — Audit V2 Phase 5)*
- **`.github/workflows/release.yml`** + **`scripts/extract-changelog-section.sh`**.
  Implements the GitHub-native release model designed in
  [`docs/release/PIPELINE.md`](docs/release/PIPELINE.md). Triggers on
  annotated tags matching `v<X>.<Y>.<Z>` (stable → Public Release) and
  `v<X>.<Y>.<Z>-*` (RC/alpha/beta → Draft Release).
  Two jobs: `verify` re-runs the four CI gates (fmt/clippy/test/doc) on
  the exact tagged commit; `release` (a) guards that
  `workspace.package.version` in `Cargo.toml` matches the tag, (b)
  builds the stripped `x86_64-linux` binaries, (c) assembles
  `hpd-X.Y.Z-x86_64-linux.tar.gz` with the layout locked in
  PIPELINE.md §3 (binaries + install/uninstall scripts + LICENSE +
  README + CHANGELOG + full `package/` tree), (d) computes
  `SHA256SUMS`, (e) optionally GPG-signs it when
  `GPG_PRIVATE_KEY` + `GPG_PASSPHRASE` repo secrets are configured
  (skipped with a `::notice::` otherwise), (f) extracts the matching
  CHANGELOG section as release notes (falls back to the
  annotated-tag message if absent), and (g) calls
  `gh release create` with `--draft --prerelease` for RCs or a plain
  publish for stable. All artifacts are also uploaded as a 90-day
  workflow artifact for safekeeping.
  The helper script is standalone-runnable (`./scripts/extract-changelog-section.sh 1.0.0`),
  exits 1 with a clear error and a list of available headers when
  the version isn't found.
  *(Lote 50 — Audit V2 Phase 5)*
- **AUR packaging — templates + opt-in sync workflow** (Phase 5
  closeout). Five new files plus targeted updates to PIPELINE.md
  and RELEASE_CHECKLIST.md to reference the real paths instead of
  placeholders:
  - `package/aur/PKGBUILD.template` — source-build package
    `hpd-handheld-power-daemon` rendered against
    `$url/archive/v$pkgver.tar.gz` and built with
    `cargo build --release --frozen --workspace`. Installs
    binaries to `/usr/bin` (AUR convention) and rewrites the
    shipped `package/hpd.service` `ExecStart` path to match via
    `sed` so the unit works against the AUR install layout.
  - `package/aur/PKGBUILD-bin.template` — prebuilt-repack package
    `hpd-handheld-power-daemon-bin` (`provides=` + `conflicts=`
    the source one) that downloads
    `releases/download/v$pkgver/hpd-$pkgver-x86_64-linux.tar.gz`
    and skips compilation entirely. Same install layout.
  - `package/aur/hpd.install` — shared pacman hook for both
    packages: `daemon-reload` on install/upgrade/remove, sends
    SIGHUP to a running `hpd.service` on upgrade (matching the
    project's documented hot-reload contract), prints "next
    steps" message on first install with the
    `systemctl enable --now` and `config.toml` copy hints.
  - `scripts/aur-sync.sh` — standalone-runnable renderer
    (`<pkgname> <version>`) used by both the workflow and the
    manual fallback path in RELEASE_CHECKLIST §5. Downloads the
    matching upstream tarball, computes its sha256, renders the
    chosen template via sed, regenerates `.SRCINFO` via
    `makepkg --printsrcinfo`, clones the AUR repo over SSH,
    commits + pushes. Detects "no changes to push" as a no-op
    so re-running for the same version is safe.
  - `.github/workflows/aur-sync.yml` — opt-in CI workflow
    triggered on `release.published`. Runs inside an
    `archlinux:base-devel` container so `makepkg` is available
    without third-party actions. Skips pre-releases (RCs go to
    Draft, AUR is for stable only); skips silently with a
    `::notice::` when `AUR_SSH_KEY` repo secret is unset. Sets
    up a non-root `builder` user (`makepkg` refuses root),
    pins AUR's host key via `ssh-keyscan`, and pushes both
    source + bin packages in sequence.
  *(Lote 51 — Audit V2 Phase 5)*
- **`scripts/doctor.sh` standalone preflight.** Diagnoses every
  prerequisite `install.sh` assumes — Linux + x86_64, sudo, the Rust
  toolchain at MSRV (1.85), systemd as pid 1, D-Bus, polkit, `wheel`
  group membership (for passwordless `hpdctl` writes), a C linker — and
  DMI-probes the board against the supported ASUS list
  (RC71L / RC72L / RC72LA / RC73XA). Reports pass/warn/fail with
  copy-paste remediation hints; supports `--quiet` and `--strict`.
- **Richer `--help` for `hpdctl` and `hpd-daemon`.** `hpdctl --help`
  now explains what the daemon manages, notes that `wheel`-group users
  change settings without `sudo`, lists worked examples, and gives
  every subcommand and
  argument an explanatory paragraph (`hpdctl <cmd> --help`).
  `hpd-daemon` gains a dependency-free `--help` / `-V` handler: run by
  hand it prints a one-screen orientation (systemctl/journalctl usage,
  config/state paths, env vars, and a pointer to `hpdctl`) instead of
  silently launching a foreground service.

### Changed

- **`install.sh` now runs `scripts/doctor.sh` as a preflight** and
  aborts with exit 1 if any prerequisite is missing. Bypass with
  `./install.sh --skip-doctor`. This stops the "`cargo: command not
  found` halfway through the install" failure mode that hit fresh
  CachyOS / minimal distro installs.

### Fixed

- **`wheel`-group members no longer hit `AuthFailed` on every
  privileged call.** The polkit `<defaults>` required admin
  authentication for all three actions, so the device owner was
  challenged for a password on every TDP / charge / profile change —
  and where no polkit auth agent was running (a minimal handheld
  session, or a terminal driven over SSH) the call failed outright with
  `org.freedesktop.DBus.Error.AuthFailed`. A new companion rule,
  `package/polkit/49-hpd.rules`, grants every `dev.cirodev.hpd.*`
  action to members of the `wheel` group without a prompt. It keys on
  **group membership rather than the `allow_active`/`allow_inactive`/
  `allow_any` session tiers**, because on handheld desktop sessions a
  physically-local terminal can register as `Remote=yes` (driven over
  SSH, or a display manager that doesn't attach the session to the
  seat), which would otherwise drop the owner into `allow_any` and
  force a prompt. Non-`wheel` callers still fall through to the
  `auth_admin` defaults, and the polkit helper remains fail-closed.
  Requires a polkit build with the JS rules engine (>= 0.106, standard
  on modern distros).
- `install.sh` and `uninstall.sh` are now tracked as `100755` in git
  (previously `100644`), so users no longer need to `chmod +x` them
  after cloning the repo.

### CI

- **`aur-sync.yml` gains `workflow_dispatch`.** Maintainers can
  trigger the AUR sync manually from the Actions tab — with an empty
  `version` input it only smoke-tests `AUR_SSH_KEY` against
  `aur@aur.archlinux.org` (no PKGBUILD push, no AUR side-effects);
  with a `version` + `packages` choice (`both` / `source` / `bin` /
  `none`) it re-runs the full push for that version. Useful for
  validating SSH credentials before cutting a release and for
  recovering from a partial release-driven failure.
- **`aur-sync.yml` smoke-test surfaces actionable diagnostics and no
  longer chases a phantom auth failure.** It prints the offered key's
  fingerprint (safe to log) and runs `ssh -v` with the verbose log
  dumped only on failure, alongside copy-paste recovery commands. It
  also fixes the underlying false negative: the container runs as root
  with `$HOME=/github/home`, but root's passwd entry points at
  `/root`, so ssh's internal `~` expansion missed `known_hosts` and
  reported a misleading "Host key verification failed" that looked
  like a publickey rejection. The smoke-test now forces absolute
  `-F` / `-i` / `UserKnownHostsFile` paths so bash and ssh agree on
  where the SSH state lives.
- **All workflows opt into Node.js 24** ahead of the 2026-06-02
  deprecation of the Node 20 action runtime.

### Packaging

- **AUR packages auto-migrate from a manual `install.sh` deployment and
  self-enable** (shipped in `1.0.0-2`). The shared `package/aur/hpd.install`
  hook: (a) in `pre_install`/`pre_upgrade` removes the files a prior
  `install.sh` left at non-package paths (`/usr/local/bin/{hpd-daemon,hpdctl}`,
  `/etc/systemd/system/hpd.service`, `/etc/dbus-1/system.d/...`,
  `/usr/share/hpd/VERSION`, and the polkit policy + rule) — fixing the
  `error: failed to commit transaction (conflicting files)` and the old
  `/usr/local/bin` binaries shadowing the packaged ones; (b) enables and
  starts `hpd.service` in `post_install` so there is no manual `systemctl`
  step; (c) restarts the daemon on upgrade so a new binary actually takes
  effect.
- **Fix `hpd.service` left stopped after an AUR upgrade** (`1.0.0-3`).
  The `1.0.0-2` hook stopped the service unconditionally in
  `pre_upgrade` and then used `try-restart` in `post_upgrade`, which is a
  no-op on an inactive unit — so every upgrade left the daemon dead. The
  migration now only stops the service when it actually finds an
  `install.sh` deployment to clean up, and `post_upgrade` `restart`s the
  unit when it is enabled.
