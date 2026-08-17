#!/bin/sh
#
# tasqx installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh -s -- --dry-run
#
# NOTHING RUNS AT PARSE TIME. The whole body is `main`, and `main "$@"` is the
# last line of the file. `sh` executes a pipe incrementally: a connection
# dropped at byte N runs bytes 0..N, so a script whose statements execute as
# they are parsed can half-install. With every statement inside a function a
# truncated download is a no-op, because the shell never reaches the call.
#
# Every subprocess gets `</dev/null` for the same reason — a child that reads
# stdin eats the remaining bytes of the script itself. Commands inside a
# pipeline (the grep/tail/tr/sed below) are the exception and must NOT get it:
# their stdin is the pipe, not the script, and redirecting it there would
# break the parse rather than protect it.
#
# No version and no checksum is written down here, which is the ruling
# scripts/brew-formula.sh's header carries: a list copied into this repository
# is correct for exactly one release and silently wrong for every one after
# it. The tag comes from the release itself.
#
# This script never uses sudo or doas, and writes nowhere but
# ${TASQX_INSTALL:-$HOME/.local/bin}.
#
# `set -eu` lives inside main for the same reason everything else does: it is a
# command, and a command outside main is a command a truncated pipe can run.
# `set` is dynamic, so every function main calls runs under it anyway.

# One line per argument, always on stderr.
err() {
    printf '%s\n' "$@" >&2
}

usage() {
    cat <<'EOF'
tasqx installer

  curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
  curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh -s -- --dry-run

A pipe passes no arguments, which is why the flags go after `sh -s --`.

Options:
  --dry-run       Print what would happen and stop, whichever other flag it is
                  combined with. Writes nothing, creates no directory,
                  downloads nothing, removes nothing.
  --uninstall     Remove an installed tasqx: its completion block, then the
                  binary, then the directory if this script made it and it is
                  now empty. Never your task database.
  --completions   Also switch Tab completion on, after the install. Opt-in: the
                  bare one-liner never edits a shell startup file.
  --help          This text.

Environment:
  TASQX_VERSION   Tag to install, with or without the leading v. Default: the
                  newest release.
  TASQX_INSTALL   Directory to install into. Default: $HOME/.local/bin.
EOF
}

# The complete mapping, and nothing else is mapped. tasqx publishes four
# archives (.github/workflows/release.yml is the matrix); three of them are
# reachable from a POSIX shell, the fourth being x86_64-pc-windows-msvc, which
# install.ps1 serves. Anything absent here — ARM Linux, musl, the BSDs — has no
# build to point at, and a guessed triple would 404 with no explanation.
#
# Sets uname_s, uname_m and target.
#
# SC2217 says `uname` does not read stdin, and it is right. The redirect stays
# anyway: it is the rule this whole script follows for every child it starts
# (see the header), and a rule with one hand-audited exception in it is a rule
# nobody can check at a glance.
# shellcheck disable=SC2217
detect_target() {
    uname_s="$(uname -s </dev/null)"
    uname_m="$(uname -m </dev/null)"

    # macOS prints `arm64` where the triple says `aarch64`. Building the target
    # as "$(uname -m)-apple-darwin" fails on Apple Silicon, which is the single
    # most likely machine to run a piped installer.
    case "${uname_s}/${uname_m}" in
    Darwin/arm64) target="aarch64-apple-darwin" ;;
    Darwin/x86_64) target="x86_64-apple-darwin" ;;
    Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
    *)
        # Names the machine's own uname values, not the triple that failed to
        # build: the reader has to recognise their own box in this line.
        err "tasqx has no prebuilt binary for ${uname_s}/${uname_m}." \
            "Prebuilt targets: x86_64-linux, aarch64-macos, x86_64-macos, x86_64-windows." \
            "Build from source instead: cargo install --git https://github.com/dimitritholen/tasqx tasqx-cli"
        return 2
        ;;
    esac
}

