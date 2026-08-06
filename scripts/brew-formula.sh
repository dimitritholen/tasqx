#!/usr/bin/env bash
#
# Render the Homebrew formula for a released tag (D57).
#
# THE FORMULA IS NOT CHECKED IN, and that is the point. A formula holds a
# version and three SHA-256 sums; a copy in this repository would be correct for
# exactly one release and silently wrong for every one after it — the drift shape
# D30 rules against, in a file nothing in CI can check. So the formula is
# generated from the release itself: the sums come from the `.sha256` files the
# release workflow already publishes beside each archive, and a run against a tag
# that does not exist fails instead of inventing one.
#
#   scripts/brew-formula.sh v0.2.0 > ../homebrew-tasqx/Formula/tasqx.rb
#
# Needs `gh` and an authenticated session, because it reads the release's assets.
set -euo pipefail

TAG="${1:-}"
if [ -z "$TAG" ]; then
    echo "usage: $0 <tag>   e.g. $0 v0.2.0" >&2
    exit 2
fi
VERSION="${TAG#v}"
REPO="dimitritholen/tasqx"
BASE="https://github.com/${REPO}/releases/download/${TAG}"

# The three targets Homebrew can serve. The release builds a fourth
# (x86_64-pc-windows-msvc); brew has nowhere to put it.
ARM_MAC="tasqx-${TAG}-aarch64-apple-darwin.tar.gz"
INTEL_MAC="tasqx-${TAG}-x86_64-apple-darwin.tar.gz"
LINUX="tasqx-${TAG}-x86_64-unknown-linux-gnu.tar.gz"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# `gh release download` refuses a tag that is not published, which is the check
# this script wants: a formula for an unreleased version is a broken URL either
# way, and finding that out here beats finding it out in somebody's `brew
# install`.
gh release download "$TAG" --repo "$REPO" --dir "$work" \
    --pattern "*.tar.gz.sha256"

# The published `.sha256` files are `<sum>  <filename>` — the shasum format, so
# the sum is the first field.
sum_for() {
    local file="$1"
    if [ ! -f "${work}/${file}.sha256" ]; then
        echo "no checksum published for ${file} in ${TAG}" >&2
        exit 1
    fi
    awk '{print $1; exit}' "${work}/${file}.sha256"
}

cat <<EOF
class Tasqx < Formula
  desc "Task manager that lives in the terminal and treats an AI agent as a normal user"
  homepage "https://github.com/${REPO}"
  # No \`version\`: Homebrew scans it out of the URL, and \`brew audit\` rejects
  # the field as redundant — the first check a tap maintainer runs, failing on
  # the formula this script exists to produce. Verified rather than assumed:
  # with the line gone, \`brew info --json\` still reports ${VERSION}.
  #
  # Not an OSI-approved license, so homebrew-core is not a route this can ever
  # take. A tap is the whole distribution story, and that is a licensing
  # consequence rather than an omission.
  license "FSL-1.1-MIT"

  on_macos do
    on_arm do
      url "${BASE}/${ARM_MAC}"
      sha256 "$(sum_for "$ARM_MAC")"
    end
    on_intel do
      url "${BASE}/${INTEL_MAC}"
      sha256 "$(sum_for "$INTEL_MAC")"
    end
  end

  on_linux do
    on_intel do
      url "${BASE}/${LINUX}"
      sha256 "$(sum_for "$LINUX")"
    end
  end

  def install
    bin.install "tasqx"

    # Generated HERE, by the binary brew has just installed, and deliberately
    # NOT copied out of the archive's own \`completions/\` directory.
    #
    # The two are different artifacts. clap bakes \`current_exe()\` into the
    # registration script, so a copy made on a CI runner names a path that
    # exists on no other machine; the archive therefore ships the ACTIVATION
    # LINE, which invokes \`tasqx\` off \$PATH and survives being moved. A package
    # manager can do better than a line for a human to paste, because it knows
    # the final path and owns a directory the shell already reads — which is the
    # whole reason this route exists: \`brew install tasqx\` and Tab works, with
    # nobody having read anything.
    #
    # The baked path is this version's Cellar path. An upgrade installs a new
    # version and runs this again, so the file and the binary move together.
    completions = buildpath/"generated-completions"
    completions.mkpath
    { "bash" => "tasqx", "zsh" => "_tasqx", "fish" => "tasqx.fish" }.each do |shell, name|
      (completions/name).write(
        with_env("TASQX_COMPLETE" => shell) { Utils.safe_popen_read(bin/"tasqx") },
      )
    end
    bash_completion.install completions/"tasqx"
    zsh_completion.install completions/"_tasqx"
    fish_completion.install completions/"tasqx.fish"
  end

  test do
    # A store of its own: the test must not find, or create, the tester's real
    # one. \`ENV\` takes strings, and a Pathname here is a TypeError at test time.
    ENV["TASQX_DB"] = (testpath/"tasks.db").to_s
    system bin/"tasqx", "init", "work"
    assert_match "Buy milk", shell_output("#{bin}/tasqx add Buy milk")

    # The completion files are the reason this formula exists, so the test
    # asserts they were generated rather than merely installed empty — and that
    # the path inside them is the one brew installed, which is the single thing
    # that cannot be checked before install time.
    registration = (share/"zsh/site-functions/_tasqx").read
    installed = (bin/"tasqx").to_s
    assert_match "#compdef tasqx", registration
    assert_match installed, registration
  end
end
EOF
