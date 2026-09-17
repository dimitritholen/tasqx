# README GIF: import a folder of decisions, then find one again by two words.
#
#   vhs scripts/memory.tape          # from the repo root → docs/img/memory.gif
#
# Same requirements as scripts/hero.tape. The docs are invented, committed in
# scripts/demo-decisions/, and copied into target/demo/decisions/ while the
# terminal is hidden, so the path on screen is the short one a user would type.

Output docs/img/memory.gif

Set Shell bash
Set FontSize 18
Set Width 1000
Set Height 290
Set Padding 16
Set Framerate 12
Set TypingSpeed 45ms
Set Theme "Catppuccin Mocha"

Source scripts/demo-prelude.tape
Type `cd target/demo && rm -rf decisions && cp -R ../../scripts/demo-decisions decisions &&`
Enter
Type `[ "$ready" ] && clear`
Enter
Sleep 2s
Show

Type "ls decisions/"
Sleep 300ms
Enter
Sleep 1500ms

Type "tasqx memory import decisions/"
Sleep 300ms
Enter
Sleep 2s

Type "tasqx memory search failed charge"
Sleep 300ms
Enter
Sleep 4s
