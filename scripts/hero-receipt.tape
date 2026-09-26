# README hero, raw take: one real Claude Code session asked to close task #51
# of the demo store. Nothing it answers is scripted; retakes are fine, edits are
# not. scripts/hero-assemble.sh cuts this into docs/img/hero-receipt.gif.
#
#   scripts/hero-stage.sh && vhs scripts/hero-receipt.tape   # → target/hero/take.mp4
#
# Needs vhs 0.11.0 (see docs/maintainers/terminal-style.md), a Claude Code
# login, and the installed tasqx built from this tree.

Output target/hero/take.mp4

Set Shell bash
Set FontFamily "DejaVu Sans Mono"
Set FontSize 18
Set Width 1060
Set Height 700
Set Padding 18
Set Framerate 12
Set TypingSpeed 40ms
Set Theme "Catppuccin Mocha"
Set WaitTimeout 120s

# Claude Code shows a rotating startup notice for its first few launches in a
# fresh config directory; three hidden launches spend them, so the take opens
# on the header and an empty prompt.
Hide
Type "for i in 1 2 3; do target/hero/launch.sh; done"
Enter
Wait+Screen /auto mode on/
Sleep 3s
Type "/exit"
Enter
Wait+Screen /auto mode on/
Sleep 3s
Type "/exit"
Enter
Wait+Screen /auto mode on/
Sleep 3s
Type "/exit"
Enter
Sleep 2s
Type "clear; target/hero/launch.sh"
Enter
Wait+Screen /auto mode on/
Sleep 3s
Show

Sleep 800ms
Type "The SDK 3.0 migration guide is written. Close its task."
Sleep 600ms
Enter
Wait+Screen /Esc to cancel|Worked for/
Sleep 6s