# Sets install_dir.
resolve_install_dir() {
    install_dir="${TASQX_INSTALL:-}"
    if [ -n "$install_dir" ]; then
        return 0
    fi
    # Refuse rather than operate on `/.local/bin`. With HOME unset the default
    # expands to a path at the filesystem root, which is the one place this
    # script must never touch.
    if [ -z "${HOME:-}" ]; then
        err "HOME is unset or empty, so there is no default install directory." \
            "Set TASQX_INSTALL to the directory tasqx should go in."
        return 2
    fi
    install_dir="${HOME}/.local/bin"
}

# Both `0.3.0` and `v0.3.0` are accepted and become `v0.3.0` ONCE, here, so the
# tag that reaches the URL path and the tag that reaches the filename are the
# same string. The archive keeps its v: release.yml packages ${GITHUB_REF_NAME}
# unstripped, so the file is `tasqx-v0.3.0-<target>.tar.gz`. Stripping the v —
# what most Rust installers do — 404s on every download.
#
# Sets tag.
normalise_tag() {
    case "$1" in
    v*) tag="$1" ;;
    *) tag="v$1" ;;
    esac
}

# ^v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$ and nothing looser. grep's stdin is
# the pipe from printf, so it gets no </dev/null.
is_release_tag() {
    printf '%s\n' "$1" | grep -q '^v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$'
}

# Resolve the newest tag from the `Location` header of releases/latest.
#
# The redirect, deliberately, and NOT the GitHub JSON API: api.github.com is
# rate-limited to 60 requests/hour per unauthenticated IP, so behind a shared
# NAT it fails for reasons the person running this cannot see.
#
# Sets tag.
resolve_latest_tag() {
    latest_url="https://github.com/${REPO}/releases/latest"
    tag_prefix="https://github.com/${REPO}/releases/tag/"

    # CHECK 1 — the fetch's own exit status. A failed fetch must not leave an
    # empty version behind, which would build `.../download//tasqx--x86_64...`
    # and blame the download for a failure that happened here.
    if command -v curl >/dev/null 2>&1; then
        if ! headers="$(curl -fsSLI "$latest_url" </dev/null 2>/dev/null)"; then
            err "could not reach ${latest_url}." \
                "Check the connection, or pin a tag with TASQX_VERSION=v0.3.0."
            return 1
        fi
    elif command -v wget >/dev/null 2>&1; then
        # wget writes the headers to stderr, indents them, and marks a
        # redirect with a trailing ` [following]`; the parse below tolerates
        # all three so the two fetchers share one code path.
        if ! headers="$(wget -S --spider -O /dev/null "$latest_url" </dev/null 2>&1)"; then
            err "could not reach ${latest_url}." \
                "Check the connection, or pin a tag with TASQX_VERSION=v0.3.0."
            return 1
        fi
    else
        err "this installer needs curl or wget, and found neither on PATH."
        return 1
    fi

    # CHECK 2 — a Location header is there at all, with its CR stripped. Header
    # lines are CRLF-terminated, and a naive cut/sed leaves the carriage return
    # inside the URL; the resulting 404 names neither the file nor the cause.
    # .gitattributes:1-15 records this exact defect class for shell scripts.
    # The last Location wins, because a chain of redirects ends at the tag page.
    location="$(printf '%s\n' "$headers" |
        grep -i '^[[:blank:]]*location:' |
        tail -n 1 |
        tr -d '\015' |
        sed -e 's/^[[:blank:]]*[Ll][Oo][Cc][Aa][Tt][Ii][Oo][Nn]:[[:blank:]]*//' \
            -e 's/[[:blank:]]*\[following\][[:blank:]]*$//')" || location=""
    if [ -z "$location" ]; then
        err "no Location header came back from ${latest_url}." \
            "Something between here and GitHub answered instead of redirecting." \
            "Pin a tag with TASQX_VERSION=v0.3.0 to skip this lookup."
        return 1
    fi

    # CHECK 3 — where the redirect actually went. A captive portal answering
    # 200 HTML, a 429, and a repository with no releases all arrive here, and
    # without this the "version" becomes a word like `releases`.
    case "$location" in
    "${tag_prefix}"?*) ;;
    *)
        err "the release redirect went somewhere unexpected: ${location}" \
            "Expected ${tag_prefix}<tag>. Refusing to guess a version."
        return 1
        ;;
    esac
    tag="${location#"$tag_prefix"}"

    # CHECK 4 — the tag's shape.
    if ! is_release_tag "$tag"; then
        err "${latest_url} redirected to something that is not a release tag: ${tag}" \
            "Expected vMAJOR.MINOR.PATCH."
        return 1
    fi
}

