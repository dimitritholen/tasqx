# A self-improving agent: a retrospective after every completed task

Every completed task is a moment where the agent knows what worked and what did
not: the edit it reverted, the command it re-ran twice, the convention it found
out the hard way. By the next session all of it is gone.

The mechanism that keeps it is two pieces. A PostToolUse hook on
`tasqx_complete_task` injects one line into the agent's context: run a
retrospective skill for the task you just finished. The skill reviews the
session and writes what it learned back into tasqx, where `tasqx_search_memory`
finds it next time. Three things accumulate: retro annotations on the tasks,
memory docs for facts and conventions, and `skill candidate` tasks for
procedures.

The trigger is task completion, not the end of a turn. A Stop hook fires on
every reply, including questions and one-line fixes, so its nudge is wrong most
of the time and the agent learns to ignore it. `tasqx_complete_task` is the one
explicit "a unit of work finished" signal, and the agent sends it itself.

This guide assumes tasqx is already wired into Claude Code over MCP. If it is
not, start with [Driving tasqx from an AI agent](ai-agent-workflow.md).

## The hook

Save this as `~/.claude/hooks/retro-trigger.sh`:

```bash
#!/usr/bin/env bash
# PostToolUse hook: fires after mcp__tasqx__tasqx_complete_task succeeds and
# nudges Claude to run the retro skill for the task just completed.
# Claude only sees this hook's output through the JSON "additionalContext"
# field printed to stdout below; plain stdout text is not shown to Claude.
# Every failure path below exits 0 so this hook can never block a completion.
set -u

payload="$(cat)"

command -v jq >/dev/null 2>&1 || exit 0

# Only the main thread runs the retro: it holds the transcript and can fork.
# A subagent's payload carries agent_id; skip there.
printf '%s' "$payload" | jq -e '.agent_id | not' >/dev/null 2>&1 || exit 0

# Success means the response parses as an object with an `unblocked` array,
# whatever shape the harness hands it in (content-block array, string, or
# already-parsed object). An isError result, or an error that merely quotes
# the word, does not pass.
printf '%s' "$payload" | jq -e '
  (.tool_response? // null) as $r
  | (if ($r|type)=="object" and ($r.isError? == true) then null
     elif ($r|type)=="array"  then ($r|map(select((.type? // "")=="text")|.text?)|join("\n"))
     elif ($r|type)=="string" then $r
     elif ($r|type)=="object" then ($r|tojson)
     else null end) as $t
  | (($t // "")|fromjson?) as $o
  | (($o|type)=="object") and (($o.unblocked|type)=="array")
' >/dev/null 2>&1 || exit 0

ref=$(printf '%s' "$payload" | jq -r '.tool_input.ref | select(type=="string" or type=="number") | tostring' 2>/dev/null)

if [ -n "$ref" ]; then
  ref_desc="Task #$ref"
else
  ref_desc="A tasqx task"
fi

msg="$ref_desc was just completed. Before ending this turn, invoke the retro skill for it (Skill tool, skill name \"retro\"). If you are about to complete more tasks in this same turn, finish those completions first and run retro once, covering all of them."

jq -cn --arg msg "$msg" '{hookSpecificOutput:{hookEventName:"PostToolUse",additionalContext:$msg}}'
```

Make it executable:

```console
chmod +x ~/.claude/hooks/retro-trigger.sh
```

Then register it in `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "^mcp__tasqx__tasqx_complete_task$",
        "hooks": [
          {
            "type": "command",
            "command": "$HOME/.claude/hooks/retro-trigger.sh",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

If you adapt any of this, keep five points:

- **The JSON `additionalContext` field is the only channel to the model.** Plain
  stdout text goes to the log, not to the agent.
- **Success is decided by parsing the response.** Every successful completion
  carries an `unblocked` array, so that array is the signal; an error result
  stays silent, even one that quotes the word.
- **The matcher is anchored**, so no future tool whose name starts the same way
  is caught by it.
- **Subagents are skipped.** Their payload carries `agent_id`, and only the main
  thread holds the transcript the retro reads.
- **Every failure path exits 0**, so the hook can never turn a good completion
  into an error. Hooks also take effect on the next matching call, with nothing
  to restart.

## The skill

The skill answers five questions from the session transcript, routes each answer
to the place that will surface it again, and writes a summary annotation on the
completed task. It runs in a fork, so its reasoning stays out of the main
context and comes back as a sentence or two. Two rules keep it honest:

- **Every claim cites what happened** (the reverted edit, the re-run command) or
  it is left out. An agent asked what it learned invents a plausible lesson
  otherwise.
- **A procedure becomes a skill only on its second sighting.** A retro run
  minutes after a task overrates the novelty of what it just did, and one-off
  skills pile up unused. The first sighting files a candidate, the second
  promotes it.

Save this as `~/.claude/skills/retro/SKILL.md`:

```markdown
---
name: retro
description: Retrospective on a task just completed in tasqx — review the decisions and rework that got it done, record the lessons on the task, and decide whether the procedure earns a skill. Use when a PostToolUse hook says a task was completed, or when the user says retro, retrospective, post-mortem, lessons learned, or asks what we learned from a task.
---

