# Release checklist

> The literal command-by-command runbook for cutting a release.
> Companion to [`PIPELINE.md`](PIPELINE.md) (the why) and
> [`VERSIONING.md`](VERSIONING.md) (the bump rules).
>
> Only maintainers should follow this. If you're a contributor,
> stop here and read [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md).
>
> **`main` is branch-protected.** Every change — including the version
> bump — lands through a pull request (CI green → squash-merge), never a
> direct `git push origin main`. Annotated **tags** are *not* protected
> and are still pushed straight to `origin` (§4), which is what triggers
> `release.yml`.

---

## 0. Prerequisites (one-time)

Confirm once per machine before your first release:

```bash
# 1. You can push to origin
git remote -v | grep CiroDev-Git/hpd-handheld-power-daemon

# 2. You have annotated-tag permission (i.e. push to refs/tags/*)
git ls-remote --tags origin | head -5

# 3. Your git identity is set
git config user.name && git config user.email

# 4. (Optional but recommended) Your GPG key is imported and
#    you know its keyid for the SHA256SUMS signing step:
gpg --list-secret-keys --keyid-format LONG
```

Recommended repo-level secrets (configure under **Settings → Secrets
and variables → Actions**):

| Secret              | Purpose                                                              |
|---------------------|----------------------------------------------------------------------|
| `RELEASE_PAT`       | PAT used by `release.yml` to **create the Release** so the `release: published` event chains to `aur-sync.yml` automatically. Without it the Release is created with `GITHUB_TOKEN`, which does **not** trigger downstream workflows — AUR must then be synced manually. Optional. |
| `GPG_PRIVATE_KEY`   | Used by `release.yml` to GPG-sign `SHA256SUMS`. Optional.            |
| `GPG_PASSPHRASE`    | Passphrase for the key above.                                        |
| `AUR_SSH_KEY`       | Private SSH key with push access to the two AUR packages. Optional.  |