# Sets tag and version_source.
resolve_version() {
    if [ -n "${TASQX_VERSION:-}" ]; then
        normalise_tag "$TASQX_VERSION"
        # Literal: the parenthetical names the source, and the value it holds
        # is already on the same line.
        version_source="(from \$TASQX_VERSION)"
        if ! is_release_tag "$tag"; then
            err "TASQX_VERSION=${TASQX_VERSION} is not a release tag." \
                "Expected vMAJOR.MINOR.PATCH, with or without the v: 0.3.0 or v0.3.0."
            return 2
        fi
        return 0
    fi
    version_source='(resolved from releases/latest)'
    resolve_latest_tag
}

# Removes everything this script creates that is not the final installed file:
# the work directory, and the staging copy inside the destination. Both names
# may be unset — the trap is armed before either exists — so both are guarded
# rather than passed to `rm` empty.
cleanup() {
    if [ -n "${tmp:-}" ]; then
        rm -rf "$tmp"
    fi
    if [ -n "${staged:-}" ]; then
        rm -f "$staged"
    fi
}

# Sets tmp to a private work directory and arms its cleanup.
#
# 700 explicitly. Every mktemp seen does that already, but an archive is
# unpacked here and a file in it is made executable, and "does that already" is
# not something this script gets to verify at run time.
#
# The trap is armed in the same breath as the directory, so no failure between
# here and the last line of main can leave an unpacked archive behind. HUP, INT
# and TERM are listed separately because a POSIX EXIT trap does not run on a
# signal — Ctrl-C during a download would otherwise leave the whole tree.
#
# shellcheck disable=SC2217
make_tmp() {
    if ! tmp="$(mktemp -d </dev/null)"; then
        err "could not create a temporary directory."
        return 1
    fi
    chmod 700 "$tmp" </dev/null
    trap 'cleanup' EXIT
    trap 'cleanup; exit 130' HUP INT TERM
}

# Fetch $1 into the file $2. curl first, wget second — the same order and the
# same "neither" wording resolve_latest_tag uses.
#
# One attempt, no retry. Both fetchers are made to speak: `curl -f` without
# `-S` and `wget -q` each fail silently, and under `set -eu` the script would
# then die with no output at all, which reads as a crash rather than as a
# network problem the reader can act on.
download() {
    if command -v curl >/dev/null 2>&1; then
        if ! curl -fsSL -o "$2" "$1" </dev/null; then
            err "download failed: $1"
            return 1
        fi
    elif command -v wget >/dev/null 2>&1; then
        if ! wget -q -O "$2" "$1" </dev/null; then
            err "download failed: $1"
            return 1
        fi
    else
        err "this installer needs curl or wget, and found neither on PATH."
        return 1
    fi
}

# Sets actual_sum to the SHA-256 of the file $1, as hex.
#
# Three probes, because no one hasher is everywhere: `sha256sum` is coreutils
# and macOS ships none, `shasum` is perl's and minimal Linux images often have
# no perl, `openssl` is the last resort. If all three are missing this ABORTS
# naming all three. There is deliberately no skip path and no `|| true`: an
# installer that reports success having verified nothing is worse than one that
# never claimed to verify, because the user cannot tell the two apart.
#
# `sha256sum -c` is never used, in either direction. macOS has no sha256sum at
# all, and `-c` matches the basename recorded INSIDE the .sha256 file — which
# an archive downloaded to a temp directory does still carry, but which any
# future change to the download name would break for a perfectly valid
# archive. Hex strings are compared instead.
#
# shellcheck disable=SC2217
hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        if ! hash_line="$(sha256sum "$1" </dev/null)"; then
            err "sha256sum could not read $1."
            return 1
        fi
    elif command -v shasum >/dev/null 2>&1; then
        if ! hash_line="$(shasum -a 256 "$1" </dev/null)"; then
            err "shasum could not read $1."
            return 1
        fi
    elif command -v openssl >/dev/null 2>&1; then
        if ! hash_line="$(openssl dgst -sha256 "$1" </dev/null)"; then
            err "openssl could not read $1."
            return 1
        fi
        # `openssl dgst` prints `SHA2-256(file)= <sum>` (`SHA256(file)= <sum>`
        # on 1.x), so the sum is the LAST field where the other two put it
        # first. Reduced to one field here so the caller has one shape.
        hash_line="${hash_line##* }"
    else
        err "no SHA-256 tool on PATH, so the download cannot be verified." \
            "Install one of: sha256sum (coreutils), shasum (perl), openssl." \
            "Refusing to install a binary this script cannot check."
        return 1
    fi
    actual_sum="$(printf '%s\n' "$hash_line" | awk '{print $1; exit}')"
    if [ -z "$actual_sum" ]; then
        err "the SHA-256 tool produced no digest for $1."
        return 1
    fi
}

