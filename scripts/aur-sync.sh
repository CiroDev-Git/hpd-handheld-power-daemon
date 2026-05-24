#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Render the appropriate PKGBUILD template for an AUR package and
# push the result to ssh://aur@aur.archlinux.org/<pkgname>.git.
#
# Usage:
#   ./scripts/aur-sync.sh <pkgname> <version>
#
# Where <pkgname> is one of:
#   hpd-handheld-power-daemon       — source build (PKGBUILD.template)
#   hpd-handheld-power-daemon-bin   — prebuilt repack (PKGBUILD-bin.template)
#
# Requires (typically inside the archlinux:base-devel CI container):
#   git, openssh-client, curl, sha256sum (coreutils), makepkg (pacman/base-devel)
#
# Expects:
#   - The current working directory to be a checkout of the upstream
#     repo (i.e. CWD/package/aur/ exists).
#   - ~/.ssh/aur contains the AUR private key (mode 0600).
#   - ~/.ssh/known_hosts contains aur.archlinux.org's host key.
#   - ~/.ssh/config routes Host aur.archlinux.org to IdentityFile ~/.ssh/aur.
#
# The CI workflow (.github/workflows/aur-sync.yml) sets all of the
# above before invoking this script.

set -euo pipefail

if [ "${1-}" = "" ] || [ "${2-}" = "" ]; then
    echo "Usage: $0 <pkgname> <version>" >&2
    echo "Examples:" >&2
    echo "  $0 hpd-handheld-power-daemon     1.1.0" >&2
    echo "  $0 hpd-handheld-power-daemon-bin 1.1.0" >&2
    exit 2
fi

pkgname="$1"
version="$2"

case "$pkgname" in
    hpd-handheld-power-daemon)
        template="package/aur/PKGBUILD.template"
        tarball_url="https://github.com/CiroDev-Git/hpd-handheld-power-daemon/archive/v${version}.tar.gz"
        ;;
    hpd-handheld-power-daemon-bin)
        template="package/aur/PKGBUILD-bin.template"
        tarball_url="https://github.com/CiroDev-Git/hpd-handheld-power-daemon/releases/download/v${version}/hpd-${version}-x86_64-linux.tar.gz"
        ;;
    *)
        echo "Error: unknown package '$pkgname'. Expected hpd-handheld-power-daemon or hpd-handheld-power-daemon-bin." >&2
        exit 2
        ;;
esac

if [ ! -r "$template" ]; then
    echo "Error: template $template not found. Run from the repo root." >&2
    exit 2
fi

install_hook="package/aur/hpd.install"
if [ ! -r "$install_hook" ]; then
    echo "Error: install hook $install_hook not found." >&2
    exit 2
fi

echo "==> Computing sha256 of $tarball_url"
checksum=$(curl --fail --silent --show-error --location "$tarball_url" \
    | sha256sum | awk '{print $1}')
if [ -z "$checksum" ] || [ "${#checksum}" -ne 64 ]; then
    echo "Error: failed to compute sha256 (got '$checksum')" >&2
    exit 1
fi
echo "    sha256: $checksum"

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

echo "==> Cloning ssh://aur@aur.archlinux.org/${pkgname}.git"
git clone "ssh://aur@aur.archlinux.org/${pkgname}.git" "$workdir/aur" \
    --quiet --depth 1 || {
    echo "Error: AUR clone failed. Check that AUR_SSH_KEY has push access to $pkgname." >&2
    exit 1
}

cd "$workdir/aur"

echo "==> Rendering PKGBUILD (version=$version)"
sed -e "s|__VERSION__|${version}|g" \
    -e "s|__SHA256__|${checksum}|g" \
    "$OLDPWD/$template" > PKGBUILD

cp "$OLDPWD/$install_hook" hpd.install

echo "==> Generating .SRCINFO via makepkg"
# makepkg --printsrcinfo reads PKGBUILD and emits the canonical
# .SRCINFO that AUR uses for search/web display.
makepkg --printsrcinfo > .SRCINFO

# Sanity-check: pkgver in .SRCINFO must match the requested version.
srcinfo_ver=$(awk '$1 == "pkgver" { print $3; exit }' .SRCINFO)
if [ "$srcinfo_ver" != "$version" ]; then
    echo "Error: .SRCINFO pkgver=$srcinfo_ver does not match requested version=$version" >&2
    exit 1
fi

git config user.name  "hpd release bot"
git config user.email "noreply@github.com"

if git diff --quiet --exit-code; then
    echo "==> No changes to push (already at v$version). Done."
    exit 0
fi

git add PKGBUILD .SRCINFO hpd.install
git commit -m "Update to v${version}"
echo "==> Pushing to AUR"
git push origin master

echo "==> Done. AUR package $pkgname is now at v$version."
