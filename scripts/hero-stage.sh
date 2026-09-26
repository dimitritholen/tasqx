#!/usr/bin/env bash
# Stage one take of the README hero: a real Claude Code session, run by
# scripts/hero-receipt.tape inside a terminal that owns nothing of the
# recorder's.
#
#   scripts/hero-stage.sh            # from the repo root → target/hero/
#
# The agent's answer is never written here. Everything this script controls is
# the room the agent walks into, rebuilt identically for every take:
#
# - the demo store (scripts/demo-store.py under the capture pin), copied to
#   target/hero/take.db so a take's writes never reach target/demo/tasks.db,
#   which the other tapes and scripts/docs-capture.sh read;
# - `~/acme-sdk`, an invented repository holding the migration guide that task
#   #51 is about, committed under a fixed date so its hash is the same on
#   every take;
# - a Claude Code config directory holding nothing but a login, the
#   first-run answers and the permission mode the tape waits on, so no hook, plugin, memory or setting of the person
#   recording reaches the session, and the header shows `~/acme-sdk` rather
#   than a real path (HOME is target/hero/home inside the take);
# - an MCP config naming tasqx alone, pointed at take.db with --no-daemon, so
#   a running daemon cannot answer from the real store.
#
# The login is copied from $CLAUDE_CREDENTIALS (default
# ~/.claude/.credentials.json) into target/hero/cc, which is gitignored.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/target/hero
pin=2026-09-16T09:00:00Z
creds=${CLAUDE_CREDENTIALS:-$HOME/.claude/.credentials.json}
[ -r "$creds" ] || { echo "hero-stage: no Claude Code login at $creds" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/home/acme-sdk/docs" "$out/cc"

TASQX_NOW=$pin TZ=UTC python3 "$root/scripts/demo-store.py" >/dev/null
cp "$root/target/demo/tasks.db" "$out/take.db"

cat >"$out/home/acme-sdk/docs/migration-3.0.md" <<'EOF'
# Migrating from SDK 2.x to 3.0

## Renamed symbols

| 2.x                  | 3.0                    |
|----------------------|------------------------|
| `Client.fetchAll()`  | `Client.list()`        |
| `Client.fetchOne()`  | `Client.get()`         |
| `AuthToken.renew()`  | `Session.refresh()`    |
| `SearchQuery.run()`  | `Client.search()`      |

## Authentication

3.0 replaces long-lived API tokens with short sessions. Call
`Session.refresh()` before a session expires; the SDK does this for you
when `autoRefresh` is on (the default).

## Worked examples

- js: `examples/js/migrate.ts`
- py: `examples/py/migrate.py`
- go: `examples/go/migrate.go`

## Rate limits

`Client.search()` is rate-limited in 3.0. The ceiling is TBD.
EOF
(
  cd "$out/home/acme-sdk"
  export GIT_AUTHOR_NAME=demo GIT_AUTHOR_EMAIL=demo@example.dev
  export GIT_COMMITTER_NAME=demo GIT_COMMITTER_EMAIL=demo@example.dev
  export GIT_AUTHOR_DATE=$pin GIT_COMMITTER_DATE=$pin
  git init -q -b main && git add -A && git commit -qm "docs: SDK 3.0 migration guide"
)

install -m 600 "$creds" "$out/cc/.credentials.json"
cat >"$out/cc/.claude.json" <<EOF
{"hasCompletedOnboarding": true, "theme": "dark", "numStartups": 5,
 "projects": {"$out/home/acme-sdk": {"hasTrustDialogAccepted": true,
                                     "hasCompletedProjectOnboarding": true}}}
EOF

# The tape waits for the footer auto mode prints, so the mode is pinned here
# rather than left to whatever Claude Code defaults to.
cat >"$out/cc/settings.json" <<'EOF'
{"permissions": {"defaultMode": "auto"}}
EOF

cat >"$out/mcp.json" <<EOF
{"mcpServers": {"tasqx": {"command": "tasqx",
  "args": ["--no-daemon", "mcp", "serve", "--scope", "write"],
  "env": {"TASQX_DB": "$out/take.db", "TASQX_CONFIG_DIR": "$root/target/demo/config",
          "TASQX_NOW": "$pin", "TZ": "UTC"}}}}
EOF

# The session gets a PATH of plain directories: a WSL PATH carries Windows
# entries with spaces, and `env -i` keeps the recorder's variables out.
for bin in claude tasqx; do
  command -v "$bin" >/dev/null || { echo "hero-stage: $bin is not on PATH" >&2; exit 1; }
done
claude_dir=$(dirname "$(command -v claude)")
tasqx_dir=$(dirname "$(command -v tasqx)")
cat >"$out/launch.sh" <<EOF
#!/usr/bin/env bash
exec env -i PATH="$tasqx_dir:$claude_dir:/usr/local/bin:/usr/bin:/bin" \\
  TERM=xterm-256color COLORTERM=truecolor LANG=C.UTF-8 \\
  HOME="$out/home" CLAUDE_CONFIG_DIR="$out/cc" \\
  bash --norc -c 'cd ~/acme-sdk && exec claude --strict-mcp-config \\
    --mcp-config "$out/mcp.json" --allowedTools "mcp__tasqx__*"'
EOF
chmod +x "$out/launch.sh"
echo "$out"