# Downloads the archive and its published checksum into $tmp, and refuses to go
# further unless they agree. This runs BEFORE the unpack, never after: tar
# writing files out of a tampered archive has already done the damage the
# checksum exists to prevent.
#
# Sets archive_name and archive_path.
#
# shellcheck disable=SC2217
fetch_and_verify() {
    archive_name="tasqx-${tag}-${target}.tar.gz"
    archive_path="${tmp}/${archive_name}"
    sums_path="${archive_path}.sha256"

    download "$archive_url" "$archive_path" || return 1
    download "${archive_url}.sha256" "$sums_path" || return 1

    # The published file is shasum's format — `<sum>  <filename>`, two spaces,
    # written by release.yml:159 — so the sum is field 1.
    # scripts/brew-formula.sh:45-52 reads the same files the same way.
    expected_sum="$(awk '{print $1; exit}' "$sums_path" </dev/null)"
    if [ -z "$expected_sum" ]; then
        err "the checksum published at ${archive_url}.sha256 is empty." \
            "Refusing to install an unverified binary."
        return 1
    fi

    hash_file "$archive_path" || return 1

    # Compared case-insensitively by lowercasing both. shasum and openssl print
    # lowercase and Get-FileHash prints upper; a digest that differs only in
    # case is the same digest, and refusing it would be a false alarm nobody
    # could diagnose from the message.
    expected_sum="$(printf '%s\n' "$expected_sum" | tr 'ABCDEF' 'abcdef')"
    actual_sum="$(printf '%s\n' "$actual_sum" | tr 'ABCDEF' 'abcdef')"

    if [ "$expected_sum" != "$actual_sum" ]; then
        # Both sums, in full. "checksum mismatch" alone leaves the reader
        # unable to tell a truncated download from a replaced asset, and the
        # published sum is the one thing they can check by hand against the
        # release page.
        err "checksum mismatch for ${archive_name} — refusing to install." \
            "  expected ${expected_sum}" \
            "  actual   ${actual_sum}" \
            "The download is corrupt, or the published asset changed."
        return 1
    fi
}

