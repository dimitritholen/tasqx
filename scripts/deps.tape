# README GIF: finishing a task names what it unblocked, and `next` moves on.
#
#   vhs scripts/deps.tape            # from the repo root → docs/img/deps.gif
#
# Same requirements as scripts/hero.tape. The chain is built hidden, so the
# ten seconds go to the part that sells it: the blocked rows, `done` naming
# the task it freed, and `next` handing that task over.

Output docs/img/deps.gif

Set Shell bash
Set FontSize 18
Set Width 900
Set Height 440
Set Padding 16
Set Framerate 12
Set TypingSpeed 45ms
Set Theme "Catppuccin Mocha"

Source scripts/demo-prelude.tape
Type "tasqx init release >/dev/null"
Enter
Type 'tasqx add "Freeze the v2 API" project:release !H >/dev/null'
Enter
Type 'tasqx add "Tag v2.0.0" project:release !H >/dev/null'
Enter
Type 'tasqx add "Announce the release" project:release !M >/dev/null'
Enter
Type "tasqx dep 63 62 >/dev/null; tasqx dep 64 63 >/dev/null; clear"
Enter
Sleep 2s
Show

Type "tasqx list project:release"
Sleep 300ms
Enter
Sleep 2500ms

Type "tasqx done 62"
Sleep 300ms
Enter
Sleep 2500ms

Type "tasqx next project:release"
Sleep 300ms
Enter
Sleep 3s
