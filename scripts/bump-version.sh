#!/usr/bin/env bash
# Compute the next [workspace.package] version from Conventional Commits
# since the last vX.Y.Z tag, and write it into Cargo.toml.
#
# Bump rule, highest wins: any commit with `!` before the `:` or a
# `BREAKING CHANGE:` footer -> major; any `feat(...)`/`feat:` -> minor;
# any `fix(...)`/`fix:` -> patch; otherwise (docs/test/chore/refactor/etc.
# only) -> no bump, exit 0 without touching anything.
#
# This is a local tool, run by hand as part of the release step (see
# CONTRIBUTING.md, "Releasing": local ff-merge into main, no PR, no
# CI bot committing to main). It never tags or pushes.
#
# Usage:
#   scripts/bump-version.sh            # print the computed version, change nothing
#   scripts/bump-version.sh --apply    # also write Cargo.toml and refresh Cargo.lock
set -euo pipefail
cd "$(dirname "$0")/.."

apply=false
if [[ "${1:-}" == "--apply" ]]; then
    apply=true
elif [[ "${1:-}" != "" ]]; then
    echo "usage: $0 [--apply]" >&2
    exit 2
fi

last_tag="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)"
range="HEAD"
if [[ -n "$last_tag" ]]; then
    range="${last_tag}..HEAD"
fi

# Subjects decide feat/fix; the full message body (subjects and bodies
# concatenated, one blob) only needs a substring check for the
# `BREAKING CHANGE:` footer, so it never needs splitting into records.
subjects="$(git log --format='%s' $range 2>/dev/null || true)"
bodies="$(git log --format='%B' $range 2>/dev/null || true)"

if [[ -z "$subjects" ]]; then
    echo "no commits since ${last_tag:-the start of history} — nothing to bump" >&2
    exit 0
fi

bump=none
if grep -qE '^BREAKING CHANGE:' <<<"$bodies" || grep -qE '^[a-zA-Z]+(\([^)]*\))?!:' <<<"$subjects"; then
    bump=major
elif grep -qE '^feat(\([^)]*\))?:' <<<"$subjects"; then
    bump=minor
elif grep -qE '^fix(\([^)]*\))?:' <<<"$subjects"; then
    bump=patch
fi

current="$(grep -m1 '^version = "' Cargo.toml | sed -E 's/version = "([^"]+)"/\1/')"
IFS='.' read -r major minor patch <<<"$current"

case "$bump" in
none)
    echo "no feat/fix commits since ${last_tag:-the start of history} (only docs/test/chore/etc.) — version stays ${current}" >&2
    exit 0
    ;;
major)
    new="$((major + 1)).0.0"
    ;;
minor)
    new="${major}.$((minor + 1)).0"
    ;;
patch)
    new="${major}.${minor}.$((patch + 1))"
    ;;
esac

if $apply; then
    sed -i -E "0,/^version = \"[^\"]+\"/s//version = \"${new}\"/" Cargo.toml
    # tasqx-cli's path dependency on tasqx-core pins a literal version
    # requirement rather than inheriting workspace.package.version (Cargo
    # has no `version.workspace = true` for a dependency's version req) —
    # left at the old value this is a silent staleness, not an error, until
    # someone tries to publish. Bump it alongside.
    sed -i -E "s/(tasqx-core = \{ path = \"\.\.\/tasqx-core\", version = \")[^\"]+(\" \})/\1${new}\2/" \
        crates/tasqx-cli/Cargo.toml
    # Keep Cargo.lock's own recorded crate versions in sync immediately,
    # rather than leaving that for whoever next runs a cargo command to
    # discover as an unexplained lockfile diff.
    cargo check --workspace --quiet --offline || cargo check --workspace --quiet
    echo "bumped ${current} -> ${new} (${bump}), Cargo.toml, tasqx-cli's tasqx-core dependency and Cargo.lock written"
else
    echo "${current} -> ${new} (${bump})"
fi
