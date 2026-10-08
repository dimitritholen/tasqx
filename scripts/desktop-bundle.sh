#!/usr/bin/env bash
# Build the Tasqx Desktop bundles for this host with `tauri build`, signed with
# whichever identities the environment carries and unsigned for the rest
# (D???). CI's desktop workflow runs this, and so does a maintainer signing a
# build by hand (docs/maintainers/desktop-release.md), so the two cannot drift.
#
# Every identity is optional. With none set — every pull request, and a tag run
# while the repository holds no signing secrets — the result is the same
# unsigned bundle, and the script still exits 0. What it reads:
#
#   APPLE_SIGNING_IDENTITY, APPLE_CERTIFICATE (base64 .p12),
#   APPLE_CERTIFICATE_PASSWORD     macOS codesign; Tauri imports the .p12 into
#                                  a throwaway keychain itself
#   APPLE_ID, APPLE_PASSWORD,
#   APPLE_TEAM_ID                  macOS notarization (app-specific password)
#   WINDOWS_CERTIFICATE_THUMBPRINT Authenticode, from a certificate already in
#                                  the user's store (CI imports the .pfx first)
#   WINDOWS_TIMESTAMP_URL          optional, defaults to DigiCert's
#   TAURI_SIGNING_PRIVATE_KEY,
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD
#                                  updater signatures: turns on
#                                  bundle.createUpdaterArtifacts, so every
#                                  updater bundle gets a .sig beside it
#
# It never tags, pushes or publishes anything; the bundles stay under
# apps/tasqx-desktop/src-tauri/target/release/bundle.
#
# Usage:
#   scripts/desktop-bundle.sh [extra tauri build arguments]
set -euo pipefail
cd "$(dirname "$0")/../apps/tasqx-desktop"

# A secret the repository does not hold reaches a GitHub step as an EMPTY
# variable, and Tauri tests these for presence, not content: an empty
# APPLE_CERTIFICATE is a certificate it then fails to decode. So empty means
# unset. The updater key PASSWORD is left alone on purpose — empty is a real
# value for a key generated without one, and unset makes Tauri prompt for it.
for name in APPLE_SIGNING_IDENTITY APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD \
    APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID WINDOWS_CERTIFICATE_THUMBPRINT \
    TAURI_SIGNING_PRIVATE_KEY; do
    if [[ -z "${!name:-}" ]]; then
        unset "$name"
    fi
done

say() { echo "desktop-bundle: $*"; }

# Updater artifacts need `plugins > updater` (its pubkey) in tauri.conf.json,
# and that config needs the updater plugin compiled into the app; without both,
# `tauri build` refuses. The app does not carry the plugin yet, so a key set
# today is reported and ignored rather than failing the run. The day the
# plugin and its pubkey land, this switches on with no change here.
if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]] && ! grep -q '"updater"' src-tauri/tauri.conf.json; then
    say "updater signatures: off (TAURI_SIGNING_PRIVATE_KEY is set, but tauri.conf.json has no plugins > updater yet)"
    unset TAURI_SIGNING_PRIVATE_KEY
elif [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    set -- "$@" --config '{"bundle":{"createUpdaterArtifacts":true}}'
    : "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:=}"
    export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
    say "updater signatures: on"
else
    say "updater signatures: off (TAURI_SIGNING_PRIVATE_KEY not set)"
fi

if [[ -n "${WINDOWS_CERTIFICATE_THUMBPRINT:-}" ]]; then
    set -- "$@" --config "{\"bundle\":{\"windows\":{\"certificateThumbprint\":\"${WINDOWS_CERTIFICATE_THUMBPRINT}\",\"digestAlgorithm\":\"sha256\",\"timestampUrl\":\"${WINDOWS_TIMESTAMP_URL:-http://timestamp.digicert.com}\"}}}"
    say "windows signing: on"
else
    say "windows signing: off (WINDOWS_CERTIFICATE_THUMBPRINT not set)"
fi

if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
    say "macos signing: on"
else
    say "macos signing: off (APPLE_SIGNING_IDENTITY not set)"
fi
if [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    say "macos notarization: on"
else
    say "macos notarization: off (needs APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID)"
fi

# node straight at the CLI rather than `npm run`: on Windows npm runs scripts
# through cmd.exe, which mangles the quotes in the JSON overrides above.
exec node node_modules/@tauri-apps/cli/tauri.js build "$@"