This skill runs after `tasqx_complete_task` (a hook injects the trigger) or on
request. Its evidence is the transcript of this session. Its outputs are an
annotation on the completed task, and optionally a memory entry, a hook
recommendation, or a skill candidate. A skill is promoted only on the second
sighting of the same procedure — retros overrate their own novelty.

## Step 0: Delegate to a fork

Run the retro in a fork so its reasoning stays out of the main context: the
`Agent` tool with `subagent_type: "fork"` and the prompt "Run the retro skill
for task #N" (list every ref if several completed this turn). The fork inherits
the transcript, which is the evidence this skill needs. Relay its verdict in one
or two sentences. You are the fork when your prompt starts with "Run the retro
skill for task": run steps 1-5 and end your final message with the verdict.

## Step 1: Answer five questions from the transcript

Cite what actually happened for each answer — the file, the command, the
reverted edit, the turn where an assumption broke. Answer only the questions
with evidence to cite.

1. **Goal vs delivered**: what was asked, what shipped, the gap if any.
2. **Rework**: every edit reverted, command re-run, assumption broken. The only
   honest signal of a hard step.
3. **Discovery**: what was learned that is written nowhere.
4. **Redo**: which steps would be skipped, which reordered.
5. **Procedure**: is any part a repeatable sequence with a verifiable end.

## Step 2: Scope gate

If questions 2 and 3 produced no evidence, annotate the task with one line,
"retro: straight through, nothing to record", and stop.

## Step 3: Route each finding

- A fact, convention or gotcha from Q3: search `tasqx_search_memory` first with
  two or three keywords. If it is not there, `tasqx_add_memory` with a title, a
  body (the ruling first, the reasoning under it), and `source` naming the repo.
- A mistake worth preventing mechanically ("never run X against Y"): name the
  pattern to the user as a hook candidate. Hooks are the user's call to make.
- A procedure from Q5: run it through Step 4.

## Step 4: The skill-worthiness test

Both gates must pass. **Procedural**: a sequence of steps with a verifiable end
(a fact belongs in memory instead, a prohibition in a hook). **Non-obvious**: it
cost a wrong attempt or a discovery to get right, so a skill adds what general
knowledge does not.

Then search for a prior sighting: `tasqx_search_memory` with the procedure's key
terms, two or three words, in two different wordings. A hit whose `source` is
`task:#N` on a task titled `skill candidate: …` is a prior sighting.

- **A gate fails**: record which gate and why in the retro annotation, and stop.
- **No prior sighting**: `tasqx_add_task` titled `skill candidate: <short name>`,
  same project, priority L, tag `skill-candidate`, then `tasqx_annotate_task` on
  it with the procedure as numbered steps, the trigger phrase a user would say,
  and `sighting 1: task #N`.
- **Prior sighting, candidate open**: annotate it `sighting 2: task #N` plus
  whatever this run added, draft the SKILL.md from that annotation, record the
  new file's path on the candidate, then `tasqx_cancel_task` it.
- **Prior sighting, candidate cancelled with a path on it**: the skill exists.
  Record the path in the retro annotation and stop.

## Step 5: Write the retro annotation

`tasqx_annotate_task` on the completed task. The body starts with `retro:`,
followed by the answers that had evidence and the routing outcome — the memory
id, the hook recommendation, the candidate task ref, or the skill path. A fork
repeats any hook pattern verbatim in its verdict, since that is the only way the
recommendation reaches the user.
```

## What accumulates, and how to read it

| What | Where it lands | How to find it |
|---|---|---|
| What a task taught | A `retro:` annotation on that task | `tasqx_get_task`, or `tasqx show <id>` |
| Facts and conventions | Memory docs | `tasqx_search_memory`, or `tasqx memory search` |
| Procedures | Tasks titled `skill candidate: …`, tagged `skill-candidate` | `tasqx list +skill-candidate` |

The agent finds the first two by itself: both live in the store
`tasqx_search_memory` covers, so the next task on the same code reads last
time's annotation ([Memory](../wiki/Memory.md) explains the ranking). Candidates
are the part you look at. On a second sighting the skill drafts a real SKILL.md,
records the path on the candidate and cancels it; list the open ones now and
then, and promote one by hand when you already know it recurs.

## Try it

The hook reads its payload from stdin, so you can drive it without an agent. The
first line below prints one line of JSON with the nudge inside
`additionalContext`. The second is an error result: it prints nothing and exits 0,
which is what keeps a failed completion from being read as a success.

```console
echo '{"tool_input":{"ref":42},"tool_response":[{"type":"text","text":"{\"unblocked\":[43],\"status\":\"done\"}"}]}' | ~/.claude/hooks/retro-trigger.sh
echo '{"tool_input":{"ref":42},"tool_response":{"isError":true,"content":[{"type":"text","text":"no unblocked tasks"}]}}' | ~/.claude/hooks/retro-trigger.sh
```

Then complete any task from Claude Code and watch it fire on the real thing: the
agent finishes the work, calls `tasqx_complete_task`, and comes back with a
verdict instead of moving straight on.

## Other clients

The skill is client-agnostic: it needs nothing but the `tasqx_*` tools, so any
MCP client offering them runs the same five questions and the same routing. Only
the trigger is Claude Code specific, so in a client without hooks put the retro
instructions in the agent's system prompt, next to the block from
[Giving an agent memory in any client](agent-starter-prompt.md), and tell it to
run them after every `tasqx_complete_task`.
