# A configuration surface — design

**Date:** 2026-07-18
**Status:** draft, pending approval
**Scope:** phase B of a three-part split (A: report status filtering, shipped as D24 · **B: a settings registry, one precedence resolver, and `tasqx config`** · C: an interactive settings TUI). C is out of scope and depends on this.

Every factual claim below was verified against the tree at `bb1a139`, not recalled. Where a claim was checked by running something rather than by reading, it says so.

## Problem

You cannot change a single tasqx setting without hand-editing a TOML file. There is no `tasqx config`, and no `tasqx theme set` — `tasqx theme list` marks the active theme with `← active`, which is a *read* of the resolved chain, and offers no way to change it. The HTML guide (`docs.rs:1571`) tells the reader to write the file themselves.

That is the visible half. The structural half is worse.

### Config today, verified

**`config.toml` is read by four hand-written functions and holds exactly two keys.** `config_table()` (`lib.rs:650`) resolves a base dir from `$TASQX_CONFIG_DIR`, else `directories::ProjectDirs::from("dev","tasqx","tasqx").config_dir()`, and parses `config.toml`. Every failure — missing dir, missing file, malformed TOML — degrades silently to `None`. On top of it sit `config_theme_name()` (`lib.rs:664`, reads `[theme] name`) and `config_notify_enabled()` (`lib.rs:677`, reads `[notify] enabled`, absent ⇒ `false`). There is no struct, no `Deserialize`, no validation, and no warning for an unknown key.

**Nothing writes it.** The only two `fs::write` calls in the workspace are `lib.rs:1207` (the HTML report) and `lib.rs:1266` (the docs page).

**`tasqx-core` cannot read it.** `crates/tasqx-core/Cargo.toml` has no `toml` dependency (verified: zero matches). So any setting the daemon, the MCP server, or the engine must honour either lives in the store or gets plumbed in at construction.

**The store has a second config home.** `storage.rs:72` creates `config (key TEXT PRIMARY KEY, value TEXT NOT NULL)`, with `get_config` (`storage.rs:287`, takes `&Connection`), `set_config` (`:295`, takes `&Transaction`), `clear_config` (`:308`). Exactly one key is ever written: `DEFAULT_PROJECT_KEY`, from three call sites in `engine.rs` (`:188` create, `:258` use, `:1212` archive-clears).

**There is no `config.*` API method.** The dispatch table has 23 methods and none of them read or write settings generically.

### D9 is only one-third implemented

D9 mandates precedence low→high: **built-in defaults → `config.toml` → `TASQX_*` env → CLI flags**, plus wholesale `TASQX_DATA_DIR` / `TASQX_CONFIG_DIR` overrides.

Verified against the code, the actual state per setting:

| Setting | flag | env | config.toml | default | full D9 chain? |
| --- | --- | --- | --- | --- | --- |
| theme | `--theme` | `TASQX_THEME` | `[theme] name` | `nord` | **yes** |
| notify enabled | — | — | `[notify] enabled` | `false` | no |
| socket | `--socket` | `TASQX_SOCK` | — | platform default | no |
| db path | — | `TASQX_DB` | — | platform data dir | no |

`theme::resolve_name` (`theme.rs:667`) is the only place the full chain exists, and it is a clean four-level fold. Every other setting re-implements a shorter chain by hand at its own call site: `resolve_socket` (`lib.rs:740`), `db_path` (`lib.rs:1701`).

`TASQX_DATA_DIR` **does not exist.** At the time of this design, the code also read `TASQX_MCP_TOKEN`; D7 later removed that misleading token surface in favor of an explicit stdio `--scope`. The current set is `TASQX_DB`, `TASQX_SOCK`, `TASQX_CONFIG_DIR`, `TASQX_THEME`, `TASQX_FORCE_COLOR`. D9's promise of a wholesale data-dir override is unbuilt, and nothing notices.

So there is no generic "resolve setting X across four layers" machinery. That is the main thing this phase has to invent, and it is the same shape as two pieces this repo already has: `cmddoc::COMMAND_REF` (one registry, three rendered surfaces, drift-guarded) and `Status::ALL` (one enum, every set derived from it).

## Decision

### 1. A settings registry

One `SETTINGS` table, modelled on `COMMAND_REF`. Each entry declares:

- `key` — the dotted name a user types (`theme.name`, `notify.enabled`)
- `home` — where the value lives (see §2)
- `kind` — `Str` / `Bool`, with parsing and a rejection message
- `default` — the built-in
- `env` — the `TASQX_*` variable that overrides the file, if any
- `flag` — the CLI flag that overrides everything, if any
- `summary` — one line, for `tasqx config list` and the HTML guide

This is the source of truth. The four hand-plucked reader functions collapse into lookups against it.

### 2. Two homes, declared per key

tasqx has two config stores with genuinely different semantics, and D21 put `default_project` in the store **deliberately**:

> The default stays in the store's `config` table — there is deliberately no `[core] default_project` key in config.toml.

D21's reasoning holds: `default_project` names a row in *this store's* `projects` table, is validated against it, and is meaningless against a different `TASQX_DB`. A second home would buy a precedence rule and a class of bug where config names a project the store has never heard of.

So the registry annotates each key with its home:

| Home | Semantics | Keys today |
| --- | --- | --- |
| `Toml` | per machine, best-effort, unvalidated | `theme.name`, `notify.enabled` |
| `Store` | per store, validated, transactional, written to the event log | `default_project` |