# Unpacks the verified archive and moves the binary into place.
#
# Sets installed_path.
#
# shellcheck disable=SC2217
install_binary() {
    if ! tar xzf "$archive_path" -C "$tmp" </dev/null; then
        err "could not unpack ${archive_name}."
        return 1
    fi

    # LOCATED, not computed. Everything in the archive sits under a
    # `tasqx-<tag>-<target>/` directory because release.yml:158 tars a staging
    # directory by name, and nothing in CI asserts that — it is observed
    # packaging behaviour, not a contract. A hardcoded
    # "$tmp/tasqx-$tag-$target/tasqx" turns any repacking change into "No such
    # file or directory" naming a path nobody wrote; a search turns it into a
    # sentence about what the archive actually held.
    #
    # An exact `-name tasqx` matches the binary and nothing else: the shipped
    # completions are `tasqx.bash`, `tasqx.zsh` and so on.
    if ! matches="$(find "$tmp" -type f -name tasqx </dev/null)"; then
        err "could not search ${tmp} for the tasqx binary."
        return 1
    fi
    if [ -z "$matches" ]; then
        err "no file named 'tasqx' inside ${archive_name}." \
            "The archive is not laid out the way this installer expects."
        return 1
    fi
    match_count="$(printf '%s\n' "$matches" | wc -l | tr -d '[:blank:]')"
    if [ "$match_count" != "1" ]; then
        err "found ${match_count} files named 'tasqx' inside ${archive_name}:" \
            "$matches" \
            "Refusing to guess which of them is the binary."
        return 1
    fi

    # Unconditional. ~/.local/bin does not exist on a fresh machine and it is
    # the default destination, so "the directory is already there" is the case
    # this must not assume.
    if ! mkdir -p "$install_dir" </dev/null; then
        err "could not create ${install_dir}."
        return 1
    fi

    installed_path="${install_dir}/tasqx"

    # Copied to a temp name INSIDE the destination and renamed, never written
    # straight over the live path: `cp` truncates before it writes, so an
    # interrupt halfway through would leave a truncated file where a working
    # tasqx used to be. `mv` within one directory is atomic, so the only file
    # that ever appears under the final name is a complete one. Staging inside
    # the destination rather than in $tmp is what makes the rename atomic —
    # across filesystems `mv` degrades to copy-then-unlink.
    if ! staged="$(mktemp "${install_dir}/.tasqx.XXXXXX" </dev/null)"; then
        err "could not write to ${install_dir}."
        return 1
    fi
    if ! cp "$matches" "$staged" </dev/null; then
        err "could not copy the binary into ${install_dir}."
        return 1
    fi
    # Explicit, because otherwise the mode inside the archive and the caller's
    # umask both get a vote — and mktemp deliberately creates 600, which would
    # install a tasqx nobody can run.
    if ! chmod 755 "$staged" </dev/null; then
        err "could not make ${staged} executable."
        return 1
    fi
    if ! mv -f "$staged" "$installed_path" </dev/null; then
        err "could not move the binary into place at ${installed_path}."
        return 1
    fi
    # Published under its final name, so cleanup must no longer remove it.
    staged=""
}

# Prints $1 with its directory resolved through symlinks and `..`.
#
# `readlink -f` is GNU and macOS 12 and older has no such flag; `test -ef` is
# not POSIX. The two paths compared below come from different places — one
# built from $TASQX_INSTALL, one printed by `command -v` — so comparing the
# strings as given would report a mismatch between /home/x/.local/bin/tasqx and
# /home/x/./.local/bin/tasqx and warn about a binary that is the same file.
abs_path() {
    ap_dir="$(CDPATH='' cd -P -- "$(dirname -- "$1")" >/dev/null 2>&1 && pwd)" || return 1
    printf '%s/%s\n' "$ap_dir" "$(basename -- "$1")"
}

