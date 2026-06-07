# Versioning

> The rules that govern *when* the version number gets bumped and
> *by how much*. The mechanics of cutting a release live in
> [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md); the *what is and
> isn't* a stable surface lives in
> [`../../CONTRIBUTING.md` §6](../../CONTRIBUTING.md#6-semver-policy).

---

## 1. The contract: SemVer, strictly

`hpd` follows [SemVer 2.0](https://semver.org/spec/v2.0.0.html) from
`v1.0.0` onward. The version number is a triple `MAJOR.MINOR.PATCH`:

- **MAJOR** — a backwards-incompatible change to the public surface.
- **MINOR** — an additive change to the public surface (new D-Bus
  method, new CLI subcommand, new config field with a default).
- **PATCH** — a bug fix with no surface change.

A pre-release suffix `-rc.N` may be appended to any version to mark
a release candidate:

```
1.0.0          stable
1.1.0-rc.1     first RC for the upcoming 1.1.0
1.1.0-rc.2     second RC
1.1.0          eventual stable
2.0.0-beta.3   third beta for a future major
```

---

## 2. What counts as "the public surface"

These are the things SemVer protects. Any change to one of them
gates the version bump.

| Surface                                                                                       | What "breaking" means                                                                                                  |
|-----------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------|
| **D-Bus interface** `dev.cirodev.hpd.PowerDaemon1`                                            | Removing or renaming a method/property/signal; changing a method's signature; changing a property's type.              |
| **`hpdctl` CLI subcommands and flags**                                                        | Removing or renaming a subcommand/flag; changing the meaning of an existing arg.                                       |
| **On-disk state at `/var/lib/hpd/state.toml`**                                                | Rejecting a previously-valid state file at load; changing the meaning of an existing field.                            |
| **Polkit action IDs `dev.cirodev.hpd.{set-tdp, set-charge, set-profile}`** and the `wheel` grant in `49-hpd.rules` | Renaming an action ID; changing its default policy (`auth_admin` → `no` etc.); removing the `wheel` passwordless grant. *(The `set-fan-curve` action was retired in 2.5.0 with the unused raw `set_fan_curve` method.)* |
| **`/etc/hpd/config.toml` schema**                                                             | Rejecting a previously-valid config file at load; changing the meaning of an existing field.                           |
| **systemd unit name `hpd.service` and its `StateDirectory` / `ConfigurationDirectory` names** | Renaming the unit, the state dir, or the config dir.                                                                   |

Things explicitly **not** part of the SemVer contract today:

- Internal Rust API (every `pub` item in `hpd-error`, `hpd-sysfs`,
  `hpd-netlink`, `hpd-capabilities`, `hpd-backend-asus`, `hpd-core`,
  `hpd-dbus`). None of these are published to crates.io; treat them
  as project-internal. If/when one is published, that crate adopts
  its own SemVer track starting at `1.0.0`.
- Log line formats. `RUST_LOG=hpd=info` output is for human/operator
  consumption and may change between any two versions.
- Internal implementation choices like the channel capacities, the
  default `sppt_factor`/`fppt_factor`, the rollback strategy, etc.

---

## 3. Decision matrix

When in doubt, walk this table top to bottom — pick the first
matching row.

| Change                                                                                          | Bump   | Notes                                                                                              |
|-------------------------------------------------------------------------------------------------|--------|----------------------------------------------------------------------------------------------------|
| Remove a D-Bus method/property/signal/action ID                                                 | MAJOR  | Always.                                                                                            |
| Rename a D-Bus method/property/signal/action ID                                                 | MAJOR  | Always. (Aliases are not maintained — see §6.)                                                     |
| Change a D-Bus method signature (param count/order/type)                                        | MAJOR  | Always.                                                                                            |
| Change a D-Bus property type                                                                    | MAJOR  | Always.                                                                                            |
| Remove or rename an `hpdctl` subcommand or flag                                                 | MAJOR  | Always.                                                                                            |
| Reject a previously-valid `/var/lib/hpd/state.toml` at load                                     | MAJOR  | Either provide migration code (see §5) or bump MAJOR.                                              |
| Reject a previously-valid `/etc/hpd/config.toml` at load                                        | MAJOR  | All fields have `#[serde(default)]`, so this should be very rare.                                  |
| Rename `hpd.service` or its `StateDirectory` / `ConfigurationDirectory`                         | MAJOR  | Always.                                                                                            |
| **Add** a new D-Bus method/property/signal/action ID                                            | MINOR  | New surface only; existing clients keep working.                                                    |
| **Add** a new `hpdctl` subcommand or flag                                                       | MINOR  | New surface only.                                                                                  |
| **Add** a new field to `/etc/hpd/config.toml` with `#[serde(default)]`                          | MINOR  | Existing files keep parsing.                                                                       |
| **Add** a new optional field to `/var/lib/hpd/state.toml` with `#[serde(default)]`              | MINOR  | Existing state files keep loading.                                                                 |
| Change runtime-tunable defaults (`sppt_factor`, `fppt_factor`, `profile_thresholds`)            | MINOR  | Existing configs override; only fresh installs see the change. Document in CHANGELOG `### Changed`. |
| Add support for a new vendor backend (new L1 crate, new `vendor-*` feature)                     | MINOR  | Additive: existing hardware paths unchanged.                                                       |
| Bug fix that doesn't touch any surface in §2                                                    | PATCH  | Plain bug fix.                                                                                     |
| Performance fix, refactor with no surface change                                                | PATCH  | If you can't think of *any* surface change, it's PATCH.                                            |
| Doc-only change                                                                                 | PATCH  | …or even no bump if it's coupled with the next release.                                            |
| Internal Rust API change in a `pub` item not on the v1 surface                                  | PATCH  | Internal API is not protected; document if downstream-affecting.                                    |
| Anything you're not sure about                                                                  | MAJOR  | Defaulting to MAJOR is always safe.                                                                |

---

## 4. The `0.x` exception (historical only)

Before `v1.0.0`, the project briefly advertised a `0.2.0` trajectory
that was abandoned in favour of jumping straight from `0.1.0` to
`1.0.0` (see [`CHANGELOG.md`](../../CHANGELOG.md) under the
`[1.0.0]` section for the rationale).

In `0.x` territory, breaking changes only require a MINOR bump per
SemVer. **This no longer applies**: `1.0.0` and onward follow the
strict rules above. Don't add a "we're still iterating, breakage is
OK" mindset back in.

---

## 5. Migrations

When a release introduces a backwards-incompatible change to an
on-disk format, the maintainer's options are:

1. **Bump MAJOR + document the migration manually.** Cheapest for
   the maintainer, most expensive for the operator. Suitable for
   small surfaces (one or two fields).
2. **Ship one-shot migration code.** The daemon detects the old
   schema on first run and rewrites it. Suitable for non-trivial
   schema evolution. Use `#[serde(rename = "old_name", default)]`
   for soft renames; for hard format shifts, version the file
   header (`schema_version = 2`) and branch on read.

Either way, the migration appears under `### Breaking — operators
/ packagers` in the release CHANGELOG section.

---

## 6. Why no deprecation cycle?

`hpd` does not maintain deprecated aliases. The reasons:

- The public surface is small (≈8 D-Bus methods, ~6 CLI
  subcommands). Renames are rare and the operator pain of
  re-learning them is bounded.
- Keeping deprecated aliases around inflates the surface
  permanently — every alias has to be tested, documented, and
  reasoned about during future audits.
- A single SemVer-MAJOR bump with a clear migration note in the
  CHANGELOG is less confusing than a slow deprecation in which
  both forms work for several releases.

Sister projects (e.g. the Linux kernel itself) sometimes keep
ABI-deprecated symbols for years. `hpd` deliberately chooses the
opposite policy: simpler surface, sharper version boundaries.

If a real-world breakage shows this policy was wrong for a
particular change, we'll reconsider — but the default is "no
aliases, bump MAJOR, document migration."

---

## 7. Pre-release versions

`-rc.N`, `-beta.N`, `-alpha.N` suffixes are reserved for actual
pre-releases — they trigger the STG behaviour in
[`PIPELINE.md`](PIPELINE.md). Don't use them as a "this is the dev
version of MAIN" sticker; the canonical "in development" version is
whatever's at `## [Unreleased]` in the CHANGELOG, not a numeric
suffix on the manifests.

`workspace.package.version` in `Cargo.toml` is bumped as part of the
release ritual (see [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md)),
not preemptively.

---

## 8. Examples from the project's own history

| From → to       | Bump  | Why                                                                                                |
|-----------------|-------|----------------------------------------------------------------------------------------------------|
| `0.1.0 → 1.0.0` | MAJOR | First stable: locked the public surface. Skipped the planned `0.2.0` trajectory (see CHANGELOG).   |
| (hypothetical) `1.0.0 → 1.0.1` | PATCH | Bug fix in `hpd-core` reducer; no D-Bus / CLI / on-disk change.                    |
| (hypothetical) `1.0.0 → 1.1.0` | MINOR | New D-Bus property `auto_cooling` (Lote 42). Additive, no breakage.                |
| (hypothetical) `1.1.0 → 2.0.0` | MAJOR | Polkit action ID `set-charge` renamed to `set-battery`. Breaks operator policies.  |

---

*Last updated: 2026-05-24 (Phase 5 — Lote 49).*
