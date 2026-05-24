#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Extract a single version section from CHANGELOG.md and print it to
# stdout. Used by .github/workflows/release.yml to feed
# `gh release create --notes-file`; can also be run manually:
#
#   ./scripts/extract-changelog-section.sh 1.0.0
#   ./scripts/extract-changelog-section.sh 1.1.0-rc.1
#   ./scripts/extract-changelog-section.sh 1.0.0 path/to/CHANGELOG.md
#
# Exits 0 with the section on stdout if the version was found,
# exits 1 (with an error on stderr) otherwise.
#
# The CHANGELOG.md format expected:
#
#   ## [1.0.0] — 2026-05-24
#   ...everything until the next "## [" header...
#   ## [0.9.0] — 2026-04-01

set -euo pipefail

if [ "${1-}" = "" ]; then
    echo "Usage: $0 <version> [changelog-path]" >&2
    echo "Example: $0 1.0.0" >&2
    exit 2
fi

version="$1"
file="${2:-CHANGELOG.md}"

if [ ! -r "$file" ]; then
    echo "Error: cannot read $file" >&2
    exit 2
fi

# Use awk so the regex anchoring is explicit and we don't have to
# worry about escaping for grep/sed. The `^## \[<version>\]` anchor
# is what guarantees we won't match a substring of a different
# version (e.g. searching "1.0" must not match "[1.0.1]").
section=$(awk -v v="$version" '
    BEGIN { in_section = 0 }
    $0 ~ "^## \\[" v "\\]"      { in_section = 1; print; next }
    in_section && /^## \[/      { exit }
    in_section                  { print }
' "$file")

# Strip any trailing horizontal rules / blank lines that belong to
# the separator between sections rather than to the section content.
section=$(printf '%s\n' "$section" | sed -E '/^---\s*$/d' | awk '
    /^$/ { blanks = blanks $0 "\n"; next }
    { if (blanks != "") { printf "%s", blanks; blanks = "" } print }
')

if [ -z "$section" ]; then
    echo "Error: no section found for version [$version] in $file" >&2
    echo "Available headers:" >&2
    grep -E '^## \[' "$file" >&2 || true
    exit 1
fi

printf '%s\n' "$section"
