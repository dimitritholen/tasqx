# The hidden start of every README GIF tape: `Source scripts/demo-prelude.tape`
# after the tape's Set lines, then any setup of its own, then `clear` and Show.
#
# It records only the invented demo store: it rebuilds target/demo/tasks.db
# under the same pinned clock as scripts/docs-capture.sh, points every variable
# tasqx reads at it, and wraps `tasqx` so each call passes --no-daemon — a
# reachable daemon would answer from the real store instead. `set +H` keeps
# bash from reading `!H` as history expansion; the terminal is left hidden.

Hide
Type "export TASQX_DB=$PWD/target/demo/tasks.db TASQX_CONFIG_DIR=$PWD/target/demo/config TASQX_NOW=2026-09-16T09:00:00Z TZ=UTC COLORTERM=truecolor"
Enter
Type "unset NO_COLOR CLICOLOR CLICOLOR_FORCE TASQX_THEME TASQX_SOCK TASQX_DASHBOARD"
Enter
Type "python3 scripts/demo-store.py >/dev/null"
Enter
Type `tasqx() { command tasqx --no-daemon "$@"; }`
Enter
Type "set +H; PS1='$ '"
Enter
