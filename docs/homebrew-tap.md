# The Homebrew tap

`brew install tasqx` is the one install route that can switch Tab completion on
without the user doing anything or reading anything (D57): brew owns directories
the shell already reads, and it knows the final path of the binary — which is the
part a release archive cannot know, because clap bakes `current_exe()` into the
registration script it generates.

**homebrew-core is not a route this can take.** It requires an OSI-approved
license and tasqx is FSL-1.1-MIT. That is a licensing consequence, not an
oversight, and it means a tap is the whole distribution story rather than a step
towards being in core.

## Creating it, once

A tap is a GitHub repository named `homebrew-<name>`, and nothing else:

```console
gh repo create dimitritholen/homebrew-tasqx --public --description "Homebrew tap for tasqx"
git clone git@github.com:dimitritholen/homebrew-tasqx.git
mkdir -p homebrew-tasqx/Formula
```

Users then need no tap command of their own — `brew install
dimitritholen/tasqx/tasqx` taps it on the way past.

## Per release

The formula holds a version and three checksums, so it is **generated from the
published release** rather than kept in this repository. A checked-in copy would
be right for exactly one tag and quietly wrong after that.

```console
scripts/brew-formula.sh v0.2.0 > ../homebrew-tasqx/Formula/tasqx.rb
cd ../homebrew-tasqx && git commit -am "tasqx 0.2.0" && git push
```

The sums come from the `.sha256` files the release workflow publishes beside each
archive, so they cannot disagree with what a user downloads. A tag with no
release fails the script instead of producing a formula with dead URLs.

## Checking it before anyone else does

```console
brew install --build-from-source ../homebrew-tasqx/Formula/tasqx.rb
brew test tasqx
brew audit --strict --formula ../homebrew-tasqx/Formula/tasqx.rb
```

`brew test` is where the claim this formula exists to make gets checked: it reads
the installed `_tasqx` back and asserts the path inside it is the binary brew
installed. That is the one thing no test before install time can see, and the one
thing that being wrong would make the completion files useless while looking
perfectly well-formed.