If none are configured, the release ships unsigned and AUR is
updated manually (see [§5](#5-aur-update-manual-fallback)).

**Fully-automatic AUR:** with both `RELEASE_PAT` *and* `AUR_SSH_KEY`
configured, pushing a stable tag publishes the Release **and** updates
both AUR packages with no manual step. `RELEASE_PAT` should be a
**fine-grained PAT** scoped to this repository with **Contents:
read & write** (enough to create a Release); classic PATs need the
`repo` scope. Set it under **Settings → Secrets and variables →
Actions → New repository secret**, name `RELEASE_PAT`.

---

## 0b. AUR account + SSH setup (one-time, before first AUR-enabled release)

Skip this section if `AUR_SSH_KEY` is already set and working.

**This must be done before pushing `v1.0.0`.** The `aur-sync.yml`
workflow will skip silently if `AUR_SSH_KEY` is absent, so a missing
setup does not break the release — it just means AUR is not updated
automatically. You can always set it up later and push manually
via §5.

### 0b-1. Create an AUR account

Register at <https://aur.archlinux.org/register/>. Use the same
maintainer email that appears in `package/aur/PKGBUILD.template`
(`# Maintainer: Cristian Ciro <cristian_ciro@icloud.com>`).

### 0b-2. Generate a dedicated SSH key pair

Generate a key specifically for AUR (do not reuse your GitHub deploy
key or your personal SSH key):

```bash
ssh-keygen -t ed25519 -C "hpd-aur-deploy" -f ~/.ssh/hpd-aur
# Leave the passphrase empty so CI can use it unattended.
```

This produces:
- `~/.ssh/hpd-aur`     — private key (goes to GitHub secret)
- `~/.ssh/hpd-aur.pub` — public key  (goes to AUR account)

### 0b-3. Register the public key on AUR

1. Copy the public key: `cat ~/.ssh/hpd-aur.pub`
2. Go to <https://aur.archlinux.org/account/> → **My Account** → **SSH Keys**.
3. Paste the public key and save.

Verify authentication (should print a list of AUR shell commands):

```bash
ssh-keyscan -t ed25519,rsa aur.archlinux.org >> ~/.ssh/known_hosts 2>/dev/null
ssh -i ~/.ssh/hpd-aur aur@aur.archlinux.org help
```

Expected output: `Commands available: ...`. If you see
`Permission denied (publickey)`, the public key is not yet saved on AUR.

### 0b-4. Add the private key as a GitHub repository secret

1. Copy the private key contents: `cat ~/.ssh/hpd-aur`
2. In the GitHub repo: **Settings → Secrets and variables → Actions →
   New repository secret**.
3. Name: `AUR_SSH_KEY`. Value: paste the full private key (including
   `-----BEGIN OPENSSH PRIVATE KEY-----` and `-----END ...-----` lines).
4. Save.

Verify by navigating to **Settings → Secrets** and confirming
`AUR_SSH_KEY` appears in the list.

### 0b-5. First-release note: package auto-creation

AUR packages do **not** need to be pre-registered in a web UI. On
the first `git push` from `scripts/aur-sync.sh`, AUR's git server
creates the package automatically under your account. The package
becomes visible in the AUR web interface as soon as it has a valid
`PKGBUILD` and `.SRCINFO` — both of which `aur-sync.sh` generates.

If the package name is already taken by another AUR user,
`git push` will fail with "permission denied". In that case, choose
a different package name and update the `case` block in
`scripts/aur-sync.sh` and the templates in `package/aur/`.

---

## 1. Pre-release sanity (the day of the release)

Run **all** of these and confirm green before touching anything else:

```bash
# Sync with origin and start from a clean main
git fetch --tags origin
git checkout main
git pull --ff-only origin main
git status                              # must show "nothing to commit, working tree clean"

# All four CI gates
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Feature matrix
cargo build -p hpd-daemon
cargo build -p hpd-daemon --no-default-features
cargo build -p hpd-daemon --features simulator

# Supply-chain (CI also runs these)
cargo audit
cargo deny check
```

If anything is yellow or red: **stop**. Fix it (via a PR into `main`),
then restart from the top of §1 once `main` is green again.

---

## 2. Pick the new version

Walk [`VERSIONING.md` §3](VERSIONING.md#3-decision-matrix) for every
entry under `## [Unreleased]` in `CHANGELOG.md`. The highest required
bump wins.

```
new_version=1.1.0          # example
```

(For an RC, use e.g. `new_version=1.1.0-rc.1`.)

---

## 3. Bump version + finalise CHANGELOG

Three edits, one commit.

### 3a. `Cargo.toml`

In the workspace root `Cargo.toml`, update `[workspace.package]`:

```toml
[workspace.package]
version = "1.1.0"          # ← bump here
```

Verify nothing else needs updating:

```bash
grep -nR 'version = ' crates/*/Cargo.toml | grep -v 'workspace = true'
# Should print nothing — every crate inherits from workspace.package.
```

### 3b. `Cargo.lock`

```bash
cargo update --workspace --offline    # refresh lockfile to match
git diff Cargo.lock                   # sanity check
```

### 3c. `CHANGELOG.md`

Rename the floating `## [Unreleased]` section to the new version
with today's ISO date:

```diff
- ## [Unreleased]
+ ## [1.1.0] — 2026-05-24
```

Confirm every entry under the renamed section follows the format:

- Categories in order: `### ⚠ Breaking — <audience>` (if any) →
  `### Added` → `### Changed` → `### Deprecated` → `### Removed` →
  `### Fixed` → `### Security`.
- Each entry: one bold noun, one sentence of *what*, one short
  paragraph of *why* / migration, audit/lote tag if applicable.

If this is a stable release that has gone through one or more RCs,
also merge any RC-only sections into the stable section (consumers
don't want to read three separate sections for the same release).

### 3d. Commit on a release branch and open the PR

`main` is protected, so the bump lands through a pull request like any
other change. Commit on a branch, push the branch, open the PR — don't
tag yet.

```bash
release_branch="release/v${new_version}"
git checkout -b "${release_branch}"
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Bump to ${new_version}

Promotes [Unreleased] → [${new_version}] in CHANGELOG.md. See that
section for the full breaking-change inventory and the audit lote
references.
"
git push -u origin "${release_branch}"
gh pr create --base main --head "${release_branch}" \
    --title "Bump to ${new_version}" \
    --body "Release bump for v${new_version} — see the CHANGELOG section for the inventory."
```

> A release PR may bundle the feature work *and* the bump (one PR), or
> bump a series of already-merged feature PRs (bump-only). Either is
> fine; the CHANGELOG section just has to be finalised before the tag.

Wait for the PR's CI to go green, then **squash-merge** it (the house
style — merged commits carry the `(#NN)` PR suffix) and delete the
branch:

```bash
gh pr checks --watch          # blocks until every check finishes
gh pr merge --squash --delete-branch
```

If CI fails, push a follow-up commit to the same branch and let the
checks re-run — never tag a red PR.

Finally, fast-forward local `main` to the squash-merge commit; the tag
in §4 must point at **that** commit, not at your pre-merge branch HEAD:

```bash
git checkout main
git pull --ff-only origin main
git rev-parse --short HEAD      # note it — this is what v${new_version} will tag
```

---

## 4. Tag and trigger the release workflow

> **Precondition:** the §3d bump PR is merged and your local `main` is
> fast-forwarded onto it — `git rev-parse HEAD` must equal
> `git rev-parse origin/main`. You are tagging that merged commit.
> (Tags bypass branch protection, so the `git push origin "v…"` below
> works as a direct push.)

### 4a. Create the annotated tag

Use a HEREDOC so the tag message ends up multi-line:

```bash
git tag -a "v${new_version}" -m "$(cat <<EOF
hpd v${new_version}

<one-paragraph summary of what's in this release — copy/paste the
top of the CHANGELOG section if useful, but keep the tag annotation
focused: highlights only, not the full list.>

Public surface — stable under SemVer:
  * D-Bus  : dev.cirodev.hpd.PowerDaemon1
  * CLI    : hpdctl
  * State  : /var/lib/hpd/state.toml
  * Polkit : dev.cirodev.hpd.{set-tdp,set-charge,set-profile,set-fan-curve}
  * Config : /etc/hpd/config.toml

Hardware: ASUS ROG Ally / Ally X / Xbox Ally X

Verification gates at tag time:
  * cargo fmt --all -- --check                          clean
  * cargo clippy --workspace --all-targets -- -D warnings clean
  * cargo test --workspace                              <N> / <N> passing
  * cargo doc --workspace                               clean under -D warnings
  * Feature matrix                                      3/3 combos clean

See CHANGELOG.md for the full inventory.
EOF
)"
```

### 4b. Push the tag

```bash
git push origin "v${new_version}"
```

### 4c. Watch the workflow

```bash
gh run watch                          # or open Actions tab in the browser
```

`release.yml` should run within ~5 seconds of the tag push. Time
budget: ~10-15 minutes for the full build + artifact upload.

Expected outcome:

- **For a stable tag** (`vX.Y.Z`): a **Published** GitHub Release at
  `https://github.com/CiroDev-Git/hpd-handheld-power-daemon/releases/tag/vX.Y.Z`,
  with the tarball + `SHA256SUMS` (+ `.asc` if GPG configured) attached,
  and the release notes auto-extracted from the CHANGELOG section.
- **For an RC tag** (`vX.Y.Z-rc.N`): the same, but the Release is
  **Draft** (visible only to maintainers).

---

## 5. AUR update (manual fallback)

If **both `RELEASE_PAT` and `AUR_SSH_KEY`** are configured, the
[`aur-sync.yml`](../../.github/workflows/aur-sync.yml) workflow has
already pushed to AUR (the `release: published` event chained
automatically) — skip to §6.

If only `AUR_SSH_KEY` is set (no `RELEASE_PAT`), the Release was created
with `GITHUB_TOKEN`, which does not trigger downstream workflows — run
`aur-sync` once by hand:

```bash
gh workflow run aur-sync.yml -f version="${new_version}" -f packages=both
```

If neither is set, the fully-manual path is running
[`scripts/aur-sync.sh`](../../scripts/aur-sync.sh) locally for each
package (Arch host required — needs `makepkg`).

### 5a. One-time SSH setup

```bash
# AUR_KEY is the path to your private key with push access
mkdir -p ~/.ssh
cp /path/to/AUR_KEY ~/.ssh/aur
chmod 600 ~/.ssh/aur
ssh-keyscan -t ed25519,rsa aur.archlinux.org >> ~/.ssh/known_hosts 2>/dev/null
cat >> ~/.ssh/config <<'EOF'
Host aur.archlinux.org
    User aur
    IdentityFile ~/.ssh/aur
    StrictHostKeyChecking yes
    IdentitiesOnly yes
EOF
chmod 600 ~/.ssh/config
ssh aur@aur.archlinux.org help   # smoke-test
```

### 5b. Source package: `hpd-handheld-power-daemon`

```bash
cd <hpd-repo-checkout>
./scripts/aur-sync.sh hpd-handheld-power-daemon "${new_version}"
```

### 5c. Binary package: `hpd-handheld-power-daemon-bin`

The binary package's `sha256` is computed over the release tarball
attached to the GitHub Release. Confirm the Release is already
published (check the Releases tab) before running this — otherwise
the script fails with a 404 from `curl` and you'll need to retry
after the asset uploads.

```bash
cd <hpd-repo-checkout>
./scripts/aur-sync.sh hpd-handheld-power-daemon-bin "${new_version}"
```

Both invocations print the computed `sha256`, the rendered PKGBUILD
location, and the AUR push result. Re-running with the same version
is a no-op (the script detects "no changes to push").

---

## 6. Post-release housekeeping

### 6a. Re-open an `[Unreleased]` section (optional)

In practice this repo lands each release's CHANGELOG entry **inside the
§3d release PR** (a dated `## [X.Y.Z]` section, no floating
`[Unreleased]`), so there is usually nothing to do here. Skip unless you
deliberately keep an `[Unreleased]` heading for contributors to target.

If you do, it goes through a PR like any other `main` change — never a
direct push:

```bash
git checkout main
git pull --ff-only origin main          # picks up the bump commit + tag
git checkout -b chore/reopen-unreleased
```

Add a fresh top section to `CHANGELOG.md`:

```markdown
## [Unreleased]

(Nothing yet.)

---

## [1.1.0] — 2026-05-24
…
```

```bash
git add CHANGELOG.md
git commit -m "Open [Unreleased] section for the next release cycle"
git push -u origin chore/reopen-unreleased
gh pr create --base main --fill
gh pr merge --squash --delete-branch
```

### 6b. Announce

- Pin the GitHub Release in the repo's Discussions tab (if
  enabled).
- Tweet / post / Mastodon — link the GitHub Release page, not the
  raw tarball.
- If the release contains a breaking change, also drop a note in
  the project's issue tracker as a "v${new_version} migration
  notes" pinned issue.

### 6c. Watch for issues for 48 hours

The first 48 hours after publishing are when most install-time
bugs surface. Keep an eye on:

- `gh issue list --label bug --state open`
- The release's Discussions thread (if any).
- Any AUR comments on the two AUR packages.

If a critical bug surfaces, prepare a `vX.Y.Z+1` patch release
following this same runbook. **Do not delete or republish** the
broken release — see [`PIPELINE.md` §7](PIPELINE.md#7-rollback-policy).

---

## 7. If something goes wrong mid-release

### Workflow failed before the Release was created

The annotated tag exists on `origin` but no GitHub Release was
created. Two options:

1. Fix-forward: delete the tag, land the fix on `main` through a PR,
   then re-tag the new `main` HEAD and push:

   ```bash
   git tag -d "v${new_version}"
   git push --delete origin "v${new_version}"
   # fix the issue on a branch, open a PR, squash-merge, then:
   git checkout main && git pull --ff-only origin main
   git tag -a "v${new_version}" -m "..."
   git push origin "v${new_version}"        # tag push bypasses protection
   ```

2. Bump to the next patch: skip the broken number entirely.
   Cleanest if the broken tag was already advertised.

### Workflow succeeded but the Release contents are wrong

If it's a Draft (RC): edit or discard it in the GitHub UI.

If it's Published (stable): leave it. Cut a `vX.Y.Z+1` with the
fix. Mark the broken release's body with a ⚠ warning.

### GPG signing failed but everything else worked

You can re-run the signing step locally and upload `SHA256SUMS.asc`
manually as a Release asset:

```bash
# Reproduce the SHA256SUMS file from the published tarball
curl -fsSL "https://github.com/CiroDev-Git/.../v${new_version}/hpd-${new_version}-x86_64-linux.tar.gz" \
    -O
sha256sum hpd-${new_version}-x86_64-linux.tar.gz > SHA256SUMS
gpg --detach-sign --armor SHA256SUMS
gh release upload "v${new_version}" SHA256SUMS.asc
```

---

## 8. Time budget

| Step                                    | Approx. time        |
|-----------------------------------------|---------------------|
| §1 Pre-release sanity                   | 5-10 min            |
| §2-§3 Bump + CHANGELOG + branch + PR    | 10 min              |
| (Wait for PR CI green + squash-merge)   | 8-12 min            |
| §4 Tag + push + `release.yml` to finish | 10-15 min           |
| §5 AUR update (manual)                  | 5 min if scripted   |
| §6a Re-open [Unreleased] + push         | 2 min               |
| §6b Announce                            | 5 min               |
| **Total (happy path, no failures)**     | **45-60 min**       |

Budget a full hour the first few times. Subsequent releases
average closer to 30 minutes once the muscle memory is in place.

---

*Last updated: 2026-06-03 — release flow moved to PR + squash-merge
(main is branch-protected); tags still push directly.*
