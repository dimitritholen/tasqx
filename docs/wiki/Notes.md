# Notes

A note is for the thing you want to write down before it gets away: an idea,
a link to read later, a question for tomorrow. It is not a task. It has no
due date and no urgency, it stays out of `tasqx list`, `next`, the dashboard's
panels and the reports, and memory search never sees it. Write it in a second and
forget it safely.

When a note turns out to matter, promote it: it becomes a task, or a memory
doc, and the note is archived with a pointer to what it became.

## tasqx note

Jot one down. Every word after `note` is the note, kept exactly as you typed
it.

```console
tasqx note try the new tracing exporter
tasqx note https://www.rfc-editor.org/rfc/rfc9457 --project api
```

- `--project` files it under a project. Without it the note has none. There
  is no `project:` shortcut as `add` has: a note's words are its own.
- Quote the text when it holds characters your shell reads (`#`, `*`, `?`,
  `&`).
- Notes have their own numbers. Note 3 and task 3 are different things.
- Put `--` before text that is a number or starts with `-`, so it is not
  read as a note's number or a flag: `tasqx note -- -5 degrees tonight`.

Read one whole, with its project and the day you added it:

```console
tasqx note 3
```

Change one by naming it first and saying what to do after:

```console
tasqx note 3 edit try the OTLP exporter instead
tasqx note 3 archive
tasqx note 3 unarchive
tasqx note 3 rm
```

- `edit` replaces the text.
- `archive` puts a note out of sight; it stays under `tasqx notes --archived`.
  `unarchive` brings it back.
- `rm` deletes it for good. `tasqx undo` does not cover notes, so archive
  when you are not sure.

The number always comes first. Any other first word is part of the note, so
`tasqx note edit the readme` writes down "edit the readme". A subverb on its
own, or followed by a number (`tasqx note archive 7`), reads as a command typed
the wrong way round: it is refused (exit 2) with the right spelling, and
`tasqx note -- archive 7` stores it as text.

`note` used to be another name for `annotate`. A number followed by text is
now refused (exit 2) and names the command you probably meant:

```console
tasqx annotate 42 "called the plumber, waiting on a quote"
```

## tasqx notes

List your notes, newest first.

<!-- fixture: notes -->
```console
$ tasqx notes
2 notes

ID  NOTE                                                                          PROJECT  ADDED
2   idea: a dark-mode toggle in the docs header                                            yesterday
1   read https://www.rfc-editor.org/rfc/rfc9457 before the error-format chan…  ↗  api      2d ago
```

Each line has the note's number, its first line, `↗` (`L` without Unicode)
when it holds a link, its project and the day you added it. A note that is
only a link is never cut short, so you can still copy it; its line runs past
the edge of the terminal instead.

- `--links` shows only notes holding an `http://` or `https://` link.
- `--archived` shows the archived ones, promoted notes among them. `--all`
  shows open and archived together.
- `--search <text>` keeps notes containing that text, ignoring case. It is a
  plain match, not the ranked search `memory search` does.
- `--project <name>` keeps the notes filed under that project.

```console
tasqx notes --links
tasqx notes --search exporter --all
```

### The notes browser

On a terminal, `tasqx notes` opens a full-screen browser over the same
notes, and the flags above decide where it opens: `--search` becomes the
search you can edit, `--archived` and `--all` choose which notes it holds.
Piped, or with `--json`, it prints the table.

| Key | What it does |
|---|---|
| `j` / `k` | move down / up (the arrows too) |
| `g` / `G` | first / last note |
| `/` | search the notes' text, every word you type |
| Enter | read the note whole; a link is shown in full and never opened |
| Esc | clear the search; with none, leave (in a note, back to the list) |
| `q` | leave (in a note, back to the list) |
| `a` | archive the note, or bring an archived one back |
| `t` | promote it to a task |
| `m` | promote it to a memory doc |
| `x` | remove it for good, after you answer `y` |
| `l` | show only the notes holding a link, or all of them again |

Each change says what it did on the line above the keys, and a refusal is
shown there too: a note someone changed after the browser read it is
refused rather than overwritten, and the list is read again. A promoted note
does not offer `t` and `m`. From the [dashboard](Dashboard-and-Live-View.md),
`N` opens the same browser, and closing it takes you back.

## Promote a note

When a note is worth doing, make it a task:

```console
tasqx note 3 promote task
tasqx note 3 promote task --title "Evaluate the OTLP exporter"
```

The task lands in the note's project, or your default project when it has
none. Its title is `--title`, or the note's first line cut at 80 characters;
when that title does not say everything, the whole note becomes the task's
first annotation. The echo is the same card `tasqx add` prints.

When it is something to remember rather than something to do, make it a
memory doc:

```console
tasqx note 3 promote memory --title "Tracing exporters we tried"
```

Either way the note is archived and remembers what it became. That happens in
one step: if the task or doc cannot be made, the note is left as it was.
Promotion is final, so a promoted note cannot be promoted again or
unarchived. Cancelling the task later does not bring the note back; to capture
the idea again, write a new note.

## Notes, tasks, memory and triage

| Question | Note | Task | Triage proposal | Memory doc |
|---|---|---|---|---|
| For | a thought you might act on | work you will do | work someone suggests | knowledge to keep |
| Urgency, `@working`, reports | no | yes | not until accepted | no |
| Found by `memory search` | no | its annotations | its annotations | yes |
| Becomes something else | `promote` | — | `accept` | — |

Use a note when you are not yet sure. A task asks to be done; a proposal in
[triage](Working-on-Tasks.md#tasqx-triage) is a task waiting for your yes; a
[memory](Memory.md) doc is something an agent should read before it works. A
note asks for nothing.

Every command here takes `--json` and prints the API's answer. Agents reach
notes through the MCP tools `tasqx_add_note`, `tasqx_list_notes`,
`tasqx_get_note` (one note whole), `tasqx_update_note`, `tasqx_remove_note` and `tasqx_promote_note`.