# The last line the user sees, and the one that decides whether this install
# was any use. Everything above it can be true while the shell still runs a
# different tasqx: CLAUDE.md tells every contributor to `cargo install --path`,
# and ~/.cargo/bin normally precedes ~/.local/bin on PATH. An installer that
# says "installed" while another binary answers `tasqx --version` gets read as
# broken — or, worse, is not noticed at all.
report_path() {
    printf '%s\n' "tasqx ${tag} installed to ${installed_path}"

    resolved="$(command -v tasqx 2>/dev/null)" || resolved=""

    # Case 1: the shell already runs what was just written. Nothing to say.
    if [ -n "$resolved" ] &&
        [ "$(abs_path "$resolved")" = "$(abs_path "$installed_path")" ]; then
        return 0
    fi

    # Case 2: nothing named tasqx is on PATH at all. That can only mean the
    # destination is not on it, because a file was just made executable there.
    if [ -z "$resolved" ]; then
        # The export line re-abbreviates $HOME, because that is the form that
        # belongs in the reader's shell rc — an absolute /home/someone path
        # pasted from a terminal is right on exactly one machine.
        path_hint="$install_dir"
        if [ -n "${HOME:-}" ]; then
            case "$install_dir" in
            "${HOME}"/*) path_hint="\$HOME${install_dir#"${HOME}"}" ;;
            esac
        fi
        err "warning: ${install_dir} is not on your PATH. Add it: export PATH=\"${path_hint}:\$PATH\""
        return 0
    fi

    # Case 3: something else wins. The version comes from running that binary,
    # because the whole point of the line is to tell the reader which of two
    # tasqx installations they have been talking to, and a guessed number would
    # make the warning as untrustworthy as the situation it reports.
    if ! other_version="$("$resolved" --version </dev/null 2>/dev/null)"; then
        other_version=""
    fi
    # `tasqx --version` prints `tasqx 0.3.0 (fbf15dcb56c5)`, so the LAST field
    # is the build hash and only the first digit-leading field is the number
    # the reader is comparing against. Scanned rather than taken as field 2, so
    # a foreign binary that prints its version alone still answers.
    other_version="$(printf '%s\n' "$other_version" |
        awk 'NR == 1 { for (i = 1; i <= NF; i++) if ($i ~ /^[0-9]/) { print $i; exit } }')"
    if [ -z "$other_version" ]; then
        other_version="version unknown"
    fi
    err "warning: 'tasqx' on your PATH resolves to ${resolved} (${other_version}), not the one just installed."
}

# Sets comp_shell to the name of the shell whose startup file should be edited,
# or to the empty string when this machine gives no usable answer.
#
# $SHELL first, because that is the variable every POSIX login shell sets to its
# own path and the only one `tasqx completions` itself consults
# (complete/install.rs:364-377). The parent process name is the fallback, and it
# exists because $SHELL is routinely unset in containers and non-login CI
# sessions — the machines a piped installer is most likely to run on. `ps` is
# probed rather than assumed: a container image with no procps is exactly the
# same population.
#
# `sh`, `dash` and `ash` are answers, not shells to set up. They are what runs
# THIS script, they are never a shell tasqx can complete, and passing one on
# would turn "I could not tell" into a refusal about a shell nobody chose.
#
# A leading `-` is stripped because a login shell's argv[0] is `-bash`, and the
# path is reduced to its last component the way `canonical_shell_name` reduces
# `/usr/bin/zsh`.
#
# shellcheck disable=SC2217
resolve_completion_shell() {
    comp_shell="${SHELL:-}"
    if [ -z "$comp_shell" ] && command -v ps >/dev/null 2>&1; then
        comp_shell="$(ps -o comm= -p "$PPID" </dev/null 2>/dev/null)" || comp_shell=""
    fi
    comp_shell="${comp_shell##*/}"
    comp_shell="${comp_shell#-}"
    case "$comp_shell" in
    "" | sh | dash | ash) comp_shell="" ;;
    esac
}

# Names the command the reader can run by hand, on the two paths where this
# script declines to run it for them. One sentence, one command, and never an
# error: see completions_step.
completion_hint() {
    err "warning: $1" \
        "Switch it on yourself with: tasqx completions <shell> --install"
}

# `--completions`: turn Tab completion on for the shell this machine appears to
# run.
#
# EXIT 0 ON EVERY FAILURE, deliberately. This runs after the binary is in place
# and reported, so the install has already succeeded; a non-zero exit here would
# tell the user their install is broken when the only thing that did not happen
# is a convenience they can add in one command. Both arms therefore print a
# warning naming that command and return 0.
#
# The shell name is passed EXPLICITLY. `tasqx completions --install` with no
# name reads $SHELL itself and refuses when it is empty
# (complete/install.rs:375-385) — so omitting the name here would put the
# fallback above out of reach on precisely the containers and CI sessions it was
# written for.
#
# `-y` is not optional. `install_into` withholds consent when stdin is not a
# terminal (complete/install.rs:994), and under `curl … | sh` stdin is the
# script itself. Without `-y` this path writes nothing, ever, on the only
# transport this installer advertises.
#
# `</dev/null` for the reason the header gives, and this is the call that
# motivated the rule: `tasqx completions` is the one child here that reads stdin
# on purpose.
completions_step() {
    resolve_completion_shell
    if [ -z "$comp_shell" ]; then
        completion_hint "cannot tell which shell to set up: \$SHELL is not set and the parent process is not a shell tasqx completes."
        return 0
    fi
    if ! "$installed_path" completions "$comp_shell" --install -y </dev/null; then
        completion_hint "could not switch on ${comp_shell} completions."
        return 0
    fi
}

