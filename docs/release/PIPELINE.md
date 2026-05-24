# Release pipeline

> How code gets from a developer's branch to a tagged GitHub Release
> the operators upgrade to. The implementation lives in
> `.github/workflows/`; this document is the *why* and the *contract*.
>
> Companion docs:
> - [`VERSIONING.md`](VERSIONING.md) — version-bump rules.
> - [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) — maintainer runbook.
> - [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — what's being released.
> - [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md) — local gates.

---

## 1. The three environments

`hpd` follows a deliberately simple **GitHub-native** release model.
There are three "environments" — they map cleanly to ordinary git
ref types instead of long-lived parallel branches.

| Env       | Git ref                            | Triggered workflow                | Artifact destination                          | Audience                                   |
|-----------|------------------------------------|-----------------------------------|-----------------------------------------------|--------------------------------------------|
| **QA**    | every push to `main` or PR ref     | `.github/workflows/ci.yml`        | GitHub Actions build artifacts (30-day retention) | Contributors verifying their PR.       |
| **STG**   | annotated tag matching `vX.Y.Z-rc.N` | `.github/workflows/release.yml`   | **Draft** GitHub Release                       | Release-candidate testers, package maintainers staging the next bump. |
| **PROD**  | annotated tag matching `vX.Y.Z`    | `.github/workflows/release.yml`   | **Published** GitHub Release + AUR sync (opt-in) | End users, distro packagers.            |

There are **no long-lived staging branches**. Pre-release work
happens on `main` and on feature/PR branches; a release candidate is
just a tag that the workflow recognises as "draft, not yet stable".

### Why not branches?

A long-lived `staging` branch creates a fork in the project's
history that has to be merged forward, conflicts to manage, and an
extra reviewer policy to enforce. For a single-binary daemon with a
small public surface, that complexity buys nothing. The tag-driven
model means:

- The exact commit being released is identifiable from `git log`.
- Reverting an RC is `git tag -d` + `git push --delete`.
- Operators can pin to a tag and `git fetch --tags && checkout`.

---

## 2. Tag conventions

All release tags are **annotated** (`git tag -a`) — never lightweight.
The annotation carries the release notes and is what GitHub displays
in its tag listing.

| Tag pattern        | Example          | Meaning                                                           |
|--------------------|------------------|-------------------------------------------------------------------|
| `vX.Y.Z`           | `v1.0.0`         | Stable release. Triggers a **Public** GitHub Release.              |
| `vX.Y.Z-rc.N`      | `v1.1.0-rc.1`    | Release candidate. Triggers a **Draft** GitHub Release.            |
| `vX.Y.Z-alpha.N` / `vX.Y.Z-beta.N` | `v2.0.0-beta.3` | Early preview. Triggers a **Draft** Release (treated as STG). |

The pre-release suffix grammar is **PEP-440-ish / SemVer 2.0** style:
- `-rc.<int>`, `-alpha.<int>`, `-beta.<int>` only.
- No date stamps, no commit hashes, no `+build` metadata in tags
  (build metadata, if needed, goes into the tarball filename).

### Reserved namespace

The strings `latest`, `nightly`, and `edge` are reserved. They MAY
be used as Docker tag aliases in the future; don't use them for git
tags.

---

## 3. What ships in a release artifact

A release artifact is a single `.tar.gz` per platform, plus a
checksum file and (optionally) a detached GPG signature.

```
hpd-1.0.0-x86_64-linux.tar.gz
├── hpd-daemon                 (release-mode binary)
├── hpdctl                      (release-mode binary)
├── install.sh                  (copied from repo root)
├── uninstall.sh                (copied from repo root)
├── LICENSE                     (GPL-3.0 text)
├── README.md                   (snapshot at tag time)
├── CHANGELOG.md                (snapshot at tag time)
└── package/                    (full directory: systemd unit, polkit, dbus policy, example config)
    ├── hpd.service
    ├── dev.cirodev.hpd.conf
    ├── hpd-example.toml
    └── polkit/
        └── dev.cirodev.hpd.policy

SHA256SUMS                       (sha256 over all *.tar.gz files attached to this release)
SHA256SUMS.asc                   (optional GPG-detached signature of SHA256SUMS)
```

### Target platforms

`1.0.0` ships a single platform tarball: `x86_64-linux` (glibc).

Future platforms will be added as separate jobs in
`release.yml` without changing the artifact layout:

| Triple                    | Status                                          |
|---------------------------|-------------------------------------------------|
| `x86_64-unknown-linux-gnu` | Shipping in `v1.0.0`.                          |
| `aarch64-unknown-linux-gnu` | Considered for `v1.x` (if a target handheld lands on ARM). |
| `x86_64-unknown-linux-musl` | Considered for static-binary use cases.        |

---

## 4. Pipeline behaviour

### QA — push to `main` / PR

Already exists today as `.github/workflows/ci.yml`. Runs on every
push to `main` and on every PR. Jobs:

1. `build-test` — Linux: fmt + clippy + test + doc + release build,
   uploads `hpd-linux-x86_64` artifact.
2. `build-macos-simulator` — macOS: builds the simulator path.
3. `feature-matrix` — 4 combos: default / `vendor-asus` /
   `simulator` / `--no-default-features`.
4. `supply-chain` — `cargo audit` + `cargo deny check`.

These artifacts have a 30-day retention. They are **not** released
to anyone — they exist so a contributor can grab a build to test on
hardware before merging.

### STG — `vX.Y.Z-rc.N` tag

When the maintainer pushes a tag matching `v*-rc.*`, `release.yml`:

1. Re-runs the four CI gates (fmt + clippy + test + doc).
2. Builds the release-mode binaries with the default
   (`vendor-asus`) feature set.
3. Assembles the tarball + checksums.
4. (Opt-in) GPG-signs the checksum file if `GPG_PRIVATE_KEY` and
   `GPG_PASSPHRASE` secrets are configured.
5. Extracts the matching section from `CHANGELOG.md` as release notes.
6. Creates a **Draft** GitHub Release with the assets attached.

The Draft is invisible to non-maintainers. The maintainer reviews
the auto-extracted notes, edits if needed, and either:

- Promotes to a Public Release manually (rare — usually a stable
  tag is what triggers PROD), or
- Discards the draft if the RC is being abandoned (`git tag -d` +
  `git push --delete origin <tag>`).

### PROD — `vX.Y.Z` tag

Same workflow as STG, with two differences:

- The GitHub Release is created as **Public** (not Draft).
- (Opt-in) AUR sync workflow runs after the release publishes, if
  `AUR_SSH_KEY` is configured. See [§6](#6-aur-distribution).

---

## 5. GPG signing

Signing is **opt-in** to keep the pipeline runnable from day one
even before a maintainer has set up a signing key. Two repository
secrets enable it:

| Secret              | Value                                                                |
|---------------------|----------------------------------------------------------------------|
| `GPG_PRIVATE_KEY`   | ASCII-armoured private key (`gpg --armor --export-secret-keys <KEY>`).|
| `GPG_PASSPHRASE`    | Passphrase for the private key.                                       |

When both are set, `release.yml` imports the key, signs
`SHA256SUMS` with `gpg --detach-sign --armor`, and attaches
`SHA256SUMS.asc` to the Release.

When either is missing, the workflow logs `GPG signing skipped: no
GPG_PRIVATE_KEY` and continues. The Release still includes
`SHA256SUMS` but without a `.asc` companion.

### Recommended key hygiene

- Use a key dedicated to releases — not your personal key.
- Set expiry (1 or 2 years); rotate before it expires.
- Publish the public key on the project website / a keyserver and
  document the fingerprint in `RELEASE_CHECKLIST.md`.

---

## 6. AUR distribution

Arch users get `hpd` via two AUR packages, rendered from templates
under [`package/aur/`](../../package/aur/):

| AUR name                          | Template                                | Source                                              |
|-----------------------------------|-----------------------------------------|-----------------------------------------------------|
| `hpd-handheld-power-daemon`       | `package/aur/PKGBUILD.template`         | Builds from source at a specific tag.               |
| `hpd-handheld-power-daemon-bin`   | `package/aur/PKGBUILD-bin.template`     | Repacks the official tarball — fast install.        |

Both packages share the install hook at `package/aur/hpd.install`,
which runs `systemctl daemon-reload` post-install/upgrade/remove and
prints the next-steps message operators see during `pacman -S`.

The AUR push is **opt-in** via an `AUR_SSH_KEY` repo secret
containing a private SSH key with push access to both AUR
repositories. The implementation:

- **Workflow:** [`.github/workflows/aur-sync.yml`](../../.github/workflows/aur-sync.yml).
  Triggers on `release.published`. Runs inside an
  `archlinux:base-devel` container so `makepkg` is available.
- **Per-package script:** [`scripts/aur-sync.sh`](../../scripts/aur-sync.sh)
  (`<pkgname> <version>`). Downloads the matching upstream tarball,
  computes its `sha256`, renders the chosen `PKGBUILD.template`
  via `sed`, generates `.SRCINFO` via `makepkg --printsrcinfo`,
  clones the AUR repo, commits, and pushes.

Behaviour rules:

- **Pre-release tags are skipped.** RCs/alphas/betas (any tag with
  `-` in it) are not published to AUR — only stable `vX.Y.Z`.
- **Missing secret is silent.** If `AUR_SSH_KEY` is not configured
  the workflow emits a `::notice::` and exits 0. The maintainer can
  still update AUR manually following the recipe in
  [`RELEASE_CHECKLIST.md` §5](RELEASE_CHECKLIST.md#5-aur-update-manual-fallback).
- **Sync runs after the GitHub Release exists.** The bin package's
  `sha256` is computed over the release asset, which must already be
  published — `release.yml` runs first, this workflow's
  `release: published` trigger fires only after the Release has its
  assets attached.

---

## 7. Rollback policy

Releases are **immutable** once published. Don't delete a published
Release. If a release is found to contain a critical bug:

1. Cut a new patch release with the fix (`vX.Y.Z+1`).
2. Mark the broken release as "yanked" in its body
   (`⚠ This release contains a known bug — upgrade to vX.Y.Z+1.`).
3. Optionally: mark it as "pre-release" in GitHub to push it below
   the "latest" badge.

The same applies to AUR: don't delete a tag, push a higher one.

Draft releases (RCs) can be deleted freely — they have no
downstream consumers by definition.

---

## 8. Permissions model

| Action                                                  | Who                |
|---------------------------------------------------------|--------------------|
| Push to `main`                                          | Anyone with PR-merge rights. |
| Create lightweight tag in a branch (local convenience)  | Anyone.            |
| Push an annotated `v*` or `v*-rc.*` tag to `origin`     | **Maintainers only.** |
| Configure repository secrets (`GPG_PRIVATE_KEY`, `AUR_SSH_KEY`) | **Repo owner only.** |
| Edit / unpublish a Draft Release                        | Maintainers.       |
| Edit / re-publish a Public Release body                 | Maintainers (rarely; releases are immutable). |
| Cut a stable release that ships breaking changes        | **Maintainer + CHANGELOG SemVer review.** |

Branch protection on `main` is recommended but not enforced
programmatically yet — it's a repo-settings concern. The required
CI checks under branch protection should be the four jobs in
`ci.yml`.

---

## 9. What this pipeline deliberately doesn't do

- **No nightlies.** The four CI gates run on every `main` push;
  there is no separate "nightly build". Contributors can grab the
  per-push artifact from GitHub Actions if they need a tip-of-main
  build.
- **No `.deb` / `.rpm` packaging in v1.0.** Considered for v1.x
  once an operator volunteers to maintain the spec/control files.
  Until then, the tarball + `install.sh` is the supported install
  path on non-Arch distros.
- **No container images.** `hpd` writes to `/sys`, so containerised
  use is a non-goal.
- **No release bot.** A human maintainer drives every release using
  the [`RELEASE_CHECKLIST.md`](RELEASE_CHECKLIST.md) runbook.

---

## 10. Diagram

```
                              ┌──────────────────────────┐
                              │       contributor        │
                              │  feature branch / PR     │
                              └────────────┬─────────────┘
                                           │
                              ┌────────────┴─────────────┐
                              │            QA            │
                              │    .github/workflows/    │
                              │         ci.yml           │
                              │  (fmt/clippy/test/doc/   │
                              │   feature-matrix/audit)  │
                              └────────────┬─────────────┘
                                           │ merge
                                           ▼
                                 ┌─────────────────┐
                                 │      main       │
                                 └────────┬────────┘
                                          │ maintainer creates tag
              ┌───────────────────────────┼─────────────────────────────┐
              │                                                          │
              ▼ git push origin v1.1.0-rc.1                              ▼ git push origin v1.1.0
   ┌────────────────────┐                                  ┌────────────────────────────┐
   │       STG          │                                  │           PROD             │
   │ release.yml (RC)   │                                  │ release.yml (stable)        │
   │  → Draft Release   │                                  │  → Public Release           │
   │  → tarball+sums    │                                  │  → tarball+sums + GPG sig   │
   │  → notes from      │                                  │  → notes from CHANGELOG     │
   │     CHANGELOG      │                                  │  → AUR sync (opt-in)        │
   └────────────────────┘                                  └────────────────────────────┘
              │                                                          │
              ▼                                                          ▼
        testers fetch,                                         users `wget tarball`,
        package maintainers                                    operators `./install.sh`,
        rehearse upgrade                                       Arch users `pacman -S` (AUR)
```

---

*Last updated: 2026-05-24 (Phase 5 design — Lote 49).*
