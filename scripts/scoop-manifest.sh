#!/usr/bin/env bash
#
# Render the Scoop manifest for a released tag — brew-formula.sh's Windows
# sibling, under the same ruling (D30's drift shape): THE MANIFEST IS NOT
# CHECKED IN HERE. A version and a hash copied into this repository are correct
# for exactly one release and silently wrong for every one after it, in a file
# nothing in CI can check. So the manifest is generated from the release
# itself: the hash comes from the `.sha256` the release workflow publishes
# beside the zip, and a run against a tag that does not exist fails instead of
# inventing one.
#
#   scripts/scoop-manifest.sh v0.5.1 > ../scoop-tasqx/bucket/tasqx.json
#
# The `autoupdate` block is the one deliberate exception to "generated per
# release": it holds a URL *template* (`$version` is Scoop's placeholder, not
# ours), which lets the bucket bump itself between generations. The template
# names the same target this script does, so the guard over TARGET covers it.
#
# Needs `gh` and an authenticated session, because it reads the release's assets.
set -euo pipefail

TAG="${1:-}"
if [ -z "$TAG" ]; then
    echo "usage: $0 <tag>   e.g. $0 v0.5.1" >&2
    exit 2
fi
VERSION="${TAG#v}"
REPO="dimitritholen/tasqx"

# The one Windows target the release matrix builds. The single declaration
# site: URL, extract_dir and the autoupdate template all derive from it, and
# `readme.rs` asserts it is a target release.yml actually builds.
TARGET="x86_64-pc-windows-msvc"

WINDOWS="tasqx-${TAG}-${TARGET}.zip"
BASE="https://github.com/${REPO}/releases/download"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# `gh release download` refuses a tag that is not published, which is the check
# this script wants: a manifest for an unreleased version is a broken URL
# either way, and finding that out here beats finding it out in somebody's
# `scoop install`.
gh release download "$TAG" --repo "$REPO" --dir "$work" \
    --pattern "${WINDOWS}.sha256"

# The published `.sha256` files are `<sum>  <filename>` — the shasum format, so
# the sum is the first field.
HASH="$(awk '{print $1; exit}' "${work}/${WINDOWS}.sha256")"

cat <<EOF
{
    "version": "${VERSION}",
    "description": "Task manager that lives in the terminal and treats an AI agent as a normal user",
    "homepage": "https://github.com/${REPO}",
    "license": {
        "identifier": "FSL-1.1-MIT",
        "url": "https://github.com/${REPO}/blob/main/LICENSE.md"
    },
    "architecture": {
        "64bit": {
            "url": "${BASE}/${TAG}/${WINDOWS}",
            "hash": "${HASH}"
        }
    },
    "extract_dir": "tasqx-${TAG}-${TARGET}",
    "bin": "tasqx.exe",
    "checkver": {
        "github": "https://github.com/${REPO}"
    },
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "${BASE}/v\$version/tasqx-v\$version-${TARGET}.zip"
            }
        },
        "extract_dir": "tasqx-v\$version-${TARGET}",
        "hash": {
            "url": "\$url.sha256"
        }
    }
}
EOF