# Takes the completion block back out, while there is still a binary able to do
# it.
#
# Silent when there is nothing to remove: `tasqx completions <shell>
# --uninstall` exits 4 on a file with no block (D33 — a command that changed
# nothing must not answer ok), and that is the ordinary case for anyone who
# never passed `--completions`. The block is removed byte for byte, so
# attempting it unconditionally costs a process and nothing else.
#
# `-y` for the same reason completions_step needs it: uninstall_from consults
# the same consent gate (complete/install.rs:1053), and without it the block
# would quietly survive the uninstall.
uninstall_completions() {
    resolve_completion_shell
    if [ -z "$comp_shell" ]; then
        return 0
    fi
    if "$1" completions "$comp_shell" --uninstall -y </dev/null >/dev/null 2>&1; then
        printf '%s\n' "removed the ${comp_shell} completion block"
    fi
}

# `--uninstall`: undo the install, in the only order that works.
#
# The completion block goes FIRST, because the program that knows how to remove
# it is the binary step 2 deletes. Reversed, this would invoke a path that no
# longer exists and leave a `source <(…)` line in a startup file pointing at a
# tasqx that is gone — a shell that prints an error on every new terminal.
#
# THE STORE IS NEVER TOUCHED. $TASQX_DB and the user's tasks are their data, not
# installer state; an uninstaller that deletes them has destroyed the one thing
# reinstalling cannot bring back.
#
# Nothing installed is exit 0, not an error. Running an uninstaller twice — or
# on a machine where the install went somewhere else — is ordinary, and a
# non-zero exit there makes every wrapper script treat a clean machine as a
# failure.
#
# SC2217 again for `rm` and `rmdir`, and the header's answer stands: one rule
# for every child, rather than a rule with hand-audited exceptions in it.
# shellcheck disable=SC2217
uninstall() {
    installed_path="${install_dir}/tasqx"

    if [ ! -e "$installed_path" ]; then
        printf '%s\n' "nothing to remove at ${installed_path}"
        return 0
    fi

    uninstall_completions "$installed_path"

    if ! rm -f "$installed_path" </dev/null; then
        err "could not remove ${installed_path}."
        return 1
    fi
    printf '%s\n' "removed ${installed_path}"

    # Only the default directory, and only with `rmdir`, which refuses a
    # directory that is not empty and is therefore incapable of taking anything
    # else with it. A directory named in $TASQX_INSTALL is the user's own
    # choice of location and is left alone whatever is in it.
    #
    # NOTE: "this script created it" cannot be known from a later process — no
    # state is kept between an install and its uninstall, by design — so this is
    # the closest safe approximation: the path this script would have had to
    # create itself, empty, removed by a call that cannot delete a file.
    if [ -z "${TASQX_INSTALL:-}" ]; then
        rmdir "$install_dir" </dev/null 2>/dev/null || true
    fi
}

# `--dry-run --uninstall`: name what an uninstall would take out, and take out
# nothing.
#
# This exists because the ordering it replaces was a defect and not a gap in the
# documentation: main read `--uninstall` before it read `--dry-run`, so the flag
# whose entire promise is "writes nothing" performed a real uninstall. A dry run
# that deletes a binary is worse than no dry run at all, because the person who
# typed it chose it in order to be safe.
#
# The binary is deliberately never RUN here. Naming the startup file the
# completion block lives in would read better, and the only thing on the machine
# that knows that path is the binary — but asking it means starting the same
# program the real uninstall starts, in the one mode that promised not to.
dry_run_uninstall() {
    installed_path="${install_dir}/tasqx"

    printf '%s\n' "tasqx installer (dry run — nothing will be removed)"
    if [ -e "$installed_path" ]; then
        printf '  %-12s %s\n' "binary" "$installed_path"
    else
        printf '  %-12s %s\n' "binary" "${installed_path} (not there — nothing to remove)"
    fi

    resolve_completion_shell
    if [ -n "$comp_shell" ]; then
        printf '  %-12s %s\n' "completions" \
            "the ${comp_shell} block, via: tasqx completions ${comp_shell} --uninstall -y"
    else
        printf '  %-12s %s\n' "completions" \
            "none — no shell tasqx completes was detected, so no block would be touched"
    fi

    # Said even though it is a no-op, because "what would be removed" is exactly
    # the question a reader is asking here, and the directory is the one thing
    # in the list whose fate depends on a variable they may not remember setting.
    if [ -z "${TASQX_INSTALL:-}" ]; then
        printf '  %-12s %s\n' "directory" \
            "${install_dir}, and only if removing the binary leaves it empty"
    else
        printf '  %-12s %s\n' "directory" \
            "${install_dir} is yours (\$TASQX_INSTALL) and is left alone"
    fi
    printf '  %-12s %s\n' "store" "never touched, here or in a real uninstall"
}

