# Contributing to `hpd`

Thanks for your interest in improving the Handheld Power Daemon.
This document is the contract between contributors and maintainers
for what goes in, how, and what's expected from a PR.

If you've just cloned the repo, the entry-point reading is:

1. [`README.md`](README.md) — what `hpd` is and how it ships.
2. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the design.
3. The dev guide for your OS:
   [`docs/dev/LINUX.md`](docs/dev/LINUX.md) or
   [`docs/dev/MACOS.md`](docs/dev/MACOS.md).
4. The crate-level `README.md` of whatever crate you intend to
   touch (under `crates/<name>/`).
5. This document.

---

## 1. Scope of contributions we welcome

| Welcome                                                          | Out of scope                                                 |
|------------------------------------------------------------------|--------------------------------------------------------------|
| Bug fixes with a regression test.                                | Fan-curve control — read-only by design (firmware owns curves). |
| A new L1 vendor backend (see [`docs/ARCHITECTURE.md` §10](docs/ARCHITECTURE.md#10-extending-the-system)). | Per-app / per-game profiles — belongs in a user-space agent above the daemon. |
| Improvements to the reducer / executor with new tests.           | RGB / haptics / display — different problem domain.          |
| Documentation, examples, dev-guide additions.                    | Packaging for non-systemd init systems.                      |
| Better D-Bus / CLI ergonomics (without breaking the v1 surface). | Cross-compiling to non-Linux runtime targets.                |
| CI improvements, performance fixes with measurements.            | Breaking changes "just because" — see §6 for SemVer policy.  |

Not sure if your idea fits? **Open an issue first.** A 10-line issue
saves both sides time vs. a 1000-line PR that has to be redirected.

---

## 2. Before you start

### Sign the work with `git config`

```bash
git config user.name  "Your Name"
git config user.email "you@example.com"
```

The repo does not require DCO sign-offs at present, but commits with
a clear name + reachable email are required for attribution.

### Toolchain

The workspace pins `rust-toolchain.toml` to **`1.85.0`**. `rustup`
installs that automatically when you `cd` into the repo. **Do not
bump the toolchain in a contribution PR** unless the PR is about
the toolchain bump itself — touch only what the PR description says
you're touching.

### One change per PR

Bundling unrelated changes is the single most common reason a PR
gets bounced back. If a fix needs a refactor first, split it:
"refactor X for Y" → "fix Y" in two commits or two PRs.

---

## 3. Local gates (run these before pushing)

CI runs four mandatory gates plus a feature matrix and a supply-chain
job. **Run all four locally before pushing** — they're the entire
fast-feedback loop and they save you a round-trip with the bot.

```bash
cargo fmt --all -- --check                                    # 1. formatting
cargo clippy --workspace --all-targets -- -D warnings         # 2. lints
cargo test --workspace                                        # 3. tests
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps    # 4. docs
```

A green local run for those four is the contract. CI additionally
runs the feature matrix and a macOS simulator build:

```bash
cargo build -p hpd-daemon                              # default (vendor-asus)
cargo build -p hpd-daemon --no-default-features        # must still compile
cargo build -p hpd-daemon --features simulator         # vendor-asus + bus + polkit bypass
```

If you've touched `Cargo.toml`, `cfg`-gated code, the dbus surface,
or anything under `package/`, run the matrix locally too.

### What the gates actually enforce

| Gate                                                                                 | Enforces                                                                  |
|--------------------------------------------------------------------------------------|---------------------------------------------------------------------------|
| `cargo fmt --all -- --check`                                                         | `rustfmt` default style, workspace-wide.                                  |
| `cargo clippy ... -- -D warnings`                                                    | `[workspace.lints]`: `unsafe_code = forbid`, `missing_docs = warn`, `clippy::{unwrap_used, expect_used, panic} = warn`. Every warning is an error in CI. |
| `cargo test --workspace`                                                             | All 58 (and counting) tests pass on Linux + macOS.                        |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace`                                   | Every `///` and `//!` block is valid rustdoc; intra-doc links resolve.    |
| CI feature matrix                                                                    | `--no-default-features`, `--features vendor-asus`, `--features simulator` all build cleanly. |
| `cargo audit` + `cargo deny check`                                                   | Dependency CVEs + license allowlist (GPL-compatible only).                |

### Hard rules

These are non-negotiable and have caught real bugs in past audits:

- **`unsafe_code` is forbidden workspace-wide** via `[workspace.lints.rust]`.
  The only exception is `hpd-netlink`, which carries
  `#[allow(unsafe_code)]` *only if* it ever needs to (today it doesn't —
  the `tokio-udev` crate carries the unsafe and `hpd-netlink` only
  consumes its safe API).
- **No `.unwrap()` / `.expect()` / `panic!` in production code.**
  Use `?` with `HpdError`. Test modules opt out with
  `#![allow(clippy::unwrap_used, …)]` inside `#[cfg(test)] mod tests`.
- **The reducer must stay pure.** No I/O, no async, no globals, no
  `println!`. Tracing inside `reduce()` is allowed but only via
  structured `tracing::info!` fields. See
  [`docs/ARCHITECTURE.md` §3](docs/ARCHITECTURE.md#3-the-state-machine).
- **Every privileged D-Bus setter calls `polkit::check(...)` *before*
  enqueuing its `Transition`.** No exceptions. The simulator bypass
  is `cfg(feature = "simulator")`, not a per-call opt-out.
- **Every new `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`**
  followed by a blank line, then attributes / doc comments.
- **Every public item carries a `///` doc comment** and every module
  file opens with a `//!` block. `missing_docs = warn` is on
  workspace-wide; CI's `-D warnings` upgrades that to an error.

---

## 4. Commit conventions

### Subject

- **Imperative mood**, present tense: "Add foo", "Fix bar",
  "Refactor baz".
- **Max 70 characters.** Long lines wrap badly in `git log --oneline`.
- **No trailing period.**
- If the change closes an audit item, append the lote tag:
  `(Audit Lote NN)`. Example commits in the existing history:

  ```
  Promote CHANGELOG [Unreleased] → [1.0.0] (2026-05-24)
  Expose auto_cooling D-Bus property (Audit Lote 42)
  Per-crate README.md for all 9 workspace crates (Audit Lote 44)
  ```

### Body

Always write a body. A "+1 line, why" diff and a commit body
explaining *why* is far more valuable than a sprawling diff with a
one-line message. Wrap at 72 characters.

Structure:

1. One short paragraph: what this commit does and **why** (the
   motivation, not the mechanics — the diff shows the mechanics).
2. Optional bullet list of secondary points (related cleanups,
   notable trade-offs, follow-ups deferred to another lote).
3. If verification matters (touches `Cargo.toml`, `cfg`-gated code,
   D-Bus surface): a "Verification gates" section listing exactly
   which commands you ran clean.
4. Trailers (issue refs, co-authors).

Reference [`CLAUDE.md`](CLAUDE.md) and the existing audit-lote
commits for the established voice.

### Co-author trailer

If the work was paired with an AI assistant or another human, add a
`Co-Authored-By:` trailer at the end of the body. The repo's audit
lotes use:

```
Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
```

### Atomic commits

Each commit should leave the tree green (all four gates pass).
**Never push commits that knowingly break CI** with the intent to
"fix it in the next commit" — split a different way.

### `git commit --amend` and force-push

Amending and force-pushing on **your own PR branch** is welcome and
expected during review. Force-pushing **to `main`** is forbidden;
the daemon's milestones are tagged and pushed (e.g. `v1.0.0`) and
rewriting them would invalidate downstream consumers.

---

## 5. CHANGELOG hygiene

Every user-visible change touches [`CHANGELOG.md`](CHANGELOG.md).
The repo follows [Keep a Changelog](https://keepachangelog.com/) +
[SemVer](https://semver.org/).

### Where the entry goes

- **Unreleased work** lands under `## [Unreleased]` at the top.
- **A release** is created by renaming `## [Unreleased]` to
  `## [X.Y.Z] — YYYY-MM-DD` in a dedicated "release" commit, then
  tagging that commit `vX.Y.Z` (annotated, with a release-notes
  message — see the `v1.0.0` tag for the format).

### Categories

In order, per [Keep a Changelog]:

```
### ⚠ Breaking — <audience>
### Added
### Changed
### Deprecated
### Removed
### Fixed
### Security
```

Audiences for the breaking subsection: **operators / packagers,
D-Bus clients, CLI clients, internal API (Rust).** Pick the most
specific.

### Entry style

- Bold the noun: `**`state file location`** — moved from …`
- One sentence of *what* + one short paragraph of *why* / migration.
- Cite the lote and audit reference at the end:
  `*(Lote 7 — Audit §7.4)*`.

The existing entries under `## [1.0.0]` are the canonical model.

---

## 6. SemVer policy

`hpd` follows SemVer strictly from `v1.0.0` forward. The stable
public surface is:

| Surface                                                              | Bump on change                  |
|----------------------------------------------------------------------|---------------------------------|
| D-Bus interface `dev.cirodev.hpd.PowerDaemon1` (methods, properties, signals) | MAJOR — breaks every client |
| `hpdctl` subcommand syntax and option flags                          | MAJOR for removals/renames; MINOR for additions |
| On-disk state at `/var/lib/hpd/state.toml` (schema, not values)      | MAJOR if non-backwards-compatible; MINOR if additive with `#[serde(default)]` |
| Polkit action IDs in `dev.cirodev.hpd.{set-tdp, set-charge, set-profile}` + the `wheel` grant in `49-hpd.rules` | MAJOR — renaming an action or dropping the `wheel` grant breaks operator policies (the `set-fan-curve` action was retired in 2.5.0 with the unused raw `set_fan_curve` method) |
| `/etc/hpd/config.toml` schema                                        | MAJOR if a previously-valid file is now rejected; MINOR if purely additive |

Internal Rust API (every `pub` item in `hpd-error`, `hpd-sysfs`,
`hpd-netlink`, `hpd-capabilities`, `hpd-backend-asus`, `hpd-core`,
`hpd-dbus`) **is not** part of the SemVer contract today, because
none of these crates are published to crates.io. Treat them as
project-internal for now; that may change if/when they get
published.

### When in doubt

Open an issue with a "SemVer classification request" label and the
maintainer will decide. Defaulting to MAJOR is always safe.

---

## 7. Adding a new D-Bus method / CLI command

The full recipe lives in
[`docs/ARCHITECTURE.md` §10](docs/ARCHITECTURE.md#10-extending-the-system).
The short form (each step is mandatory):

1. `Transition` variant in `hpd-core/src/transition.rs`.
2. Handle in `reduce()` (`hpd-core/src/reducer.rs`) — pure, no I/O.
3. New `Effect` variant + `Executor::handle_effect` arm if
   side-effecting.
4. Extend `spawn_properties_changed_emitter` in
   `hpd-daemon/src/main.rs` if it changes a D-Bus property.
5. New `PolkitAction` variant in `hpd-dbus/src/actions.rs` + matching
   `<action>` in `package/polkit/dev.cirodev.hpd.policy` (if
   privileged). The `wheel` grant in `package/polkit/49-hpd.rules`
   matches `dev.cirodev.hpd.*` by prefix, so it already covers the new
   action — no edit needed there.
6. Method on `PowerDaemonInterface` (`hpd-dbus/src/service.rs`) with
   `polkit::check(...)` before enqueuing.
7. Proxy method in `hpd-cli/src/dbus.rs` + subcommand in
   `hpd-cli/src/main.rs`.
8. Tests covering reducer behaviour + (if possible) executor
   integration.
9. `### Added` / `### Changed` entry in `CHANGELOG.md`.

---

## 8. Adding a new vendor backend

See [`docs/ARCHITECTURE.md` §10](docs/ARCHITECTURE.md#adding-a-new-vendor-backend)
for the full recipe. Key constraints:

- The backend must implement `PowerEnvelope` at minimum;
  `ChargeControl`, `PlatformProfile`, `FanControl` are optional via
  `Option<&dyn …>` on `HwBackend`.
- A `detect.rs` returning `Option<Model>` from a `DmiInfo`.
- A `vendor-<name>` feature in `hpd-daemon/Cargo.toml` gating the
  optional dep.
- A per-crate `README.md` matching the format used in the existing
  nine.
- Hardware matrix update in `package/hpd-example.toml` and the root
  `README.md`.

Real hardware testing is **strongly preferred** before merge. Open
the PR as a draft and tag it `hardware-test-needed` if you've only
been able to verify the simulator path so far.

---

## 9. PR checklist

Copy this into your PR description and tick each item:

```
- [ ] Local gates green:
      - [ ] cargo fmt --all -- --check
      - [ ] cargo clippy --workspace --all-targets -- -D warnings
      - [ ] cargo test --workspace
      - [ ] RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
- [ ] Feature matrix (if Cargo.toml or cfg-gated code touched):
      - [ ] --no-default-features
      - [ ] --features vendor-asus (default)
      - [ ] --features simulator
- [ ] CHANGELOG entry under [Unreleased]
- [ ] Tests added or modified for behaviour changes
- [ ] Rustdoc on every public item I added (missing_docs gate)
- [ ] SPDX header on every new .rs file
- [ ] No new .unwrap() / .expect() / panic! in production code
- [ ] D-Bus / CLI / on-disk surface changes documented in CHANGELOG
      with a SemVer classification
- [ ] One change per PR (or a clear "this is X+Y because…" rationale)
```

---

## 10. Review process

- A maintainer triages within ~7 days. If your PR has been silent
  longer than that, ping the issue or open one.
- Reviews focus on: correctness, fit with the architecture rules,
  test coverage, doc accuracy. **Style is enforced by `fmt` and
  `clippy`, not by reviewers.**
- Discussion happens in the PR thread. Resolve conversations only
  after pushing the matching change.
- Squash-merge is the default; the resulting commit message should
  be the PR title (subject) and a clean body (rewrite during the
  squash if needed).
- Tagging a release is a maintainer-only action.

---

## 11. Security issues

If you find a security-relevant bug — anything that could let an
unprivileged user bypass polkit, escape the systemd sandbox, or
trigger a sysfs write the user is not authorized for — **do not
open a public issue**. Email the maintainer directly at the address
listed in `Cargo.toml`'s `authors` field with the words
`hpd security` in the subject. Public disclosure happens after a fix
ships.

---

## 12. Code of conduct

Be civil, give credit, assume good faith. Reviews are about the
code, not the contributor. If a review thread crosses into personal
territory, a maintainer will intervene; if the maintainer crosses
the line, escalate by email.

---

*Thanks again for contributing. Every audit lote in this repo's
history started as someone deciding to spend an afternoon on it.*
