# The README's hero GIF: capture a task with everything on one line, make a
# second wait on it, then find the first again in `pick` and open its card.
#
#   vhs scripts/hero.tape            # from the repo root → docs/img/hero.gif
#
# Needs vhs 0.11.0 with ttyd and ffmpeg, and a `tasqx` on PATH built from this
# tree (`cargo install --path crates/tasqx-cli --force`). vhs 0.12.0 prints
# "Creating docs/img/hero.gif...", exits 0 and writes nothing
# (charmbracelet/vhs#787); take 0.11.0 from its GitHub release until that ships.
#
# It records only the invented demo store, through scripts/demo-prelude.tape,
# where the next free short id is #62. `pick` lists the working set, which a
# blocked task is not in, so the search finds #62 and its card names #63 under
# "blocks".

Output docs/img/hero.gif

Set Shell bash
Set FontSize 16
Set Width 1200
Set Height 560
Set Padding 16
Set Framerate 12
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"

Source scripts/demo-prelude.tape
Type `[ "$ready" ] && clear`
Enter
Sleep 2s
Show

Type 'tasqx add "Launch the new pricing page" project:website due:friday !H +launch est:3h'
Sleep 300ms
Enter
Sleep 1500ms

Type 'tasqx add "Announce the new pricing" project:website +launch'
Sleep 300ms
Enter
Sleep 1200ms

Type "tasqx dep 63 62"
Sleep 300ms
Enter
Sleep 1500ms

Type "tasqx pick"
Sleep 300ms
Enter
Sleep 2s
Type "/"
Sleep 400ms
Type@110ms "new pricing"
Sleep 900ms
# Enter keeps the filter, the second opens the card. The recording ends on
# the card, not on esc: a GIF loops, and its last frame is the one a reader
# sits on.
Enter
Sleep 800ms
Enter
Sleep 4500ms