main() {
    set -eu

    REPO="dimitritholen/tasqx"
    action="install"
    # A MODIFIER, not an action, and that is the fix rather than a style
    # preference: while `--dry-run` set `action` too, the last of
    # `--dry-run` and `--uninstall` on the command line won, so
    # `--dry-run --uninstall` uninstalled for real and `--uninstall --dry-run`
    # printed an install plan for an install nobody asked for. Held separately,
    # it can win over every action below instead of racing them.
    dry_run="no"
    # A modifier, not an action. D57 rules that completion is switched on
    # without asking only where a package manager did the installing; every
    # other route asks first, and here "asks" means the user typed the flag.
    # The bare one-liner must reach the end of an install having edited no
    # startup file at all.
    want_completions="no"

    while [ "$#" -gt 0 ]; do
        case "$1" in
        --dry-run) dry_run="yes" ;;
        --uninstall) action="uninstall" ;;
        --completions) want_completions="yes" ;;
        --help)
            usage
            return 0
            ;;
        *)
            err "unknown option: $1"
            usage >&2
            return 2
            ;;
        esac
        shift
    done

    resolve_install_dir || return

    # Before detect_target and before resolve_version, because neither is an
    # answer this needs: removing a binary requires no platform triple and no
    # network. An uninstall that consulted GitHub would fail on the machine of
    # anyone offline, or after the release it was installed from was deleted.
    if [ "$action" = "uninstall" ]; then
        # `--dry-run` first, and this is the ordering the whole flag depends on.
        if [ "$dry_run" = "yes" ]; then
            dry_run_uninstall
            return 0
        fi
        uninstall
        return
    fi

    detect_target || return
    resolve_version || return

    # Built once, from the single normalised tag, so the path and the filename
    # can never disagree. The checksum lives at this URL with `.sha256`
    # appended; the download step builds it where it uses it.
    archive_url="https://github.com/${REPO}/releases/download/${tag}/tasqx-${tag}-${target}.tar.gz"

    if [ "$dry_run" = "yes" ]; then
        # These four field labels are a contract: the CI installer job and
        # install.ps1 both assert on them. They are not a debugging aid.
        printf '%s\n' "tasqx installer (dry run — nothing will be written)"
        printf '  %-10s %-18s%s\n' "version" "$tag" "$version_source"
        printf '  %-10s %s %s -> %s\n' "platform" "$uname_s" "$uname_m" "$target"
        printf '  %-10s %s\n' "archive" "$archive_url"
        printf '  %-10s %s\n' "install to" "${install_dir}/tasqx"
        # Said only when it was asked for, so the four contract lines above are
        # the whole of an ordinary dry run. `--completions` edits a startup
        # file, which is the write a reader of a dry run most wants named.
        if [ "$want_completions" = "yes" ]; then
            resolve_completion_shell
            if [ -n "$comp_shell" ]; then
                printf '  %-10s %s\n' "completions" \
                    "would be switched on for ${comp_shell} after the install"
            else
                printf '  %-10s %s\n' "completions" \
                    "asked for, but no shell tasqx completes was detected"
            fi
        fi
        return 0
    fi

    make_tmp || return
    fetch_and_verify || return
    install_binary || return
    report_path

    # Last, after the install is done and reported. The order is the reason
    # completions_step can afford to exit 0 on every failure.
    if [ "$want_completions" = "yes" ]; then
        completions_step
    fi
}

main "$@"

# watched-fail: unquoted expansion for shellcheck
watched_fail_probe() { local p=$1; echo $p; }
