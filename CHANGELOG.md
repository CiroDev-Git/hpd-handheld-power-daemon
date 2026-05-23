# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each entry references the Audit lote that introduced the change. The audit
itself lives at [`docs/audit/AUDIT_V1.md`](docs/audit/AUDIT_V1.md) and the
remediation plan at [`docs/audit/REMEDIATION_PLAN_V1.md`](docs/audit/REMEDIATION_PLAN_V1.md).

---

## [Unreleased] — 0.2.0 target

This release groups every change from the AUDIT_V1 remediation work. The
public surface (D-Bus interface, state file location, CLI flags) is
considered **unstable** during 0.1.x and is being intentionally broken
in 0.2.0 to consolidate. Subsequent minor releases will respect SemVer.

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

### ⚠ Breaking — internal API (Rust)

- **`HpdError::Backend`** — Changed from struct variant
  `{ reason: String }` to tuple variant wrapping the new
  `BackendError` (`HpdError::Backend(BackendError)`). External Rust
  consumers (none today) would need to migrate `match` arms.
  *(Lote 8 — Audit §4.1)*

### Added

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
  `*.deb`, `*.rpm`), logs and tmp artifacts.
  *(Lote 1)*

### Changed

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

- **Dead types and methods**
  - `BatteryPercent` unit (unused). *(Lote 1)*
  - `HpdError::DbusClient(String)` variant (unused). *(Lote 1)*
  - `SysfsIo::is_writable` method and its `Real`/`Mock` impls
    (defined but never called). *(Lote 1)*
  - `Transition::ConfigReload` variant and its reducer arm. Will be
    reintroduced as a functional reload pathway in a later lote.
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