**`tasqx config list` shows both, with a source column.** A user asking "what are my settings" reasonably expects the default project in that list, and omitting it would be a lie by omission.

**`tasqx config set` writes only `Toml` keys.** For a `Store` key it exits `bad_request` naming the verb that owns it: `default_project is set by \`tasqx use <project>\`, which validates the name against this store`. That keeps D21's "one fact, one home" intact while the read surface stays complete.

This rule is the one part of the design that is genuinely arguable, and it is recorded as **D25** rather than treated as an implementation detail.

### 3. One resolver

```rust
/// Resolve a setting across the D9 chain: flag > env > config.toml > default.
fn resolve(key: &Setting, flag: Option<&str>) -> Value
```

`theme::resolve_name`'s existing fold is the correct behaviour and becomes the generic implementation, including its `pick()` rule that a whitespace-only value at any level is treated as absent. `resolve_name` is then either deleted or reduced to a call into the resolver, so there is one chain in the codebase rather than four.

**Deliberately not changed in this phase:** `socket` and `db path` keep their current chains. Giving them a config level is a behaviour change (a config file that previously did nothing would start winning over a platform default), and it belongs in its own decision. The registry records that they are env-only so the gap is visible rather than implied.

### 4. `tasqx config`

```
tasqx config list                 # every setting: key, value, source, default
tasqx config get <key>            # the resolved value
tasqx config set <key> <value>    # writes config.toml
tasqx config unset <key>          # removes the key, falling back to the default
tasqx config path                 # prints the config.toml path (it may not exist yet)
```

`list` names the *source* per row (`default` / `config.toml` / `$TASQX_THEME` / `--theme`), because the question a user actually has when a setting surprises them is "which layer won".

`tasqx theme set <name>` becomes an alias for `tasqx config set theme.name <name>`, with the same validation `theme list` already does — an unknown theme name is rejected rather than silently written.

## The writing problem

**Verified by running it:** parsing a `config.toml` into a `toml::Table` and serializing it back destroys comments *and* reorders sections. Given this input:

```toml
# my notes about the theme
[theme]
name = "gruvbox"  # inline note

[notify]
enabled = true
```

`toml::to_string_pretty(&parsed)` emits:

```toml
[notify]
enabled = true

[theme]
name = "gruvbox"
```

Both comments gone, sections alphabetised. For a file whose entire premise is that a human hand-edits it, that is a data-loss bug, not a cosmetic one.

`toml_edit` is the format-preserving parser for exactly this case. **It is not currently a dependency, direct or transitive** — `toml 1.1.3` pulls `toml_datetime`, `toml_parser` and `toml_writer`, not `toml_edit` (verified with `cargo tree`). Adding it is a real decision for a crate that carries seven dependencies today.

The alternatives are worse: rewriting the file wholesale and warning the user that their comments are gone, or surgical line-level edits, which is a hand-rolled TOML parser wearing a disguise.

**Recommendation: add `toml_edit`.** It is maintained by the same authors as `toml`, it is what `cargo` itself uses to edit `Cargo.toml`, and the alternative is knowingly destroying user data.

Writes must also be **atomic**: write to a temp file in the same directory, then rename. A crash mid-write on the current design would leave the user with no config at all, and the reader degrades silently to `None`, so they would not even get an error — just their theme quietly reverting.

## Error handling

`config_table()` swallowing malformed TOML is defensible for a reader that must never block a task capture. It is indefensible for `tasqx config`, where the user is explicitly asking about the file.

- The four resolver paths keep degrading silently (unchanged behaviour).
- `tasqx config list/get/set` **reports** a malformed file with the parse error and its line, and exits non-zero. `set` refuses to write over a file it could not parse, rather than replacing it with a valid file that has lost content.
- An unknown key on `get`/`set` is `bad_request` listing the valid keys. This is the one place the registry pays for itself immediately: today an unknown key in `config.toml` is silently ignored forever.

## Drift guards

Following the repo's existing pattern, where documentation rot is a build failure:

- Every `TASQX_*` variable the code reads appears in `SETTINGS`. A grep-based guard over the source, in the spirit of `every_clap_flag_is_documented_in_its_verbs_usage`, which found eleven real gaps when it was written.
- Every `SETTINGS` entry appears in the HTML guide's config section.
- Every `Toml`-home key round-trips: `set` then `get` returns what was set, over a real temp file.
- Setting a `Store`-home key is rejected and names the owning verb.
- `theme.name` resolution matches `theme::resolve_name`'s existing four precedence tests, which must keep passing unmodified — that is the proof the generic resolver did not change behaviour.

## Out of scope

- The interactive TUI (phase C). This is its data layer.
- Giving `socket` or `db path` a config level.
- Implementing `TASQX_DATA_DIR`. It is a real D9 gap and worth its own change; conflating it with this one would hide a behaviour change inside a refactor.
- Making D24's report default configurable. It was explicitly rejected in D24 on the grounds that a preference with no way to set it is not a feature — which this phase now makes false, so it becomes reconsiderable *afterwards*, not here.
- Any change to `themes/*.toml` or the plugin config D9 also describes.

## Open question for the reviewer

Whether `tasqx config set default_project X` should be rejected (this design) or should transparently route to `project.use` for the caller's convenience. Routing is friendlier, but it means one command writes to two stores with different guarantees — one transactional and evented, one a best-effort file — and the failure modes differ in ways the output would have to explain anyway.
