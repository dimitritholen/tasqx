# README GIF: finishing a task leaves a conclusion, and the brief for the task
# it unblocked hands that conclusion over, with memory, in one read.
#
#   vhs scripts/brief.tape           # from the repo root → docs/img/brief.gif
#
# Same requirements as scripts/hero.tape. #51 in the demo store waits on #50;
# the tape writes #50's conclusion note while hidden, then records `done 50`
# and `brief 51` — the read an agent makes before starting (`tasqx_brief_task`
# over MCP is the same call).

Output docs/img/brief.gif

Set Shell bash
Set FontSize 16
Set Width 960
Set Height 980
Set Padding 16
Set Framerate 12
Set TypingSpeed 45ms
Set Theme "Catppuccin Mocha"

Source scripts/demo-prelude.tape
Type `tasqx annotate 50 "Shipped: /search allows 100 requests a minute per API key, with bursts of 20. Over the limit it answers 429 with Retry-After." >/dev/null &&`
Enter
Type `[ "$ready" ] && clear`
Enter
Sleep 2s
Show

Type "tasqx done 50"
Sleep 300ms
Enter
Sleep 2500ms

Type "tasqx brief 51 --card --memory-limit 2"
Sleep 300ms
Enter
Sleep 8s
