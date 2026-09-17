# README GIF: `report --outcomes` says whether the work worked, each rate
# beside the count it rests on.
#
#   vhs scripts/outcomes.tape        # from the repo root → docs/img/outcomes.gif
#
# Same requirements as scripts/hero.tape. The numbers are the demo store's
# (scripts/demo-store.py, `outcomes()`), so a change there is a re-record.

Output docs/img/outcomes.gif

Set Shell bash
Set FontSize 16
Set Width 1080
Set Height 300
Set Padding 16
Set Framerate 12
Set TypingSpeed 45ms
Set Theme "Catppuccin Mocha"

Source scripts/demo-prelude.tape
Type `[ "$ready" ] && clear`
Enter
Sleep 2s
Show

Type "tasqx report --outcomes"
Sleep 300ms
Enter
Sleep 8s
