//! `tasqx docs` — the English user guide, as ONE self-contained HTML file.
//!
//! Same idiom as `html.rs` (the `report --html` generator): one document with an
//! inline `<style>`, an inline `<script>`, a system-font stack, and zero external
//! requests — no CDN, no web fonts, no images, no server. Everything the browser
//! needs is in the file, so it opens off a temp path, a USB stick, or an air-gapped
//! box identically. Light/dark via `prefers-color-scheme` over the same CSS custom
//! properties. Every string that could contain markup goes through [`html::esc`] —
//! the one escaper both surfaces share.
//!
//! **Multi-page without a server.** The guide is eleven pages, each a `<section>`
//! with a stable `id`. The inline script shows one at a time and drives
//! `location.hash`, so every page is cross-linkable (`docs.html#filters`) and the
//! back button works. With JavaScript off, the `:target` CSS fallback still selects
//! a page and, absent any hash, the page simply renders as one long scrollable
//! document — the content is never *hidden behind* the script.
//!
//! **The doc-drift guard lives here, not in prose.** [`VERBS`], [`METHODS`], and
//! [`DOCUMENTED_CLEAR_FIELDS`] are the lists the page renders *from*, and the
//! tests at the bottom of this file assert each one equals the real surface it
//! claims to describe — clap's subcommand table, the core's `core.capabilities`,
//! and `main::CLEARABLE`. Adding a verb without documenting it fails the build,
//! which is the cheapest honest guard available: the docs cannot silently fall
//! behind the CLI, because the CLI's own table is the assertion.
//!
//! Beyond the *names*, the guards now reach some of the descriptive columns too:
//! each verb's one-line description is rendered from [`crate::cmddoc`] rather
//! than restated here (so `-h` and this guide cannot disagree), and every
//! method's documented **required** parameters are checked against the engine's
//! own "missing required field" complaint. Two honest gaps remain, both named at
//! their tests: *optional* parameter names are unguarded (the engine ignores
//! unknown keys, so there is no failure to observe), and only the six
//! bare-callable methods have their return shapes checked.
//!
//! Every command and every block of output on this page was executed against the
//! real binary on an isolated store; nothing here is illustrative.

use crate::html::esc;

/// The verb table the Commands page renders: `(verb, aliases, method)`.
///
/// This is the single source — the page is generated from it and
/// [`documented_verbs`] reads it, so there is no parallel list to fall out of
/// sync. Order is reading order, not clap's.
///
/// **There is deliberately no "what it does" column.** There used to be, and it
/// was a second hand-written prose description of every verb sitting next to
/// [`crate::cmddoc::CmdDoc::summary`] — the one `tasqx <verb> -h` prints — with
/// nothing comparing them. They had already diverged in wording on most rows
/// (`init` read "Create a project." here and "Create a project — just a name, no
/// folder." under `-h`), and nothing could ever have caught a divergence in
/// *meaning*: the guide and the terminal simply describing the same verb
/// differently is not a state any test can distinguish from correct.
///
/// A guard comparing the two would have had to accept "merely consistent",
/// which is unassertable prose-equivalence. So the column is gone and the page
/// renders [`crate::cmddoc`]'s summary instead. One string per verb, used by
/// both surfaces, with no second copy left to drift.
const VERBS: [(&str, &str, &str); 29] = [
    ("init", "—", "project.create"),
    ("use", "—", "project.use"),
    ("add", "<code>a</code>, <code>new</code>", "task.add"),
    ("modify", "<code>mod</code>, <code>m</code>, <code>edit</code>", "task.modify"),
    ("list", "<code>ls</code>, <code>l</code>", "task.list"),
    ("next", "—", "task.list"),
    ("show", "<code>get</code>", "task.get"),
    ("why", "—", "task.get"),
    ("start", "<code>s</code>", "task.start"),
    ("stop", "<code>st</code>", "task.stop"),
    ("done", "<code>d</code>, <code>x</code>, <code>complete</code>", "task.done"),
    ("cancel", "<code>delete</code>, <code>del</code>, <code>rm</code>", "task.cancel"),
    ("reopen", "—", "task.reopen"),
    ("annotate", "<code>note</code>", "annotation.add"),
    ("dep", "—", "dependency.add"),
    ("undep", "—", "dependency.remove"),
    ("projects", "—", "project.list"),
    ("report", "—", "report.summary"),
    ("chart", "—", "event.list"),
    ("theme", "—", "— (no store)"),
    ("config", "—", "— (registry + core.capabilities)"),
    ("export", "—", "store.export"),
    ("import", "—", "store.import"),
    ("api", "—", "(any)"),
    ("daemon", "—", "(serves all)"),
    ("watch", "—", "task.list + push"),
    ("mcp", "—", "(subset)"),
    ("docs", "—", "— (no store)"),
    ("manual", "<code>man</code>", "— (no store)"),
];

/// The method table the JSON API page renders: `(method, params, returns)`.
/// Single source, same reason as [`VERBS`].
const METHODS: [(&str, &str, &str); 23] = [
    (
        "project.create",
        "<code>name</code>, <code>description?</code>",
        "The project, plus <code>default</code> (did it claim the default?) and \
         <code>current_default</code>.",
    ),
    (
        "project.list",
        "<code>include_archived?</code>",
        "<code>{projects}</code>; each row carries <code>default</code>.",
    ),
    (
        "project.use",
        "<code>name</code>",
        "<code>{name, default, previous}</code>. Sets the default project.",
    ),
    (
        "project.archive",
        "<code>name</code>",
        "The archived project, plus <code>default_cleared</code>.",
    ),
    (
        "task.add",
        "<code>title</code>, <code>project?</code>, <code>priority?</code>, <code>due?</code>, \
         <code>scheduled?</code>, <code>wait?</code>, <code>recurrence?</code>, \
         <code>remind?</code>, <code>estimate?</code>, <code>tags?</code>",
        "The new task, incl. the <code>project</code> it landed in (the default, if none given).",
    ),
    (
        "task.list",
        "<code>filter?</code>, <code>sort?</code>, <code>limit?</code>, <code>fields?</code>",
        "<code>{count, tasks}</code>. An omitted <code>filter</code> matches everything.",
    ),
    ("task.get", "<code>ref</code>", "Full detail incl. annotations, deps, <code>blocked</code>."),
    ("task.start", "<code>ref</code>, <code>keep?</code>", "The task, timer running."),
    ("task.stop", "<code>ref</code>", "The task, with tracked time."),
    ("task.done", "<code>ref</code>", "The task; plus the spawned next instance if recurring."),
    (
        "task.modify",
        "<code>ref</code>, <code>set</code>, <code>expected_rev?</code>",
        "The task. <code>null</code> in <code>set</code> clears a field.",
    ),
    ("task.cancel", "<code>ref</code>", "<code>{short_id, status}</code>."),
    ("task.reopen", "<code>ref</code>", "<code>{short_id, status}</code>."),
    ("tag.add", "<code>ref</code>, <code>tags</code>", "The task's tags."),
    ("annotation.add", "<code>ref</code>, <code>body</code>", "The annotation."),
    ("dependency.add", "<code>ref</code>, <code>depends_on</code>", "Dep state + <code>blocked</code>."),
    ("dependency.remove", "<code>ref</code>, <code>depends_on</code>", "Dep state + <code>blocked</code>."),
    (
        "report.summary",
        "<code>group_by?</code>, <code>filter?</code>, <code>metrics?</code>",
        "<code>{groups, generated}</code>. <code>group_by</code> defaults to \
         <code>project</code>.",
    ),
    ("store.export", "<code>filter?</code>", "<code>{tasks, dropped_dependencies}</code>."),
    ("store.import", "<code>tasks</code>", "<code>{imported}</code>."),
    ("event.list", "<code>limit?</code>", "<code>{count, events}</code> — the append-only log."),
    ("reminder.fire", "<code>ref</code>, <code>at</code>", "<code>{fired, short_id, at}</code>. Idempotent."),
    ("core.capabilities", "—", "<code>{api, methods, features, default_project}</code>."),
];

/// The CLI verbs this guide documents — read straight off the rendered table.
/// Test-only: the page renders from [`VERBS`] directly, so this exists purely to
/// let the drift guard compare that same table against clap.
#[cfg(test)]
fn documented_verbs() -> Vec<&'static str> {
    VERBS.iter().map(|(v, _, _)| *v).collect()
}

/// The JSON API methods this guide documents — read straight off [`METHODS`].
/// Test-only, same reason as [`documented_verbs`].
#[cfg(test)]
fn documented_methods() -> Vec<&'static str> {
    METHODS.iter().map(|(m, _, _)| *m).collect()
}

/// The one-line description of `verb`, taken from the terminal command registry
/// so the guide and `tasqx <verb> -h` cannot disagree.
///
/// Falls back to the empty string rather than panicking: a missing entry is
/// already a hard test failure in `html_verbs_agree_with_cmddoc`, and the
/// generator has no business aborting `tasqx docs` over a documentation gap.
fn verb_summary(verb: &str) -> &'static str {
    crate::cmddoc::find(verb).map(|d| d.summary).unwrap_or("")
}

/// The fields `modify --clear` accepts. Asserted equal to `main::CLEARABLE`.
pub const DOCUMENTED_CLEAR_FIELDS: [&str; 8] =
    ["project", "priority", "due", "scheduled", "wait", "remind", "recurrence", "estimate"];

/// The guide's pages, in nav order: `(anchor id, nav label, page title)`.
const PAGES: [(&str, &str, &str); 11] = [
    ("overview", "Overview", "What tasqx is"),
    ("install", "Install &amp; quickstart", "Install and quickstart"),
    ("commands", "Commands", "Every command"),
    ("filters", "Filter grammar", "The filter grammar"),
    ("scheduling", "Scheduling &amp; recurrence", "Dates and recurrence"),
    ("reminders", "Reminders", "Reminders"),
    ("daemon", "Daemon &amp; watch", "The daemon and live watch"),
    ("mcp", "MCP", "The MCP server"),
    ("api", "JSON API", "The JSON API"),
    ("data", "Export &amp; import", "Export and import"),
    ("themes", "Themes &amp; reports", "Themes, charts and reports"),
];

/// Render the whole guide as one self-contained HTML string.
pub fn generate() -> String {
    let mut body = String::new();

    body.push_str(&header());
    body.push_str("<div class=\"shell\">");
    body.push_str(&nav());
    body.push_str("<main>");

    body.push_str(&page_overview());
    body.push_str(&page_install());
    body.push_str(&page_commands());
    body.push_str(&page_filters());
    body.push_str(&page_scheduling());
    body.push_str(&page_reminders());
    body.push_str(&page_daemon());
    body.push_str(&page_mcp());
    body.push_str(&page_api());
    body.push_str(&page_data());
    body.push_str(&page_themes());

    body.push_str("</main></div>");
    body.push_str(&format!(
        "<footer>tasqx {ver} · every command and every block of output on this page was \
         executed against this binary. One file, no external requests.</footer>",
        ver = esc(env!("CARGO_PKG_VERSION")),
    ));

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>tasqx — user guide</title>\n<style>\n{css}\n</style>\n</head>\n\
         <body>\n{body}\n<script>\n{js}\n</script>\n</body>\n</html>\n",
        css = css(),
        js = js(),
    )
}

// ============================================================================
// Chrome
// ============================================================================

fn header() -> String {
    format!(
        "<header class=\"top\">\
           <div class=\"brand\">tasqx <span class=\"muted\">user guide</span></div>\
           <button id=\"navtoggle\" aria-label=\"Toggle navigation\">Menu</button>\
           <div class=\"ver muted\">v{}</div>\
         </header>",
        esc(env!("CARGO_PKG_VERSION"))
    )
}

fn nav() -> String {
    let mut links = String::new();
    for (id, label, _) in PAGES {
        // `label` is a literal above and already entity-safe; ids are literals too.
        links.push_str(&format!("<a href=\"#{id}\" data-page=\"{id}\">{label}</a>"));
    }
    format!("<nav id=\"nav\">{links}</nav>")
}

// ============================================================================
// Page 1 — Overview
// ============================================================================

fn page_overview() -> String {
    let mut s = page_open("overview", "What tasqx is");

    s.push_str(&lead(
        "tasqx is a fast, terminal-first, AI-native task manager. It is a headless Rust \
         core engine that exposes one stable, versioned JSON API — and every surface you \
         touch (the CLI, the MCP server, the HTML report, this guide) is a client of that \
         one contract.",
    ));

    s.push_str(&h3("The five ideas that shape everything else"));
    s.push_str(&table(
        &["Principle", "What it means for you"],
        &[
            &["Fast", "One-shot commands open SQLite, do the work, and exit. No daemon required, no warmup."],
            &["Local-first", "Your data is one SQLite file on your disk. No account, no cloud, works offline forever."],
            &["One API", "The CLI does nothing you cannot do over JSON. Every verb below names the method it calls."],
            &["AI-native", "The same typed API drives a bundled MCP server, so an agent is a first-class user."],
            &["Honest", "Every command speaks human text and <code>--json</code>. Exit codes are contract, not decoration."],
        ],
    ));

    s.push_str(&h3("How the pieces fit"));
    s.push_str(&p(
        "The core is a plain Rust library. The CLI links it and calls functions in-process — \
         no IPC on the hot path. The JSON API is a thin envelope over that <em>same</em> dispatch \
         layer, so \"call a function\" and \"send a JSON command\" run identical code. There is \
         exactly one dispatch table.",
    ));
    s.push_str(&pre_plain(
        "  tasqx CLI ─┐\n\
         \x20 report   ─┼─→ stdio: one JSON envelope ─┐\n\
         \x20 plugins  ─┘                             ├─→ dispatch ─→ storage ─→ SQLite (tasks.db + WAL)\n\
         \x20 TUI/GUI  ─┐                             │                        └─→ events (append-only log)\n\
         \x20 MCP      ─┴─→ socket / named pipe ─→ daemon ─┘",
    ));

    s.push_str(&h3("Every mutation is logged, in the same transaction"));
    s.push_str(&p(
        "Adding, modifying, completing — each writes its row to an append-only <code>events</code> \
         table inside the <em>same</em> SQLite transaction as the change itself. The log cannot \
         drift from the data, because there is no window in which one exists without the other. \
         That is what makes <a href=\"#themes\">charts</a>, <a href=\"#reminders\">reminder dedupe</a>, \
         and a future sync possible without a migration.",
    ));

    s.push_str(&h3("Where your data lives"));
    s.push_str(&table(
        &["What", "Where"],
        &[
            &["Store", "<code>$TASQX_DB</code> if set, else the platform data dir (<code>%APPDATA%\\tasqx\\tasqx\\data\\tasks.db</code> on Windows — the doubled segment is what the <code>directories</code> crate produces from organization + application)"],
            &["Config", "<code>$TASQX_CONFIG_DIR/config.toml</code>, else the platform config dir"],
            &["Themes", "<code>$TASQX_CONFIG_DIR/themes/*.toml</code>"],
            &["Socket", "<code>$TASQX_SOCK</code>, else a platform default (see <a href=\"#daemon\">Daemon</a>)"],
        ],
    ));
    s.push_str(&note(
        "Point <code>$TASQX_DB</code> at a scratch file to try anything in this guide without \
         touching your real store. Every example below was run exactly that way.",
    ));

    // Rendered from `config::SETTINGS` rather than hand-written. That makes
    // drift structurally impossible rather than merely detected: a new setting
    // appears here the moment it is registered, with no second list to forget.
    // Note what that means for the guard below — since both sides derive from
    // one constant, it CANNOT catch an undocumented setting, and does not claim
    // to. It catches the section being deleted.
    s.push_str(&h3("Settings"));
    s.push_str(&p(
        "<code>tasqx config list</code> shows every setting with the layer that supplied it. \
         Resolution order is the CLI flag, then the <code>TASQX_*</code> environment variable, \
         then <code>config.toml</code>, then the built-in default.",
    ));
    let setting_rows: Vec<Vec<String>> = crate::config::SETTINGS
        .iter()
        .map(|st| {
            vec![
                format!("<code>{}</code>", st.key),
                match st.home {
                    crate::config::Home::Toml => "<code>config.toml</code>".to_string(),
                    crate::config::Home::Store => "the store".to_string(),
                },
                if st.default.is_empty() {
                    "—".to_string()
                } else {
                    format!("<code>{}</code>", st.default)
                },
                esc(st.summary),
            ]
        })
        .collect();
    s.push_str(&table_owned(&["Setting", "Home", "Default", "What it does"], &setting_rows));
    s.push_str(&p(
        "<code>tasqx config edit</code> opens the same settings on a full-screen editor: up and \
         down move, enter toggles a switch or opens a theme picker, escape leaves. Moving through \
         the theme list repaints the screen in that theme <em>before</em> anything is written, \
         which is the one thing editing <code>config.toml</code> by hand cannot do. \
         <code>default_project</code> is shown there but not editable — it lives in the store and \
         is set with <code>tasqx use</code>. Piped or redirected, <code>config edit</code> refuses \
         and exits 2 instead of writing escape codes into your pipe; scripts should use \
         <code>config set</code>.",
    ));

    s.push_str(&page_close("overview"));
    s
}

// ============================================================================
// Page 2 — Install & quickstart
// ============================================================================

fn page_install() -> String {
    let mut s = page_open("install", "Install and quickstart");

    s.push_str(&lead(
        "tasqx is a single static binary with no runtime and no dynamic linking. Build it \
         from the workspace and put it on your PATH.",
    ));

    s.push_str(&h3("Build"));
    s.push_str(&snippet(
        "cargo build --release -p tasqx-cli\n# the binary lands at target/release/tasqx (tasqx.exe on Windows)",
        "",
    ));
    s.push_str(&p(
        "Requires a stable Rust toolchain (built and verified on 1.95). On Windows the MSVC \
         toolchain is discovered automatically — you do not need it on your PATH.",
    ));

    s.push_str(&h3("Sixty seconds with tasqx"));
    s.push_str(&p("Create a project, capture some work, and look at it."));

    s.push_str(&snippet(
        "tasqx init work.tasqx --desc \"The tasqx project itself\"",
        "Project work.tasqx created  ·  now your default project",
    ));

    s.push_str(&p(
        "Now capture a task. Everything after <code>add</code> is the title — except the bits \
         tasqx recognises as structure. That is the <em>inline sugar</em>: <code>+tag</code>, \
         <code>project:</code>, <code>!priority</code>, <code>due:</code>, <code>est:</code>, \
         <code>repeat:</code>, <code>remind:</code>. Whatever is left over is the title.",
    ));

    s.push_str(&snippet(
        "tasqx add \"Ship the v1 JSON API freeze +api +release project:work.tasqx !high due:friday est:4h\"",
        "Added #1  ·  pending  ·  urgency 17.5  ·  work.tasqx\n  Ship the v1 JSON API freeze",
    ));

    s.push_str(&p(
        "The sugar was consumed; only the real title remains. Tags and a due date, no project — \
         <code>init</code> made <code>work.tasqx</code> the default, so it is filled in for you:",
    ));

    s.push_str(&snippet(
        "tasqx add \"Write the user guide +docs due:friday\"",
        "Added #2  ·  pending  ·  urgency 11.5  ·  work.tasqx\n  Write the user guide",
    ));

    s.push_str(&p(
        "Every field is also available as a flag, which is what you want when a value contains \
         spaces or comes from a variable:",
    ));

    s.push_str(&snippet(
        "tasqx add \"Renew the TLS cert\" project:work.tasqx +ops --due -1d",
        "Added #3  ·  pending  ·  urgency 12.0  ·  work.tasqx\n  Renew the TLS cert",
    ));

    s.push_str(&warn(
        "Flags go <em>outside</em> the quoted title. <code>tasqx add \"Renew the cert --due -1d\"</code> \
         puts the literal text <code>--due -1d</code> in your title — the shell handed tasqx one \
         argument, and tasqx believed it. Quote the title, leave the flags bare.",
    ));

    s.push_str(&h3("Look at the working set"));
    s.push_str(&p(
        "Bare <code>tasqx</code> is <code>tasqx list</code> with the <code>@working</code> filter: \
         everything pending or active that is not blocked, hottest first.",
    ));
    s.push_str(&snippet(
        "tasqx",
        "  ID    URG  P  TASK                                  PROJECT         DUE                     TAGS\n\
         --------------------------------------------------------------------------------------------------\n\
         \x20  1   17.5  H  Ship the v1 JSON API freeze           work.tasqx      2026-07-17T00:00:00Z    api release\n\
         \x20  3   12.0  -  Renew the TLS cert                    work.tasqx      2026-07-15T00:00:00Z    ops\n\
         \x20  2   11.5  -  Write the user guide                  work.tasqx      2026-07-17T00:00:00Z    docs\n\
         --------------------------------------------------------------------------------------------------\n\
         3 task(s)",
    ));

    s.push_str(&p(
        "That <code>URG</code> column is urgency — a computed score, not something you set. \
         <code>tasqx next</code> is the \"what now\" button: the single hottest unblocked task.",
    ));
    s.push_str(&snippet(
        "tasqx next",
        "#1  (urgency 17.5)  Ship the v1 JSON API freeze",
    ));

    s.push_str(&p("And <code>tasqx why</code> shows its arithmetic, so the ranking is never a mystery:"));
    s.push_str(&snippet(
        "tasqx why 1",
        "Why #1 has urgency 17.5\n\
         \x20 priority         6.00\n\
         \x20 due_proximity   11.46\n\
         \x20 age             -0.00\n\
         \x20 = total          17.5",
    ));

    s.push_str(&h3("Work it, finish it"));
    s.push_str(&snippet(
        "tasqx start 1\ntasqx stop 1\ntasqx done 1",
        "Started task  ·  timer running (since 2026-07-16T08:51:09.6070293Z)\n\
         Stopped  ·  tracked PT0S\n\
         Done  ·  completed 2026-07-16T08:51:10.0430255Z",
    ));

    s.push_str(&h3("Where to go next"));
    s.push_str(&table(
        &["If you want to…", "Read"],
        &[
            &["Know every verb and flag", "<a href=\"#commands\">Commands</a>"],
            &["Ask precise questions of your store", "<a href=\"#filters\">Filter grammar</a>"],
            &["Say \"friday\" or \"every 3 days\"", "<a href=\"#scheduling\">Scheduling &amp; recurrence</a>"],
            &["Be told about a task before it is late", "<a href=\"#reminders\">Reminders</a>"],
            &["Give an AI agent access", "<a href=\"#mcp\">MCP</a>"],
            &["Script tasqx from another language", "<a href=\"#api\">JSON API</a>"],
        ],
    ));

    s.push_str(&page_close("install"));
    s
}

// ============================================================================
// Page 3 — Commands
// ============================================================================

fn page_commands() -> String {
    let mut s = page_open("commands", "Every command");

    s.push_str(&lead(
        "Every verb is a thin translation to exactly one core API method: it builds a params \
         object, dispatches, and renders. Add <code>--json</code> to any of them to get the raw \
         API result instead of the table.",
    ));

    s.push_str(&h3("Global flags"));
    s.push_str(&p("These work on every subcommand."));
    s.push_str(&table(
        &["Flag", "Effect"],
        &[
            &["<code>--json</code>", "Print the raw JSON API result instead of the human table."],
            &["<code>--theme &lt;name&gt;</code>", "Override the theme (<code>nord</code>, <code>gruvbox</code>, <code>dracula</code>, <code>solarized</code>, <code>mono</code>, or a user file). Beats <code>$TASQX_THEME</code> and the config."],
            &["<code>--socket &lt;addr&gt;</code>", "Socket / named pipe of a daemon. Overrides <code>$TASQX_SOCK</code>."],
            &["<code>--no-daemon</code>", "Never route through a daemon; always run in-process. The escape hatch for scripts."],
            &["<code>--help</code>, <code>--version</code>", "The source of truth for this page."],
        ],
    ));

    s.push_str(&h3("The verb table"));
    // The count is counted, not spelled out. It was written as "Twenty-six" and
    // the table had already grown to 28 — the same restated-value bug as the
    // prose column below, and one no test could see because a wrong number reads
    // exactly like a right one.
    s.push_str(&p(&format!(
        "{} verbs. The method column is the API call the verb makes — that mapping is \
         the whole contract.",
        VERBS.len()
    )));
    // Rendered from VERBS, which the drift test asserts against clap's own
    // subcommand table. The page cannot list a verb the CLI lacks, or omit one it has.
    //
    // The "What it does" cell comes from `cmddoc` — the same string `tasqx
    // <verb> -h` prints — rather than from a prose column here, so the guide and
    // the terminal cannot describe a verb differently. It is plain text, so it
    // goes through `esc` like every other untrusted-shaped cell.
    let verb_rows: Vec<Vec<String>> = VERBS
        .iter()
        .map(|(verb, aliases, method)| {
            let m = if method.starts_with('—') || method.starts_with('(') {
                method.to_string()
            } else {
                format!("<code>{method}</code>")
            };
            vec![
                format!("<code>{verb}</code>"),
                aliases.to_string(),
                m,
                esc(verb_summary(verb)),
            ]
        })
        .collect();
    s.push_str(&table_owned(&["Verb", "Aliases", "Method", "What it does"], &verb_rows));

    s.push_str(&h3("Referring to a task"));
    s.push_str(&p(
        "Anywhere a command takes a <code>&lt;ref&gt;</code>, it accepts either the short id you \
         see in the table (<code>1</code>) or the full UUID. Short ids are for your fingers; UUIDs \
         are stable forever and are what <a href=\"#data\">export</a> carries.",
    ));

    // ---- add
    s.push_str(&h3("add"));
    s.push_str(&p("Capture a task. Flags win over inline sugar when both name the same field."));
    s.push_str(&table(
        &["Flag", "Sugar", "Notes"],
        &[
            &["<code>--project &lt;p&gt;</code>", "<code>project:</code>, <code>proj:</code>", "Free-form dotted name."],
            &["<code>--priority &lt;p&gt;</code> / <code>-p</code>", "<code>!high</code>, <code>!h</code>", "<code>H</code>, <code>M</code>, <code>L</code> (or high/medium/low)."],
            &["<code>--tag &lt;t&gt;</code> / <code>-t</code>", "<code>+tag</code>", "Repeatable."],
            &["<code>--due &lt;d&gt;</code>", "<code>due:</code>", "<a href=\"#scheduling\">Natural language</a>."],
            &["<code>--scheduled &lt;d&gt;</code>", "<code>scheduled:</code>, <code>sched:</code>", "When you plan to start."],
            &["<code>--wait &lt;d&gt;</code>", "<code>wait:</code>", "Hide in the backlog until then."],
            &["<code>--repeat &lt;r&gt;</code>", "<code>repeat:</code>, <code>every:</code>", "<a href=\"#scheduling\">Recurrence rule</a>."],
            &["<code>--remind &lt;r&gt;</code>", "<code>remind:</code>", "<a href=\"#reminders\">Offset or absolute</a>."],
            &["<code>--estimate &lt;e&gt;</code> / <code>-e</code>", "<code>est:</code>, <code>estimate:</code>", "<code>4h</code>, <code>90m</code>, <code>1h30m</code>, <code>2d</code>, or ISO <code>PT4H</code>."],
        ],
    ));
    s.push_str(&p(
        "A <code>project:</code> must be one you created — <code>init</code> it first, and every \
         task is somewhere <code>projects</code> lists:",
    ));
    s.push_str(&snippet(
        "tasqx init home\ntasqx add \"Water the plants project:home repeat:\\\"every 3 days\\\" due:today\"",
        "Project home created  ·  default is still work.tasqx  (tasqx use home)\n\
         Added #4  ·  pending  ·  urgency 12.0  ·  home\n  Water the plants",
    ));

    // ---- modify
    s.push_str(&h3("modify"));
    s.push_str(&p(
        "Takes the same sugar and the same date grammar as <code>add</code> — a token means the \
         same thing in both verbs. Bare words become the new title; omit them to leave it alone.",
    ));
    s.push_str(&snippet(
        "tasqx modify 2 due:monday !medium",
        "Modified #2  ·  rev 4\n  due         <- 2026-07-20T00:00:00Z\n  priority    <- M",
    ));
    s.push_str(&p(
        "Setting and clearing are deliberately different shapes. A value is <code>due:friday</code> \
         or <code>--due friday</code>; removal is <em>only ever</em> <code>--clear due</code>. There \
         is no magic empty value, so a shell variable that expands to nothing can never silently \
         wipe a field it meant to set.",
    ));
    s.push_str(&snippet(
        "tasqx modify 2 --clear priority",
        "Modified #2  ·  rev 5\n  priority    <- (cleared)",
    ));
    s.push_str(&p("<code>--clear</code> is repeatable over a closed set:"));
    s.push_str(&pre_plain(&DOCUMENTED_CLEAR_FIELDS.join("   ")));
    s.push_str(&p(
        "<code>title</code> and <code>status</code> are absent on purpose: a task without a title \
         is not a task, and lifecycle moves through <code>start</code>/<code>done</code>/<code>cancel</code> \
         so their invariants hold. Naming a field in both a set and a <code>--clear</code> is a \
         <code>bad_request</code>, not a precedence puzzle.",
    ));
    s.push_str(&p(
        "<code>--expected-rev &lt;n&gt;</code> gives you optimistic concurrency: the modify fails \
         with <code>conflict</code> (exit 5) unless the task is still at that rev, so a concurrent \
         edit is reported instead of clobbered. <code>tasqx show &lt;ref&gt; --json</code> reports the \
         current <code>_rev</code>, and every successful modify prints the new one.",
    ));

    // ---- list
    s.push_str(&h3("list"));
    // The key list is rendered from `engine::SORT_KEYS`, never retyped: this
    // page is where a reader looks up what `sort` accepts, and a stale list
    // here sends them to a key the engine now refuses.
    s.push_str(&p(&format!(
        "Everything after <code>list</code> is the <a href=\"#filters\">filter</a>. No filter means \
         <code>@working</code>. Results sort by <code>-urgency</code>. Callers of the JSON API can \
         pass <code>sort</code>; the valid keys are {}, each optionally prefixed with <code>-</code> \
         for descending. An unknown key is rejected rather than ignored.",
        tasqx_core::engine::SORT_KEYS
            .iter()
            .map(|k| format!("<code>{k}</code>"))
            .collect::<Vec<_>>()
            .join(", ")
    )));
    s.push_str(&snippet(
        "tasqx list \"project:work.tasqx +api\"",
        "  ID    URG  P  TASK                                  PROJECT         DUE                     TAGS\n\
         --------------------------------------------------------------------------------------------------\n\
         \x20  1   17.5  H  Ship the v1 JSON API freeze           work.tasqx      2026-07-17T00:00:00Z    api release\n\
         --------------------------------------------------------------------------------------------------\n\
         1 task(s)",
    ));

    // ---- show
    s.push_str(&h3("show"));
    s.push_str(&p("Everything about one task, including annotations and dependency state."));
    s.push_str(&snippet(
        "tasqx show 1",
        "#1  Ship the v1 JSON API freeze\n\
         \x20 status     pending\n\
         \x20 priority   H\n\
         \x20 project    work.tasqx\n\
         \x20 urgency    17.5\n\
         \x20 due        2026-07-17T00:00:00Z\n\
         \x20 blocked    false\n\
         \x20 tags       api release\n\
         \x20 · Blocked on the D12 decision",
    ));

    // ---- annotate
    s.push_str(&h3("annotate"));
    s.push_str(&snippet(
        "tasqx annotate 1 \"Blocked on the D12 decision\"",
        "Annotated #1: Blocked on the D12 decision",
    ));

    // ---- dep
    s.push_str(&h3("dep and undep"));
    s.push_str(&p(
        "<code>tasqx dep 2 1</code> reads \"#2 depends on #1\". A task with at least one \
         not-yet-done dependency is <strong>blocked</strong>, and blocked tasks drop out of \
         <code>@working</code> — which is exactly why <code>next</code> never hands you something \
         you cannot start.",
    ));
    s.push_str(&snippet(
        "tasqx dep 2 1",
        "#2 now depends on #1   ·   depends on: #1   blocked=true",
    ));
    s.push_str(&p("Note that #2 has vanished from the working set — it is blocked by #1:"));
    s.push_str(&snippet(
        "tasqx list",
        "  ID    URG  P  TASK                                  PROJECT         DUE                     TAGS\n\
         --------------------------------------------------------------------------------------------------\n\
         \x20  1   17.5  H  Ship the v1 JSON API freeze           work.tasqx      2026-07-17T00:00:00Z    api release\n\
         \x20  3   12.0  -  Renew the TLS cert                    work.tasqx      2026-07-15T00:00:00Z    ops\n\
         \x20  4   12.0  -  Water the plants                      home            2026-07-16T00:00:00Z    \n\
         --------------------------------------------------------------------------------------------------\n\
         3 task(s)",
    ));
    s.push_str(&snippet(
        "tasqx undep 2 1",
        "#2 no longer depends on #1   ·   still depends on: (none)   blocked=false",
    ));

    // ---- projects
    s.push_str(&h3("projects"));
    s.push_str(&p(
        "Lists projects created with <code>init</code>. Add <code>--all</code> to include archived \
         ones. The <code>*</code> marks the <a href=\"#use\">default project</a>. Every project a \
         task can be in is on this list: naming a project no <code>init</code> created is \
         <code>not_found</code> (exit 4) and naming an archived one is <code>conflict</code> \
         (exit 5), on <code>add</code> and <code>modify</code> alike — a typo'd \
         <code>project:</code> tells you, instead of filing the task somewhere this list would \
         never show it.",
    ));
    s.push_str(&snippet(
        "tasqx projects",
        "DEFAULT  PROJECT                   ARCHIVED   DESCRIPTION\n\
         *        work.tasqx                no         The tasqx project itself",
    ));

    // ---- use
    s.push_str(&h3("use"));
    s.push_str(&p(
        "The default project is where a bare <code>tasqx add</code> lands — an <code>add</code> with \
         no <code>project:</code> inherits it. The <em>first</em> project you create claims it; after \
         that, nothing moves it implicitly. <code>use</code> is the one way to change it.",
    ));
    s.push_str(&snippet(
        "tasqx init prive.klussen\ntasqx use prive.klussen\ntasqx add \"Fix the shed door\"",
        "Project prive.klussen created  ·  default is still work.tasqx  (tasqx use prive.klussen)\n\
         Default project is now prive.klussen  ·  a bare `tasqx add` lands here  ·  was work.tasqx\n\
         Added #7  ·  pending  ·  urgency 0.0  ·  prive.klussen\n\
         \x20 Fix the shed door",
    ));
    s.push_str(&p(
        "Note what <code>init</code> did <em>not</em> do: creating <code>prive.klussen</code> left the \
         default alone and told you so, naming the command that would move it. Every <code>add</code> \
         reports the project it landed in, so an inherited project is never a silent one.",
    ));
    s.push_str(&p(
        "The project must exist and must not be archived — <code>use</code> on an unknown name is \
         <code>not_found</code> (exit 4) and never writes; on an archived one it is \
         <code>conflict</code> (exit 5). Archiving the project that <em>is</em> the default clears \
         the default rather than leaving it pointed at a retired project, and a bare \
         <code>add</code> is then projectless until you <code>use</code> another.",
    ));

    // ---- lifecycle
    s.push_str(&h3("start, stop, done, cancel, reopen"));
    s.push_str(&p(
        "<code>start</code> runs a timer and, by default, stops any other active task — pass \
         <code>--keep</code> to opt out of single-active. <code>done</code> on a recurring task \
         spawns the next instance and tells you so:",
    ));
    s.push_str(&snippet(
        "tasqx done 4",
        "Done  ·  completed 2026-07-16T08:51:10.0430255Z\n  -> next: #5 due 2026-07-19T00:00:00Z",
    ));

    // ---- docs
    s.push_str(&h3("docs"));
    s.push_str(&p("This page. It needs no store and no network."));
    s.push_str(&table(
        &["Invocation", "Behaviour"],
        &[
            &["<code>tasqx docs</code>", "Write the guide to a temp file and open your default browser."],
            &["<code>tasqx docs --out &lt;path&gt;</code>", "Write it there. Never opens a browser."],
            &["<code>tasqx docs --no-open</code>", "Write the temp file, print the path, do not open."],
            &["<code>tasqx docs --stdout</code>", "Write the HTML to stdout (pipe it anywhere)."],
        ],
    ));
    s.push_str(&note(
        "<code>tasqx docs</code> never fails because a browser is missing. On a headless box the \
         launch is attempted, the failure is reported on stderr with the file path, and the \
         command exits 0 — the file is the deliverable, opening it is a courtesy.",
    ));

    s.push_str(&h3("Exit codes"));
    s.push_str(&p("Stable contract. Script against these."));
    s.push_str(&table(
        &["Code", "Meaning"],
        &[
            &["<code>0</code>", "Success."],
            &["<code>1</code>", "Could not open the store, or a local I/O failure."],
            &["<code>2</code>", "<code>bad_request</code> — a bad value, an unparseable date, contradictory flags."],
            &["<code>4</code>", "<code>not_found</code> — no such task, project, or reference."],
            &["<code>5</code>", "<code>conflict</code> — a lost <code>--expected-rev</code> race, or a lifecycle rule."],
        ],
    ));
    s.push_str(&snippet(
        "tasqx modify 1 --expected-rev 1 -p H ; echo \"exit=$?\"",
        "error [conflict]: expected_rev 1 but task is at rev 2\nexit=5",
    ));

    s.push_str(&page_close("commands"));
    s
}

// ============================================================================
// Page 4 — Filters
// ============================================================================

fn page_filters() -> String {
    let mut s = page_open("filters", "The filter grammar");

    s.push_str(&lead(
        "One small grammar, used everywhere a query is taken: <code>list</code>, <code>report</code>, \
         <code>export</code>, <code>watch</code>, <code>task.list</code>, and the MCP tools. Learn it \
         once.",
    ));

    s.push_str(&h3("The grammar"));
    // Rendered from the parser's own const, never transcribed. The transcription
    // that used to live here drifted: it still said a tag took a bare `WORD`
    // after quoted tags shipped, while the paragraph directly below it — this
    // same page — advertised `+"needs paint"`.
    s.push_str(&pre_plain(tasqx_core::filter::GRAMMAR));

    s.push_str(&h3("Predicates"));
    s.push_str(&table(
        &["Predicate", "Matches"],
        &[
            &["<code>+api</code>", "Tasks tagged <code>api</code>."],
            &["<code>-api</code>", "Tasks <em>not</em> tagged <code>api</code>."],
            &["<code>project:work.tasqx</code>", "Exact project match."],
            &["<code>status:pending</code>", "Exact status: <code>backlog</code>, <code>pending</code>, <code>active</code>, <code>done</code>, <code>cancelled</code>."],
            &["<code>@working</code>", "Status pending or active, <em>and</em> not blocked. The default filter."],
            &["<code>@blocked</code>", "At least one dependency that is not yet done. Also spelled <code>+blocked</code> or <code>status:blocked</code>."],
            &["<code>due.before:&lt;RFC3339&gt;</code>", "Due strictly before that instant."],
            &["<code>due.after:&lt;RFC3339&gt;</code>", "Due strictly after that instant."],
        ],
    ));

    s.push_str(&h3("Values with spaces"));
    s.push_str(&p(
        "A space separates predicates, so a project or tag whose name contains one must be \
         double-quoted — <code>project:\"Home Renovation\"</code>, <code>+\"needs paint\"</code>. \
         The rule is the shell's: inside quotes, spaces and parentheses are ordinary characters \
         and <code>and</code>/<code>or</code> are ordinary words, so a project named \
         <code>a (b)</code> no longer breaks the grouping. Write <code>\\\"</code> for a literal \
         quote and <code>\\\\</code> for a literal backslash. Quoting changes where a predicate \
         <em>ends</em>, not what it means: <code>\"project:x\"</code> is still a project match.          Your shell's own quotes are enough — tasqx reads each argument the shell hands it as one          value, so there is nothing to escape twice.",
    ));
    s.push_str(&p(
        "This is one rule for the whole tool, not a filter dialect: <code>tasqx add</code> and \
         <code>tasqx modify</code> split their inline sugar with the same scanner, so a name you \
         can create you can also filter for, spelled the same way. The one value that needs the \
         escaped form on both sides is a name containing a quote — \
         <code>project:\"My \\\"Big\\\" Project\"</code> — because an argument carrying a literal \
         quote is read by the scanner rather than taken whole.",
    ));
    s.push_str(&snippet("tasqx list project:\"Home Renovation\" +\"needs paint\"", ""));

    s.push_str(&h3("Combining"));
    s.push_str(&p(
        "A space is an implicit <code>and</code>. <code>or</code> and parentheses are explicit, and \
         both keywords are case-insensitive.",
    ));
    s.push_str(&snippet(
        "tasqx list \"(+api or +ops) and status:pending\"",
        "  ID    URG  P  TASK                                  PROJECT         DUE                     TAGS\n\
         --------------------------------------------------------------------------------------------------\n\
         \x20  1   17.5  H  Ship the v1 JSON API freeze           work.tasqx      2026-07-17T00:00:00Z    api release\n\
         \x20  3   12.0  -  Renew the TLS cert                    work.tasqx      2026-07-15T00:00:00Z    ops\n\
         --------------------------------------------------------------------------------------------------\n\
         2 task(s)",
    ));

    s.push_str(&h3("Two behaviours worth knowing"));
    s.push_str(&p(
        "<strong>Dates compare as instants, not strings.</strong> <code>due.before:</code> and \
         <code>due.after:</code> parse both sides to timestamps and compare those. \
         <code>2026-07-17T00:00:00Z</code> and <code>2026-07-17T02:00:00+02:00</code> are the same \
         instant, and the filter knows it — a lexicographic comparison would not.",
    ));
    s.push_str(&warn(
        "<strong>Unknown tokens are rejected.</strong> A token the grammar does not recognise \
         is an error naming the token, not a term that matches everything. So \
         <code>tasqx list \"priority:H\"</code> fails — <code>priority:</code> is not a \
         predicate — instead of silently listing every task. The same goes for a dangling \
         <code>or</code> or an unclosed <code>(</code>. Values are the other half of the rule \
         and behave differently on purpose: <code>status:pendign</code> parses fine and simply \
         matches no row, because the token shape is grammar while the value is data.",
    ));

    s.push_str(&h3("Where the grammar stops"));
    s.push_str(&p(
        "On purpose, and permanently: no arithmetic, no computed expressions, no subqueries. \
         A filter language that grows those becomes a query language nobody can predict. \
         For anything beyond this, <a href=\"#data\">export</a> to JSON and use a real tool — \
         <code>jq</code>, a script, whatever you like. The store is yours.",
    ));

    s.push_str(&page_close("filters"));
    s
}

// ============================================================================
// Page 5 — Scheduling
// ============================================================================

fn page_scheduling() -> String {
    let mut s = page_open("scheduling", "Dates and recurrence");

    s.push_str(&lead(
        "Every date field — <code>due</code>, <code>scheduled</code>, <code>wait</code> — takes the \
         same natural-language grammar, through the flag or through the sugar. It resolves to \
         RFC3339 UTC at the moment you type it.",
    ));

    s.push_str(&h3("The four date fields"));
    s.push_str(&table(
        &["Field", "Means"],
        &[
            &["<code>due</code>", "The deadline. Drives urgency and is the anchor for relative reminders."],
            &["<code>scheduled</code>", "When you intend to start. Informational."],
            &["<code>wait</code>", "Hide this task until then — it stays out of the working set."],
            &["<code>estimate</code>", "Not a date: an effort duration, totalled by <code>report</code>."],
        ],
    ));

    s.push_str(&h3("What you can write"));
    s.push_str(&table(
        &["Form", "Examples"],
        &[
            &["Absolute", "<code>2026-07-20</code>, <code>2026-07-20T17:00</code>, <code>\"2026-07-20 17:00\"</code>, any RFC3339"],
            &["Relative words", "<code>today</code>, <code>tomorrow</code>, <code>yesterday</code>"],
            &["Weekdays", "<code>monday</code>…<code>sunday</code>, <code>mon</code>…<code>sun</code>, optional leading <code>next</code>"],
            &["Long offsets", "<code>\"in 3 days\"</code>, <code>\"in 2 weeks\"</code>, <code>\"in 1 month\"</code>"],
            &["Short offsets", "<code>3d</code>, <code>2w</code>, <code>1mo</code>, <code>1y</code> — signed: <code>+3d</code>, <code>-1d</code>"],
            &["Boundaries", "<code>eom</code> / <code>\"end of month\"</code>, <code>eow</code> / <code>\"end of week\"</code> (ISO week ends Sunday)"],
            &["Trailing time", "<code>\"friday 17:00\"</code>, <code>\"tomorrow 9am\"</code>, <code>\"monday 5pm\"</code>"],
            &["Leading filler", "<code>\"at 6pm\"</code>, <code>\"on friday\"</code>, <code>\"by monday 5pm\"</code> — <code>at</code>/<code>on</code>/<code>by</code>/<code>@</code> are ignored"],
        ],
    ));

    s.push_str(&h3("The rules that resolve ambiguity"));
    s.push_str(&table(
        &["Situation", "Resolution"],
        &[
            &["A date with no time", "00:00:00 — the start of that day."],
            &["A bare time (<code>9am</code>)", "Today, or tomorrow if that time already passed."],
            &["A weekday that <em>is</em> today", "The next one — seven days out, not zero."],
            &["Any naive date/time", "Interpreted as UTC, like everything else in the store."],
        ],
    ));

    s.push_str(&warn(
        "Short offsets carry <strong>day</strong>-and-larger units only: <code>d</code>, <code>w</code>, \
         <code>mo</code>, <code>y</code>. <code>-2h</code> is not a date — it is a reminder offset, and \
         a different grammar. Feeding it to <code>--due</code> is a clean error, not a guess:",
    ));
    s.push_str(&snippet(
        "tasqx add \"Overdue ping\" --due -2h",
        "error [bad_request]: could not parse date: \"-2h\" (try e.g. tomorrow, friday, 2026-07-20, \"in 3 days\", eom, or 2026-07-20T17:00)",
    ));

    s.push_str(&h3("A leading hyphen needs no escaping"));
    s.push_str(&p(
        "<code>--due -1d</code> works. It looks like it should trip the argument parser into \
         reading <code>-1d</code> as a flag — every date-taking flag opts out of that explicitly, \
         so a signed offset is always a value:",
    ));
    s.push_str(&snippet(
        "tasqx add \"Renew the TLS cert\" project:work.tasqx +ops --due -1d",
        "Added #3  ·  pending  ·  urgency 12.0  ·  work.tasqx\n  Renew the TLS cert",
    ));

    s.push_str(&h3("Estimates"));
    s.push_str(&p(
        "Human durations, parsed at the edge and stored as ISO-8601 so <code>report</code> can \
         total them. <code>4h</code>, <code>90m</code>, <code>1h30m</code>, <code>2d</code>, <code>1w</code>, \
         or ISO <code>PT4H</code> directly.",
    ));
    s.push_str(&snippet(
        "tasqx add \"Nail down the schema\" est:soon",
        "error [bad_request]: could not parse duration: \"soon\" (try e.g. 4h, 90m, 1h30m, 2d, 1w, or ISO PT4H)",
    ));

    s.push_str(&h3("Recurrence"));
    s.push_str(&p(
        "A recurring task is a <strong>template</strong>. Completing an instance spawns the next \
         one with its date advanced by the rule. Set it with <code>repeat:</code> / <code>every:</code> \
         / <code>--repeat</code>, and stop it with <code>--clear recurrence</code>.",
    ));
    s.push_str(&table(
        &["Rule", "Example"],
        &[
            &["<code>every N days|weeks|months</code>", "<code>\"every 3 days\"</code>, <code>\"every week\"</code>"],
            &["<code>weekly on &lt;days&gt;</code>", "<code>\"weekly on mon,wed,fri\"</code>"],
            &["<code>monthly on day &lt;D&gt;</code>", "<code>\"monthly on day 15\"</code>"],
            &["<code>monthly on the &lt;Nth&gt; &lt;weekday&gt;</code>", "<code>\"monthly on the 2nd tuesday\"</code>, <code>\"monthly on the last friday\"</code>"],
        ],
    ));
    s.push_str(&p("This is a deliberate subset — not full RRULE. Anything outside it is a clean error."));
    s.push_str(&snippet(
        "tasqx add \"Water the plants project:home repeat:\\\"every 3 days\\\" due:today\"\ntasqx done 4\ntasqx show 5",
        "Added #4  ·  pending  ·  urgency 12.0  ·  home\n\
         \x20 Water the plants\n\
         Done  ·  completed 2026-07-16T08:51:10.0430255Z\n\
         \x20 -> next: #5 due 2026-07-19T00:00:00Z\n\
         #5  Water the plants\n\
         \x20 status     pending\n\
         \x20 priority   -\n\
         \x20 project    home\n\
         \x20 urgency    9.7\n\
         \x20 due        2026-07-19T00:00:00Z\n\
         \x20 repeats    every 3 days\n\
         \x20 blocked    false",
    ));

    s.push_str(&h3("Missed occurrences collapse"));
    s.push_str(&p(
        "If your machine was off for a week, a daily task does <em>not</em> hand you seven \
         instances. The rule advances at least once and then skips every slot at or before now, \
         so you get exactly one future instance. A backfill storm is never useful.",
    ));

    s.push_str(&h3("Month-end, precisely"));
    s.push_str(&p(
        "The two monthly forms differ on purpose, and the difference matters at month boundaries:",
    ));
    s.push_str(&table(
        &["Rule", "From Jan 31", "Why"],
        &[
            &["<code>monthly on day 31</code>", "Jan 31 → Feb 28 → <strong>Mar 31</strong>", "Re-clamps against the stored target day every step, so month-end recovers."],
            &["<code>every 1 month</code>", "Jan 31 → Feb 28 → <strong>Mar 28</strong>", "Advances from the previous (already clamped) date, so it drifts and stays there."],
        ],
    ));
    s.push_str(&note(
        "Pick <code>monthly on day 31</code> when you mean \"the last-ish day of every month\". \
         Pick <code>every 1 month</code> when you mean \"same slot, one month on\".",
    ));

    s.push_str(&page_close("scheduling"));
    s
}

// ============================================================================
// Page 6 — Reminders
// ============================================================================

fn page_reminders() -> String {
    let mut s = page_open("reminders", "Reminders");

    s.push_str(&lead(
        "tasqx is quiet by default. A task notifies you only if you gave it a <code>remind</code>, \
         and nothing else ever puts a task on the reminder heap.",
    ));

    s.push_str(&h3("The two forms"));
    s.push_str(&table(
        &["Form", "Example", "Behaviour"],
        &[
            &["Offset from <code>due</code>", "<code>-1h</code>, <code>-30m</code>, <code>-2d</code>, <code>+15m</code>", "Stays symbolic in the store. Move <code>due</code> and the reminder moves with it."],
            &["Absolute instant", "<code>\"friday 9am\"</code>, <code>2026-07-20T17:00</code>", "Resolved once, at set time, through the same date grammar."],
        ],
    ));
    s.push_str(&p(
        "The <strong>sign</strong> is what disambiguates them: a leading <code>-</code> or \
         <code>+</code> means offset, anything else goes to the date parser. Without that rule \
         <code>3d</code> would be ambiguous — \"3 days before due\" or \"in 3 days\"?",
    ));
    s.push_str(&p(
        "Offsets take <code>s</code>, <code>m</code>, <code>h</code>, <code>d</code>, <code>w</code>. \
         Negative is before due, which is what you almost always want.",
    ));

    s.push_str(&snippet(
        "tasqx modify 1 --remind -1h\ntasqx show 1",
        "Modified #1  ·  rev 5\n\
         \x20 remind      <- -1h\n\
         #1  Ship the v1 JSON API freeze\n\
         \x20 status     pending\n\
         \x20 priority   H\n\
         \x20 project    work.tasqx\n\
         \x20 urgency    17.5\n\
         \x20 due        2026-07-17T00:00:00Z\n\
         \x20 remind     -1h\n\
         \x20 blocked    false\n\
         \x20 tags       api release\n\
         \x20 · Blocked on the D12 decision",
    ));
    s.push_str(&note(
        "Notice <code>remind</code> shows as <code>-1h</code>, not as a resolved timestamp. That is \
         the point: it is still an offset. Push <code>due</code> back a week and the reminder \
         follows, with no second edit.",
    ));

    s.push_str(&h3("Who delivers them"));
    s.push_str(&p(
        "The <a href=\"#daemon\">daemon</a>. It keeps an in-memory min-heap of upcoming reminder \
         instants, rebuilt from the store on start and whenever a task changes. A one-shot \
         <code>tasqx</code> command never fires anything — there would be nobody to fire it.",
    ));
    s.push_str(&snippet(
        "tasqx add \"Deploy the release\" --due \"2026-07-16T09:00\" --remind -1h\ntasqx daemon --socket tasqx-remdemo",
        "Added #1  ·  pending  ·  urgency 12.0\n\
         \x20 Deploy the release\n\
         tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)\n\
         tasqx reminder: [#1] Deploy the release (due 2026-07-16T09:00:00Z)",
    ));
    s.push_str(&p(
        "That reminder had already ripened (due 09:00, minus 1h, and it was past 08:00) — so it \
         fired on the next daemon start. <strong>A reminder that ripened while the daemon was \
         down still fires, once, on the next start.</strong> Sleeping your laptop does not lose it.",
    ));

    s.push_str(&h3("It fires exactly once"));
    s.push_str(&p(
        "Firing writes a <code>reminded</code> event, and that event row <em>is</em> the dedupe \
         record. Restart the daemon and the same reminder does not come back:",
    ));
    s.push_str(&snippet(
        "tasqx daemon --socket tasqx-remdemo   # second start, same store",
        "tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)",
    ));
    s.push_str(&p(
        "Silence — correct. The key is the (task, <em>instant</em>) pair, not just the task: \
         moving <code>due</code> moves a relative reminder to a genuinely new instant, which \
         <em>should</em> fire again.",
    ));

    s.push_str(&h3("Delivery never fails"));
    s.push_str(&p(
        "The always-compiled backend writes one line to stderr and returns. That is the \
         headless/CI-safe path: with no notification transport anywhere, delivery degrades to a \
         logged line and exit 0 — never an error.",
    ));
    s.push_str(&p(
        "Native OS toasts (Windows, macOS, Linux/D-Bus) live behind the off-by-default \
         <code>notify-os</code> build feature <em>and</em> need an explicit opt-in:",
    ));
    s.push_str(&pre_plain("# config.toml\n[notify]\nenabled = true"));
    s.push_str(&p(
        "Even then the stderr line still comes first, so the verifiable surface never depends on \
         which backend is live. Two opt-ins, both off by default — that is the quiet-by-default \
         rule taken seriously.",
    ));

    s.push_str(&h3("Firing one by hand"));
    s.push_str(&p(
        "<code>reminder.fire</code> takes a <code>ref</code> and an <code>at</code> instant, and it is \
         idempotent — the second call reports <code>fired: false</code> rather than notifying twice:",
    ));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"r1\",\"method\":\"reminder.fire\",\"params\":{\"ref\":\"1\",\"at\":\"2026-07-16T00:00:00Z\"}}' | tasqx api\necho '{\"tasqx\":\"1\",\"id\":\"r2\",\"method\":\"reminder.fire\",\"params\":{\"ref\":\"1\",\"at\":\"2026-07-16T00:00:00Z\"}}' | tasqx api",
        "{\"id\":\"r1\",\"ok\":true,\"result\":{\"at\":\"2026-07-16T00:00:00Z\",\"fired\":true,\"short_id\":1},\"tasqx\":\"1\"}\n\
         {\"id\":\"r2\",\"ok\":true,\"result\":{\"at\":\"2026-07-16T00:00:00Z\",\"fired\":false,\"short_id\":1},\"tasqx\":\"1\"}",
    ));
    s.push_str(&p("And the event is in the log, where the dedupe check reads it:"));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"actor\": \"user\",\n\
         \x20 \"entity\": \"task\",\n\
         \x20 \"op\": \"reminded\",\n\
         \x20 \"payload\": {\n\
         \x20   \"at\": \"2026-07-16T00:00:00Z\",\n\
         \x20   \"due\": \"2026-07-17T00:00:00Z\",\n\
         \x20   \"remind\": \"-1h\",\n\
         \x20   \"short_id\": 1,\n\
         \x20   \"title\": \"Deploy the release\"\n\
         \x20 },\n\
         \x20 \"ts\": \"2026-07-16T08:52:35.4429051Z\"\n\
         }",
    ));

    s.push_str(&page_close("reminders"));
    s
}

// ============================================================================
// Page 7 — Daemon & watch
// ============================================================================

fn page_daemon() -> String {
    let mut s = page_open("daemon", "The daemon and live watch");

    s.push_str(&lead(
        "The daemon is optional. It holds one database connection, serves the JSON API over a \
         local socket to many concurrent clients, pushes change notifications, and runs the \
         reminder scheduler. The CLI never requires it.",
    ));

    s.push_str(&h3("One-shot or daemon?"));
    s.push_str(&table(
        &["Mode", "When", "Why"],
        &[
            &["One-shot", "The default. Scripts, cron, the HTML report.", "No process to manage. Open the DB, run one command, exit."],
            &["Daemon", "Long-lived clients: a TUI, a GUI, <code>watch</code>, reminders.", "One writer, warm caches, live push, and something to fire reminders."],
        ],
    ));
    s.push_str(&p(
        "If a daemon is reachable, one-shot commands route through it automatically — single \
         writer, live-update semantics for free. If not, they open the store in-process. \
         <strong>Same command surface either way</strong>, and a missing or stale socket falls back \
         immediately rather than hanging. <code>--no-daemon</code> forces the in-process path.",
    ));

    s.push_str(&h3("Socket addresses"));
    s.push_str(&p("Resolution order: <code>--socket</code>, then <code>$TASQX_SOCK</code>, then the platform default."));
    s.push_str(&table(
        &["Platform", "Default"],
        &[
            &["Windows", "The named pipe <code>tasqx-default</code>"],
            &["Linux", "<code>$XDG_RUNTIME_DIR/tasqx/tasqx.sock</code> (falls back to the data dir)"],
            &["macOS", "<code>&lt;data dir&gt;/tasqx.sock</code> — macOS has no runtime dir"],
        ],
    ));

    s.push_str(&h3("Running it"));
    s.push_str(&p(
        "Diagnostics go to stderr; the socket carries the newline-delimited JSON API. Ctrl-C \
         stops it cleanly, unwinding the accept loop and removing the socket file (a no-op for \
         Windows named pipes). <code>--db</code> points it at a specific store.",
    ));
    s.push_str(&snippet(
        "tasqx daemon --socket tasqx-docsdemo",
        "tasqx daemon: listening on tasqx-docsdemo (Ctrl-C to stop)",
    ));
    s.push_str(&p("Now route a command through it — note this is the ordinary <code>add</code>, unchanged:"));
    s.push_str(&snippet(
        "tasqx --socket tasqx-docsdemo add \"Wire up the docs page +docs project:work.tasqx due:tomorrow\"",
        "Added #6  ·  pending  ·  urgency 11.5  ·  work.tasqx\n  Wire up the docs page",
    ));

    s.push_str(&h3("watch"));
    s.push_str(&p(
        "<code>watch</code> subscribes to a daemon and re-renders on every <code>task.changed</code> \
         push. It takes a <a href=\"#filters\">filter</a>, defaulting to the working set. It needs a \
         running daemon and will never auto-spawn one — it hints instead:",
    ));
    s.push_str(&snippet(
        "tasqx watch --socket nope",
        "tasqx watch: no daemon reachable at nope\nhint: start one with `tasqx daemon` (add `--socket nope` to match)",
    ));
    s.push_str(&p(
        "On a terminal it clears the screen and repaints the table. Through a pipe it streams one \
         line per event instead — so it composes with everything else in your shell:",
    ));
    s.push_str(&snippet(
        "tasqx watch --socket tasqx-docsdemo | cat\n# ... meanwhile, in another shell:\n#   tasqx --socket tasqx-docsdemo add \"Wire up the docs page ...\"\n#   tasqx --socket tasqx-docsdemo done 3",
        "  ID    URG  P  TASK                                  PROJECT         DUE                     TAGS\n\
         --------------------------------------------------------------------------------------------------\n\
         \x20  1   17.5  H  Ship the v1 JSON API freeze           work.tasqx      2026-07-17T00:00:00Z    api release\n\
         \x20  3   12.0  -  Renew the TLS cert                    work.tasqx      2026-07-15T00:00:00Z    ops\n\
         \x20  5    9.7  -  Water the plants                      home            2026-07-19T00:00:00Z    \n\
         \x20  2    8.9  -  Write the user guide                  work.tasqx      2026-07-20T00:00:00Z    docs\n\
         --------------------------------------------------------------------------------------------------\n\
         4 task(s)\n\
         task.changed op=add short_id=6\n\
         task.changed op=done short_id=3",
    ));

    s.push_str(&page_close("daemon"));
    s
}

// ============================================================================
// Page 8 — MCP
// ============================================================================

fn page_mcp() -> String {
    let mut s = page_open("mcp", "The MCP server");

    s.push_str(&lead(
        "tasqx bundles an MCP server, so an AI agent reads and mutates your tasks with zero glue. \
         It is the same core API underneath — the agent and your shell are peers.",
    ));

    s.push_str(&h3("It fails closed"));
    s.push_str(&p(
        "Scope precedence: <code>--token</code>, then <code>$TASQX_MCP_TOKEN</code>, then a \
         least-privilege default of <strong>read-only</strong>. An unwired server never silently \
         exposes destructive tools to a model. Write access is an explicit, deliberate act:",
    ));
    s.push_str(&snippet(
        "tasqx mcp token --scope read\ntasqx mcp token --scope write",
        "tasqx_mcp_read_019f6a1f-a6ab-7b10-bc00-2eae37c94a40\ntasqx_mcp_write_019f6a1f-a6c5-7881-b751-163b313d22f4",
    ));
    s.push_str(&snippet(
        "tasqx mcp serve   # no token",
        "tasqx mcp: no token provided; defaulting to READ-ONLY scope. For write access, pass a token from `tasqx mcp token --scope write`.\n\
         tasqx mcp: serving over stdio (scope=read)",
    ));

    s.push_str(&h3("The tools"));
    s.push_str(&p(
        "Four read tools always; seven write tools only with a write token. Each carries MCP \
         annotations (<code>readOnlyHint</code>, <code>destructiveHint</code>) so a client can reason \
         about them before calling.",
    ));
    s.push_str(&table(
        &["Tool", "Scope", "Does"],
        &[
            &["<code>tasqx_list_tasks</code>", "read", "List tasks by <a href=\"#filters\">filter</a>."],
            &["<code>tasqx_get_task</code>", "read", "One task's full detail."],
            &["<code>tasqx_summary</code>", "read", "Aggregate report by project/status/priority."],
            &["<code>tasqx_list_projects</code>", "read", "List projects."],
            &["<code>tasqx_add_task</code>", "write", "Capture a task."],
            &["<code>tasqx_modify_task</code>", "write", "Change fields."],
            &["<code>tasqx_complete_task</code>", "write", "Complete a task."],
            &["<code>tasqx_start_timer</code>", "write", "Start the timer."],
            &["<code>tasqx_stop_timer</code>", "write", "Stop the timer."],
            &["<code>tasqx_tag_task</code>", "write", "Add tags."],
            &["<code>tasqx_create_project</code>", "write", "Create a project."],
        ],
    ));

    s.push_str(&h3("Talking to it"));
    s.push_str(&p(
        "Newline-delimited JSON-RPC 2.0 on stdin/stdout. Diagnostics go to stderr <em>only</em> — \
         stdout carries nothing but responses, so the transport is never corrupted by a log line.",
    ));
    s.push_str(&snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"demo\",\"version\":\"1\"}}}' | tasqx mcp serve 2>/dev/null",
        "{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{\"capabilities\":{\"tools\":{}},\"protocolVersion\":\"2024-11-05\",\"serverInfo\":{\"name\":\"tasqx\",\"version\":\"0.1.0\"}}}",
    ));
    s.push_str(&snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"tasqx_list_tasks\",\"arguments\":{\"filter\":\"+api\"}}}' | tasqx mcp serve 2>/dev/null",
        "{\"id\":3,\"jsonrpc\":\"2.0\",\"result\":{\"content\":[{\"text\":\"{\\n  \\\"count\\\": 1,\\n  \\\"tasks\\\": [\\n    {\\n      \\\"_rev\\\": 4,\\n      \\\"due\\\": \\\"2026-07-17T00:00:00Z\\\",\\n      \\\"estimate\\\": \\\"PT4H\\\",\\n      \\\"priority\\\": \\\"H\\\",\\n      \\\"project\\\": \\\"work.tasqx\\\",\\n      \\\"short_id\\\": 1,\\n      \\\"status\\\": \\\"pending\\\",\\n      \\\"tags\\\": [\\n        \\\"api\\\",\\n        \\\"release\\\"\\n      ],\\n      \\\"title\\\": \\\"Ship the v1 JSON API freeze\\\",\\n      \\\"urgency\\\": 17.5\\n    }\\n  ]\\n}\",\"type\":\"text\"}],\"isError\":false}}",
    ));
    s.push_str(&p("Ask a read-only server to write, and it refuses by name:"));
    s.push_str(&snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"tasqx_add_task\",\"arguments\":{\"title\":\"nope\"}}}' | tasqx mcp serve 2>/dev/null",
        "{\"id\":4,\"jsonrpc\":\"2.0\",\"result\":{\"content\":[{\"text\":\"error [bad_request]: tool `tasqx_add_task` requires write scope, but this MCP server is running read-only\",\"type\":\"text\"}],\"isError\":true}}",
    ));

    s.push_str(&h3("Wiring it into a client"));
    s.push_str(&p(
        "Most MCP clients take a command and an environment. Mint a token once, then hand the \
         server the scope you actually want it to have:",
    ));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"mcpServers\": {\n\
         \x20   \"tasqx\": {\n\
         \x20     \"command\": \"tasqx\",\n\
         \x20     \"args\": [\"mcp\", \"serve\"],\n\
         \x20     \"env\": { \"TASQX_MCP_TOKEN\": \"tasqx_mcp_write_...\" }\n\
         \x20   }\n\
         \x20 }\n\
         }",
    ));
    s.push_str(&note(
        "Start an agent on a read token. Give it write only once you have watched what it does \
         with read — the default is read-only precisely so that choice is yours to make on purpose.",
    ));

    s.push_str(&page_close("mcp"));
    s
}

// ============================================================================
// Page 9 — JSON API
// ============================================================================

fn page_api() -> String {
    let mut s = page_open("api", "The JSON API");

    s.push_str(&lead(
        "The load-bearing artifact. The CLI is a client; so is the MCP server; so could yours be. \
         One envelope in, one envelope out.",
    ));

    s.push_str(&h3("The transport"));
    s.push_str(&p(
        "<code>tasqx api</code> reads ONE request envelope on stdin and writes ONE response on \
         stdout. No framing, no handshake, no daemon needed. For many calls on one connection, \
         talk to the <a href=\"#daemon\">daemon</a> socket instead — same envelopes, newline-delimited.",
    ));

    s.push_str(&h3("Request"));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"tasqx\":  \"1\",            // API major. Required.\n\
         \x20 \"id\":     \"e1\",           // Optional. Echoed back if present.\n\
         \x20 \"method\": \"task.list\",    // Required.\n\
         \x20 \"params\": { }              // Optional; defaults to {}.\n\
         }",
    ));

    s.push_str(&h3("Response"));
    s.push_str(&p("Success carries <code>result</code>; failure carries <code>error</code>. <code>ok</code> tells you which without inspecting further."));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"e1\",\"method\":\"task.list\",\"params\":{\"filter\":\"+api\",\"sort\":[\"-urgency\"]}}' | tasqx api",
        "{\"id\":\"e1\",\"ok\":true,\"result\":{\"count\":1,\"tasks\":[{\"_rev\":4,\"completed\":null,\"created\":\"2026-07-16T08:51:09.2509427Z\",\"due\":\"2026-07-17T00:00:00Z\",\"estimate\":\"PT4H\",\"id\":\"019f6a1f-6142-70d3-be5b-e28dc6060e6c\",\"modified\":\"2026-07-16T08:51:09.6830568Z\",\"priority\":\"H\",\"project\":\"work.tasqx\",\"recurrence\":null,\"remind\":null,\"scheduled\":null,\"short_id\":1,\"status\":\"pending\",\"tags\":[\"api\",\"release\"],\"title\":\"Ship the v1 JSON API freeze\",\"urgency\":17.5,\"wait\":null}]},\"tasqx\":\"1\"}",
    ));

    s.push_str(&h3("Errors"));
    s.push_str(&p(
        "An error is a value, not a crash. It carries a stable <code>code</code>, a human \
         <code>message</code>, and machine-readable <code>data</code> — and the CLI's exit codes are \
         these same codes.",
    ));
    s.push_str(&table(
        &["Code", "Exit", "Means"],
        &[
            &["<code>bad_request</code>", "2", "Malformed params, an unparseable date, contradictory input."],
            &["<code>not_found</code>", "4", "No such task/project/reference."],
            &["<code>conflict</code>", "5", "A lost <code>expected_rev</code> race, or a lifecycle rule."],
            &["<code>unsupported_version</code>", "—", "The <code>tasqx</code> major you sent is not this build's."],
            &["<code>internal</code>", "1", "A bug or an I/O failure. Should not happen."],
        ],
    ));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"e2\",\"method\":\"task.get\",\"params\":{\"ref\":\"999\"}}' | tasqx api",
        "{\"error\":{\"code\":\"not_found\",\"data\":{\"short_id\":999},\"message\":\"no task with short_id 999\"},\"id\":\"e2\",\"ok\":false,\"tasqx\":\"1\"}",
    ));
    s.push_str(&p("Version mismatches are caught before dispatch, and tell you what <em>is</em> supported:"));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"2\",\"id\":\"v1\",\"method\":\"task.list\"}' | tasqx api",
        "{\"error\":{\"code\":\"unsupported_version\",\"data\":{\"supported\":\"1\"},\"message\":\"unsupported api major version: 2\"},\"id\":\"v1\",\"ok\":false,\"tasqx\":\"1\"}",
    ));

    s.push_str(&h3("Feature detection"));
    s.push_str(&p(
        "Do not guess what a build supports — ask it. <code>core.capabilities</code> is the \
         handshake, and it also reports the current default project.",
    ));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"c1\",\"method\":\"core.capabilities\"}' | tasqx api",
        "{\"id\":\"c1\",\"ok\":true,\"result\":{\"api\":\"1\",\"default_project\":\"work.tasqx\",\"features\":[\"dependencies\",\"filter.boolean\",\"reminders\"],\"methods\":[\"project.create\",\"project.list\",\"project.archive\",\"task.add\",\"task.list\",\"task.get\",\"task.start\",\"task.stop\",\"task.done\",\"task.modify\",\"task.cancel\",\"task.reopen\",\"tag.add\",\"annotation.add\",\"dependency.add\",\"dependency.remove\",\"report.summary\",\"store.export\",\"store.import\",\"event.list\",\"reminder.fire\",\"core.capabilities\"]},\"tasqx\":\"1\"}",
    ));

    s.push_str(&h3("The methods"));
    s.push_str(&p(
        "All twenty-two — and this table is what the tests compare against \
         <code>core.capabilities</code>, so it cannot describe a method this build does not have.",
    ));
    let method_rows: Vec<Vec<String>> = METHODS
        .iter()
        .map(|(method, params, returns)| {
            vec![format!("<code>{method}</code>"), params.to_string(), returns.to_string()]
        })
        .collect();
    s.push_str(&table_owned(&["Method", "Params", "Returns"], &method_rows));

    s.push_str(&h3("Reading tasks without the API"));
    s.push_str(&p(
        "Any CLI command with <code>--json</code> prints the raw API result — the same bytes the \
         envelope's <code>result</code> would carry. That is usually the shortest path from a shell \
         script to structured data:",
    ));
    s.push_str(&snippet(
        "tasqx show 1 --json",
        "{\n\
         \x20 \"_rev\": 4,\n\
         \x20 \"annotations\": [\n\
         \x20   {\n\
         \x20     \"body\": \"Blocked on the D12 decision\",\n\
         \x20     \"created\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20     \"id\": \"019f6a1f-62f3-75f0-bf57-e5ff9c7c452a\"\n\
         \x20   }\n\
         \x20 ],\n\
         \x20 \"blocked\": false,\n\
         \x20 \"completed\": null,\n\
         \x20 \"created\": \"2026-07-16T08:51:09.2509427Z\",\n\
         \x20 \"depends_on\": [],\n\
         \x20 \"due\": \"2026-07-17T00:00:00Z\",\n\
         \x20 \"estimate\": \"PT4H\",\n\
         \x20 \"id\": \"019f6a1f-6142-70d3-be5b-e28dc6060e6c\",\n\
         \x20 \"modified\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20 \"priority\": \"H\",\n\
         \x20 \"project\": \"work.tasqx\",\n\
         \x20 \"recurrence\": null,\n\
         \x20 \"remind\": null,\n\
         \x20 \"scheduled\": null,\n\
         \x20 \"short_id\": 1,\n\
         \x20 \"status\": \"pending\",\n\
         \x20 \"tags\": [\n\
         \x20   \"api\",\n\
         \x20   \"release\"\n\
         \x20 ],\n\
         \x20 \"title\": \"Ship the v1 JSON API freeze\",\n\
         \x20 \"urgency\": 17.5,\n\
         \x20 \"wait\": null\n\
         }",
    ));

    s.push_str(&page_close("api"));
    s
}

// ============================================================================
// Page 10 — Export & import
// ============================================================================

fn page_data() -> String {
    let mut s = page_open("data", "Export and import");

    s.push_str(&lead(
        "Your data is yours. <code>export</code> emits canonical JSON — stable UUIDs, every field, \
         sorted keys — that is git-diffable, greppable, and round-trips exactly.",
    ));

    s.push_str(&h3("Export"));
    s.push_str(&p(
        "With no filter you get everything. With a <a href=\"#filters\">filter</a> you get a slice. \
         Human output <em>is</em> the JSON array — there is no separate pretty mode to drift from it.",
    ));
    s.push_str(&snippet(
        "tasqx export +api",
        "[\n\
         \x20 {\n\
         \x20   \"_rev\": 4,\n\
         \x20   \"annotations\": [\n\
         \x20     {\n\
         \x20       \"body\": \"Blocked on the D12 decision\",\n\
         \x20       \"created\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20       \"id\": \"019f6a1f-62f3-75f0-bf57-e5ff9c7c452a\"\n\
         \x20     }\n\
         \x20   ],\n\
         \x20   \"completed\": null,\n\
         \x20   \"created\": \"2026-07-16T08:51:09.2509427Z\",\n\
         \x20   \"depends_on\": [],\n\
         \x20   \"due\": \"2026-07-17T00:00:00Z\",\n\
         \x20   \"estimate\": \"PT4H\",\n\
         \x20   \"id\": \"019f6a1f-6142-70d3-be5b-e28dc6060e6c\",\n\
         \x20   \"modified\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20   \"priority\": \"H\",\n\
         \x20   \"project\": \"work.tasqx\",\n\
         \x20   \"recurrence\": null,\n\
         \x20   \"remind\": null,\n\
         \x20   \"scheduled\": null,\n\
         \x20   \"short_id\": 1,\n\
         \x20   \"status\": \"pending\",\n\
         \x20   \"tags\": [\n\
         \x20     \"api\",\n\
         \x20     \"release\"\n\
         \x20   ],\n\
         \x20   \"title\": \"Ship the v1 JSON API freeze\",\n\
         \x20   \"urgency\": 17.5,\n\
         \x20   \"wait\": null\n\
         \x20 }\n\
         ]",
    ));

    s.push_str(&h3("Filtered exports and dependency edges"));
    s.push_str(&p(
        "A filter selects a subset, so a dependency pointing <em>out</em> of that subset cannot \
         travel with it — the target is not in the document. Those edges are trimmed, and you are \
         told, on <strong>stderr</strong>:",
    ));
    s.push_str(&snippet(
        "tasqx export +docs > slice.json",
        "note: dropped 1 dependency edge(s) pointing outside the exported set; widen the filter to keep them",
    ));
    s.push_str(&note(
        "The note is on stderr <em>because</em> stdout is the JSON. A note there would corrupt \
         every pipe. <code>tasqx export +docs > slice.json</code> gives you a clean file and a \
         visible warning, both.",
    ));
    s.push_str(&p(
        "The <code>--json</code> form reports the same thing as data — <code>dropped_dependencies</code> \
         is always present, and is <code>0</code> for an unfiltered export:",
    ));
    s.push_str(&snippet(
        "tasqx export +api --json",
        "{\n  \"dropped_dependencies\": 0,\n  \"tasks\": [ ... ]\n}",
    ));

    s.push_str(&h3("Import"));
    s.push_str(&p(
        "Takes a file, or <code>-</code> for stdin. It accepts either a bare array (what \
         <code>export</code> prints) or a <code>{\"tasks\": [...]}</code> object. Import is an \
         <strong>upsert on the UUID</strong> — re-importing the same document is a no-op, not a \
         duplicate.",
    ));
    s.push_str(&snippet(
        "tasqx import slice.json",
        "Imported 2 task(s)",
    ));
    s.push_str(&snippet(
        "tasqx export +api | TASQX_DB=/tmp/other.db tasqx import -",
        "Imported 1 task(s)",
    ));

    s.push_str(&h3("A dangling edge is rejected, not repaired"));
    s.push_str(&p(
        "A dependency target must be in the payload <em>or</em> already in the store. Anything \
         else fails with <code>bad_request</code> naming the id — and because the whole import is \
         one transaction, a rejection writes <strong>nothing</strong>.",
    ));
    s.push_str(&pre_plain(
        "error [bad_request]: store.import: task 019f6a0f-99df-… depends on 019f6a0f-99b5-…,\n\
         \x20 which is neither in the payload nor in the store (export the dependency too, or drop the edge)",
    ));
    s.push_str(&p(
        "That is a deliberate choice. An edge to an unknown id means the wrong slice was \
         exported; repairing it quietly would hide exactly the mistake worth seeing. Payload \
         order does not matter — tasks are written first, edges second, so a forward reference \
         is fine.",
    ));

    s.push_str(&h3("Recipes"));
    s.push_str(&table(
        &["Want", "Do"],
        &[
            &["Back up", "<code>tasqx export > backup.json</code>"],
            &["Version your tasks in git", "<code>tasqx export > tasks.json &amp;&amp; git commit -am wip</code> — canonical output means clean diffs."],
            &["Move to another machine", "<code>tasqx export > all.json</code>, copy, <code>tasqx import all.json</code>"],
            &["Hand one project to a colleague", "<code>tasqx export project:work.tasqx > slice.json</code> — mind the edge note."],
            &["Query beyond the filter grammar", "<code>tasqx export | jq '[.[] | select(.urgency > 15)]'</code>"],
        ],
    ));

    s.push_str(&page_close("data"));
    s
}

// ============================================================================
// Page 11 — Themes & reports
// ============================================================================

fn page_themes() -> String {
    let mut s = page_open("themes", "Themes, charts and reports");

    s.push_str(&lead(
        "Default output should be something you want to look at. Themes drive the terminal and \
         the HTML report from the same palette, and degrade honestly when the terminal cannot \
         keep up.",
    ));

    s.push_str(&h3("Themes"));
    s.push_str(&p("Five built-ins. Resolution order: <code>--theme</code>, <code>$TASQX_THEME</code>, <code>config.toml</code>, default."));
    s.push_str(&snippet(
        "tasqx theme list",
        "Built-in themes\n  nord ← active\n  gruvbox\n  dracula\n  solarized\n  mono",
    ));
    s.push_str(&p(
        "<code>tasqx theme show [name]</code> previews every role plus the cold→hot urgency ramp, \
         rendered at your terminal's <em>real</em> capability. Set one permanently:",
    ));
    s.push_str(&pre_plain("# config.toml\n[theme]\nname = \"gruvbox\""));
    s.push_str(&p(
        "Drop a <code>.toml</code> in <code>$TASQX_CONFIG_DIR/themes/</code> and it appears in \
         <code>theme list</code> alongside the built-ins.",
    ));
    s.push_str(&note(
        "Capability is detected, not assumed. Pipe tasqx into <code>cat</code> and the colour goes \
         away; on a terminal without Unicode the block glyphs degrade to ASCII rather than \
         emitting mojibake. <code>mono</code> is there for when you want that unconditionally.",
    ));

    s.push_str(&h3("Reports"));
    s.push_str(&p(
        "<code>tasqx report [group_by] [filter] [--all]</code> — group by <code>project</code> (default), \
         <code>status</code>, or <code>priority</code>. Estimates total as ISO-8601 durations, which \
         is why <a href=\"#scheduling\"><code>est:</code></a> is parsed at the edge rather than stored \
         as opaque text.",
    ));
    s.push_str(&p(
        "<strong>What counts.</strong> A report is an aggregation, so it leaves <em>cancelled</em> \
         tasks out — tasqx has no hard delete, and without this every task you ever threw away \
         would inflate your totals forever. <em>Done</em> tasks still count: completed work is real \
         work, and it carries nearly all your tracked time. Two ways to override that: pass \
         <code>--all</code> to count everything including cancelled, or name a status in the filter \
         — <code>tasqx report status:cancelled</code> means what it says and is taken literally.",
    ));
    s.push_str(&snippet(
        "tasqx report",
        "PROJECT               COUNT         EST  OVERDUE     TRACKED\n\
         home                      1        PT0S        1        PT0S\n\
         work.tasqx                3     PT5H30M        1        PT0S",
    ));
    s.push_str(&snippet(
        "tasqx report status",
        "STATUS                COUNT         EST  OVERDUE     TRACKED\npending                   4     PT5H30M        2        PT0S",
    ));

    s.push_str(&h3("Charts"));
    s.push_str(&p(
        "All three read the append-only event log, so they are history, not a snapshot — and they \
         are pure reads that never touch your tasks.",
    ));
    s.push_str(&snippet(
        "tasqx chart throughput --weeks 12",
        "Weekly throughput   added [#]  done [#]\n\
         \x20 W27  added   0   done   0   net   0\n\
         \x20 W28  added   0   done   0   net   0\n\
         \x20 W29  added ##########  5   done ##  1   net  +4\n\
         \x20 > 4-wk velocity 0.2 done/wk - WIP trending up",
    ));
    s.push_str(&snippet(
        "tasqx chart heatmap --weeks 4",
        "Completions - last 4 weeks   . 0  : 1-2  + 3-4  # 5+\n\
         \x20 Mon . . . . \n\
         \x20     . . . . \n\
         \x20 Wed . . . . \n\
         \x20     . . . : \n\
         \x20 Fri . . . . \n\
         \x20     . . . . \n\
         \x20 Sun . . . . \n\
         \x20 > 1 done - current streak 1 days - best 1",
    ));
    s.push_str(&snippet(
        "tasqx chart burndown --days 7",
        "Remaining open - all tasks\n\
         \x20   4  ______#\n\
         \x20   0  2026-07-10 -> 2026-07-16\n\
         \x20 > 4 left - up 4 over 7 days - not burning down",
    ));
    s.push_str(&table(
        &["Chart", "Flags"],
        &[
            &["<code>throughput</code>", "<code>--weeks &lt;n&gt;</code> (default 12), <code>--weekly</code>"],
            &["<code>heatmap</code>", "<code>--weeks &lt;n&gt;</code> (default 12), <code>--year</code> (52 weeks)"],
            &["<code>burndown</code>", "<code>--days &lt;n&gt;</code> (default 30), <code>--project &lt;p&gt;</code>"],
        ],
    ));

    s.push_str(&h3("The HTML report"));
    s.push_str(&p(
        "<code>tasqx report --html</code> emits a weekly review as one self-contained file — inline \
         CSS, inline SVG charts, no external requests — themed from the same palette as your \
         terminal. Exactly like the page you are reading.",
    ));
    s.push_str(&snippet(
        "tasqx report --html --out review.html",
        "Wrote self-contained HTML report → review.html",
    ));
    s.push_str(&p(
        "Without <code>--out</code> it writes to stdout. Every panel is a pure read of the core API: \
         throughput, burndown, completed this week, overdue, per-project, now-actionable, top tags.",
    ));
    s.push_str(&note(
        "Both HTML surfaces hold the same line: no CDN, no web fonts, no remote images, no \
         scripts fetched from anywhere. Mail the file, commit it, open it on a plane — it renders \
         the same.",
    ));

    s.push_str(&page_close("themes"));
    s
}

// ============================================================================
// Small HTML builders — every caller-supplied string goes through `esc`
// ============================================================================

/// Open a page section. `title` is escaped; `id` is a literal from [`PAGES`].
fn page_open(id: &str, title: &str) -> String {
    format!("<section class=\"page\" id=\"{id}\"><h2>{}</h2>", esc(title))
}

/// Close a page, appending prev/next links derived from [`PAGES`].
fn page_close(id: &str) -> String {
    let idx = PAGES.iter().position(|(p, _, _)| *p == id);
    let mut links = String::new();
    if let Some(i) = idx {
        if i > 0 {
            let (pid, plabel, _) = PAGES[i - 1];
            links.push_str(&format!("<a class=\"prev\" href=\"#{pid}\">← {plabel}</a>"));
        }
        if i + 1 < PAGES.len() {
            let (nid, nlabel, _) = PAGES[i + 1];
            links.push_str(&format!("<a class=\"next\" href=\"#{nid}\">{nlabel} →</a>"));
        }
    }
    format!("<div class=\"pagenav\">{links}</div></section>")
}

/// A lead paragraph, emitted as trusted HTML so it can carry `<code>`/`<em>` like
/// the table cells already do. Every caller is a compile-time literal in this file
/// and this page renders no store data, so there is no untrusted input to escape.
/// Escaping here silently printed the tags as text instead. Note the contrast with
/// `html.rs`, which renders task titles and must keep escaping them.
fn lead(text: &str) -> String {
    format!("<p class=\"lead\">{text}</p>")
}

/// A section heading.
fn h3(text: &str) -> String {
    let anchor: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    format!("<h3 id=\"h-{anchor}\">{}</h3>", esc(text))
}

/// A prose paragraph. **Trusted markup**: the argument is a literal in this file,
/// never user input, so inline `<code>`/`<a>` are intentional. Nothing reaching
/// these builders comes from the store or from argv.
fn p(html: &str) -> String {
    format!("<p>{html}</p>")
}

fn note(html: &str) -> String {
    format!("<div class=\"callout note\"><span class=\"tag\">Note</span><p>{html}</p></div>")
}

fn warn(html: &str) -> String {
    format!("<div class=\"callout warn\"><span class=\"tag\">Careful</span><p>{html}</p></div>")
}

/// A command + its real output. Both are **escaped** — they are verbatim terminal
/// text, and an unescaped `<` in a snippet would be markup rather than a character.
fn snippet(cmd: &str, output: &str) -> String {
    let out = if output.is_empty() {
        String::new()
    } else {
        format!("<pre class=\"out\"><code>{}</code></pre>", esc(output))
    };
    format!(
        "<div class=\"snip\">\
           <div class=\"snip-h\"><span class=\"dollar\">$</span><button class=\"copy\" type=\"button\">Copy</button></div>\
           <pre class=\"cmd\"><code>{}</code></pre>{out}\
         </div>",
        esc(cmd),
    )
}

/// A preformatted block with no command line (grammar, config, JSON). Escaped.
fn pre_plain(text: &str) -> String {
    format!("<pre class=\"plain\"><code>{}</code></pre>", esc(text))
}

/// A table. Headers are escaped; cells are **trusted markup** (literals in this
/// file) so they can carry `<code>` and cross-page links.
fn table(headers: &[&str], rows: &[&[&str]]) -> String {
    let owned: Vec<Vec<String>> =
        rows.iter().map(|r| r.iter().map(|c| (*c).to_string()).collect()).collect();
    table_owned(headers, &owned)
}

/// [`table`] for rows built at runtime (the verb and method tables, generated
/// from `VERBS` / `METHODS`). Same contract: headers escaped, cells trusted.
fn table_owned(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut h = String::new();
    for x in headers {
        h.push_str(&format!("<th>{}</th>", esc(x)));
    }
    let mut b = String::new();
    for row in rows {
        b.push_str("<tr>");
        for cell in row {
            b.push_str(&format!("<td>{cell}</td>"));
        }
        b.push_str("</tr>");
    }
    // The wrapper is what scrolls, so a wide table never scrolls the page.
    format!(
        "<div class=\"tw\"><table class=\"grid\"><thead><tr>{h}</tr></thead><tbody>{b}</tbody></table></div>"
    )
}

// ============================================================================
// Inline CSS — light/dark, responsive, wide content scrolls in its own box
// ============================================================================

fn css() -> String {
    // A system-font stack: no web font can be requested, so none can be missing.
    String::from(
        ":root {\n\
         --accent: #5e81ac; --accent2: #88c0d0; --warn: #b58900; --danger: #bf616a;\n\
         --bg: #ffffff; --fg: #1a1d23; --muted: #6b7280; --card: #f6f7f9; --line: #e3e6ea;\n\
         --code-bg: #f2f4f7; --term-bg: #23262d; --term-fg: #d8dee9;\n\
         }\n\
         @media (prefers-color-scheme: dark) {\n\
         :root {\n\
         --accent: #88c0d0; --accent2: #81a1c1; --warn: #ebcb8b; --danger: #bf616a;\n\
         --bg: #22262e; --fg: #d8dee9; --muted: #8b93a3; --card: #2b3039; --line: #3a4150;\n\
         --code-bg: #2b3039; --term-bg: #1b1e24; --term-fg: #d8dee9;\n\
         }\n\
         }\n\
         * { box-sizing: border-box; }\n\
         html { scroll-behavior: smooth; }\n\
         body { margin: 0; background: var(--bg); color: var(--fg);\n\
         font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif;\n\
         line-height: 1.6; -webkit-text-size-adjust: 100%; }\n\
         code, pre, .mono { font-family: ui-monospace, \"Cascadia Code\", \"SF Mono\", Consolas, \"Liberation Mono\", monospace; }\n\
         a { color: var(--accent); text-decoration: none; }\n\
         a:hover { text-decoration: underline; }\n\
         .muted { color: var(--muted); }\n\
         \n\
         /* ---- top bar ---- */\n\
         header.top { position: sticky; top: 0; z-index: 20;\n\
         background: color-mix(in srgb, var(--bg) 92%, transparent); backdrop-filter: blur(8px);\n\
         border-bottom: 1px solid var(--line); padding: 0.8rem 1.25rem;\n\
         display: flex; align-items: center; gap: 1rem; }\n\
         .brand { font-weight: 700; font-size: 1.1rem; letter-spacing: -0.01em; }\n\
         .brand .muted { font-weight: 400; }\n\
         .ver { margin-left: auto; font-size: 0.8rem; }\n\
         #navtoggle { display: none; background: var(--card); color: var(--fg);\n\
         border: 1px solid var(--line); border-radius: 8px; padding: 0.3rem 0.7rem;\n\
         font: inherit; font-size: 0.85rem; cursor: pointer; }\n\
         \n\
         /* ---- layout ---- */\n\
         .shell { display: flex; align-items: flex-start; gap: 2rem;\n\
         max-width: 78rem; margin: 0 auto; padding: 0 1.25rem; }\n\
         nav { position: sticky; top: 4.2rem; flex: 0 0 15rem; padding: 1.5rem 0;\n\
         max-height: calc(100vh - 4.2rem); overflow-y: auto; }\n\
         nav a { display: block; padding: 0.4rem 0.7rem; border-radius: 7px;\n\
         color: var(--fg); font-size: 0.9rem; border-left: 2px solid transparent; }\n\
         nav a:hover { background: var(--card); text-decoration: none; }\n\
         nav a.active { background: var(--card); border-left-color: var(--accent);\n\
         color: var(--accent); font-weight: 600; }\n\
         main { flex: 1 1 auto; min-width: 0; padding: 1.5rem 0 4rem; }\n\
         \n\
         /* ---- pages: JS shows one; without JS everything renders ---- */\n\
         .js .page { display: none; }\n\
         .js .page.active { display: block; animation: fade 0.18s ease-out; }\n\
         @keyframes fade { from { opacity: 0; transform: translateY(3px); } to { opacity: 1; } }\n\
         @media (prefers-reduced-motion: reduce) {\n\
         html { scroll-behavior: auto; }\n\
         .js .page.active { animation: none; }\n\
         }\n\
         \n\
         /* ---- type ---- */\n\
         h2 { font-size: 1.65rem; letter-spacing: -0.02em; margin: 0 0 0.5rem; }\n\
         h3 { font-size: 1.05rem; letter-spacing: -0.01em; margin: 2.2rem 0 0.5rem;\n\
         padding-top: 0.4rem; border-top: 1px solid var(--line); }\n\
         p { margin: 0 0 0.9rem; }\n\
         p.lead { font-size: 1.08rem; color: var(--muted); margin-bottom: 1.4rem; }\n\
         p code, td code, li code { background: var(--code-bg); border: 1px solid var(--line);\n\
         border-radius: 5px; padding: 0.05em 0.35em; font-size: 0.86em; white-space: nowrap; }\n\
         \n\
         /* ---- snippets: the terminal look ---- */\n\
         .snip { margin: 0 0 1.1rem; border: 1px solid var(--line); border-radius: 10px;\n\
         overflow: hidden; background: var(--term-bg); }\n\
         .snip-h { display: flex; align-items: center; padding: 0.35rem 0.75rem;\n\
         border-bottom: 1px solid color-mix(in srgb, var(--term-fg) 15%, transparent); }\n\
         .snip-h .dollar { color: var(--accent2); font-family: ui-monospace, monospace;\n\
         font-size: 0.8rem; font-weight: 700; }\n\
         .copy { margin-left: auto; background: transparent; color: var(--term-fg);\n\
         border: 1px solid color-mix(in srgb, var(--term-fg) 25%, transparent);\n\
         border-radius: 6px; padding: 0.1rem 0.5rem; font: inherit; font-size: 0.72rem;\n\
         cursor: pointer; opacity: 0.7; }\n\
         .copy:hover { opacity: 1; }\n\
         /* Every wide block scrolls itself — the page never scrolls sideways. */\n\
         .snip pre { margin: 0; padding: 0.7rem 0.85rem; overflow-x: auto;\n\
         font-size: 0.82rem; line-height: 1.5; }\n\
         .snip pre.cmd { color: var(--accent2); font-weight: 600; }\n\
         .snip pre.out { color: var(--term-fg); opacity: 0.92;\n\
         border-top: 1px dashed color-mix(in srgb, var(--term-fg) 15%, transparent); }\n\
         pre.plain { background: var(--card); border: 1px solid var(--line); border-radius: 10px;\n\
         padding: 0.8rem 0.9rem; margin: 0 0 1.1rem; overflow-x: auto;\n\
         font-size: 0.82rem; line-height: 1.5; color: var(--fg); }\n\
         pre code { background: none; border: 0; padding: 0; white-space: pre; }\n\
         \n\
         /* ---- callouts ---- */\n\
         .callout { border: 1px solid var(--line); border-left-width: 3px; border-radius: 8px;\n\
         background: var(--card); padding: 0.75rem 0.9rem; margin: 0 0 1.1rem; }\n\
         .callout p { margin: 0.25rem 0 0; font-size: 0.92rem; }\n\
         .callout .tag { font-size: 0.68rem; font-weight: 700; text-transform: uppercase;\n\
         letter-spacing: 0.07em; }\n\
         .callout.note { border-left-color: var(--accent); }\n\
         .callout.note .tag { color: var(--accent); }\n\
         .callout.warn { border-left-color: var(--warn); }\n\
         .callout.warn .tag { color: var(--warn); }\n\
         \n\
         /* ---- tables: the wrapper scrolls, not the page ---- */\n\
         .tw { overflow-x: auto; margin: 0 0 1.1rem; border: 1px solid var(--line);\n\
         border-radius: 10px; }\n\
         table.grid { width: 100%; border-collapse: collapse; font-size: 0.88rem;\n\
         min-width: 26rem; }\n\
         table.grid th { text-align: left; color: var(--muted); font-weight: 600;\n\
         font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.05em;\n\
         background: var(--card); border-bottom: 1px solid var(--line);\n\
         padding: 0.5rem 0.7rem; white-space: nowrap; }\n\
         table.grid td { padding: 0.5rem 0.7rem; border-bottom: 1px solid var(--line);\n\
         vertical-align: top; }\n\
         table.grid tr:last-child td { border-bottom: 0; }\n\
         \n\
         /* ---- page nav ---- */\n\
         .pagenav { display: flex; gap: 1rem; margin-top: 2.5rem; padding-top: 1rem;\n\
         border-top: 1px solid var(--line); font-size: 0.9rem; }\n\
         .pagenav .next { margin-left: auto; }\n\
         footer { max-width: 78rem; margin: 0 auto; padding: 1.5rem 1.25rem 3rem;\n\
         border-top: 1px solid var(--line); color: var(--muted); font-size: 0.8rem; }\n\
         \n\
         /* ---- responsive ---- */\n\
         @media (max-width: 52rem) {\n\
         .shell { flex-direction: column; gap: 0; }\n\
         #navtoggle { display: block; }\n\
         nav { position: static; flex: none; width: 100%; max-height: none;\n\
         padding: 0.5rem 0; display: none; border-bottom: 1px solid var(--line); }\n\
         nav.open { display: block; }\n\
         main { padding-top: 1.25rem; }\n\
         h2 { font-size: 1.4rem; }\n\
         }\n",
    )
}

// ============================================================================
// Inline JS — client-side page switching, no framework, no external anything
// ============================================================================

fn js() -> String {
    // Written defensively: if anything here throws, the `js` class is never added
    // and the document degrades to one long readable page rather than a blank one.
    //
    // Navigation is driven by `location.hash` and the `hashchange` event, and NOT
    // by `history.pushState`. That is load-bearing, not stylistic: this file is
    // opened over `file://`, whose origin is `null`, and `pushState` throws a
    // SecurityError there. Letting each `<a href="#…">` do its ordinary default
    // thing sets the hash, fires `hashchange`, and gives us real history entries —
    // so the back button and deep links work on the exact transport `tasqx docs`
    // actually uses.
    String::from(
        "(function () {\n\
        \x20 var pages = Array.prototype.slice.call(document.querySelectorAll('.page'));\n\
        \x20 var links = Array.prototype.slice.call(document.querySelectorAll('nav a'));\n\
        \x20 var nav = document.getElementById('nav');\n\
        \x20 if (!pages.length) { return; }\n\
        \x20 // Only hide pages once we know we can show them again.\n\
        \x20 document.documentElement.classList.add('js');\n\
        \n\
        \x20 function show(id) {\n\
        \x20   var found = pages.some(function (p) { return p.id === id; });\n\
        \x20   if (!found) { id = pages[0].id; }\n\
        \x20   pages.forEach(function (p) { p.classList.toggle('active', p.id === id); });\n\
        \x20   links.forEach(function (a) { a.classList.toggle('active', a.dataset.page === id); });\n\
        \x20   if (nav) { nav.classList.remove('open'); }\n\
        \x20 }\n\
        \n\
        \x20 // The browser sets the hash for us; we only react. Nav links, prev/next,\n\
        \x20 // and cross-references inside prose therefore all take one path.\n\
        \x20 window.addEventListener('hashchange', function () {\n\
        \x20   show(location.hash.slice(1));\n\
        \x20   window.scrollTo(0, 0);\n\
        \x20 });\n\
        \n\
        \x20 // Clicking the page you are already on fires no hashchange; close the\n\
        \x20 // mobile nav anyway so the tap is not a no-op.\n\
        \x20 document.addEventListener('click', function (e) {\n\
        \x20   var a = e.target.closest ? e.target.closest('a[href^=\"#\"]') : null;\n\
        \x20   if (a && nav) { nav.classList.remove('open'); }\n\
        \x20 });\n\
        \n\
        \x20 if (nav) {\n\
        \x20   var tog = document.getElementById('navtoggle');\n\
        \x20   if (tog) {\n\
        \x20     tog.addEventListener('click', function (e) {\n\
        \x20       e.stopPropagation();\n\
        \x20       nav.classList.toggle('open');\n\
        \x20     });\n\
        \x20   }\n\
        \x20 }\n\
        \n\
        \x20 // Copy buttons: clipboard where available, a select-all fallback where not.\n\
        \x20 document.addEventListener('click', function (e) {\n\
        \x20   if (!e.target.classList || !e.target.classList.contains('copy')) { return; }\n\
        \x20   var box = e.target.closest('.snip');\n\
        \x20   var cmd = box && box.querySelector('pre.cmd code');\n\
        \x20   if (!cmd) { return; }\n\
        \x20   var btn = e.target;\n\
        \x20   function ok() { btn.textContent = 'Copied'; setTimeout(function () { btn.textContent = 'Copy'; }, 1200); }\n\
        \x20   if (navigator.clipboard && navigator.clipboard.writeText) {\n\
        \x20     navigator.clipboard.writeText(cmd.textContent).then(ok, function () {});\n\
        \x20   } else {\n\
        \x20     var r = document.createRange();\n\
        \x20     r.selectNodeContents(cmd);\n\
        \x20     var sel = window.getSelection();\n\
        \x20     sel.removeAllRanges();\n\
        \x20     sel.addRange(r);\n\
        \x20     ok();\n\
        \x20   }\n\
        \x20 });\n\
        \n\
        \x20 show(location.hash.slice(1) || pages[0].id);\n\
        }());",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    // ---- doc-drift guards ---------------------------------------------------
    //
    // The cheapest honest guard available: the docs render FROM these lists, and
    // each list is asserted equal to the real surface it claims to describe. A
    // new verb or method cannot ship undocumented, because the CLI's own tables
    // are the assertion.

    /// THE drift guard. `VERBS` is what the Commands page renders; clap's
    /// subcommand table is the truth. Adding a verb without documenting it, or
    /// documenting one that does not exist, fails here.
    #[test]
    fn documented_verbs_match_the_cli_surface() {
        let mut real: Vec<String> = crate::Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut documented: Vec<String> =
            documented_verbs().iter().map(|s| s.to_string()).collect();
        real.sort();
        documented.sort();

        let undocumented: Vec<&String> = real.iter().filter(|v| !documented.contains(v)).collect();
        assert!(
            undocumented.is_empty(),
            "these CLI verbs exist but `tasqx docs` does not document them: {undocumented:?}"
        );
        let invented: Vec<&String> = documented.iter().filter(|v| !real.contains(v)).collect();
        assert!(
            invented.is_empty(),
            "`tasqx docs` documents these verbs, but the CLI has no such subcommand: {invented:?}"
        );
        assert_eq!(real, documented);
    }

    /// The HTML verb table and the terminal registry may not disagree on the
    /// structural fields: verb/aliases/method are asserted equal here. Prose is
    /// no longer comparable because it is no longer duplicated — the page
    /// renders `cmddoc`'s summary directly (see [`VERBS`]).
    #[test]
    fn html_verbs_agree_with_cmddoc() {
        use crate::cmddoc::COMMAND_REF;
        for (verb, aliases_html, method) in VERBS {
            let d = COMMAND_REF.iter().find(|d| d.verb == verb)
                .unwrap_or_else(|| panic!("VERBS has `{verb}`, cmddoc does not"));
            assert_eq!(d.method, method, "method drift on `{verb}`");
            let mut html_aliases: Vec<String> = if aliases_html == "—" {
                vec![]
            } else {
                aliases_html.split(',')
                    .map(|a| a.trim().replace("<code>", "").replace("</code>", ""))
                    .collect()
            };
            let mut ours: Vec<String> = d.aliases.iter().map(|s| s.to_string()).collect();
            html_aliases.sort(); ours.sort();
            assert_eq!(html_aliases, ours, "alias drift (html vs cmddoc) on `{verb}`");
        }
        // reverse direction: every cmddoc verb (incl. `manual`) must be documented
        // in the HTML guide too.
        for d in COMMAND_REF {
            assert!(VERBS.iter().any(|(v, ..)| *v == d.verb),
                "cmddoc verb `{}` missing from the HTML VERBS table", d.verb);
        }
    }

    /// Aliases are part of the documented surface too: the table claims specific
    /// aliases per verb, and clap knows the real ones.
    #[test]
    fn documented_aliases_match_the_cli_surface() {
        let cmd = crate::Cli::command();
        for (verb, aliases, _) in VERBS {
            let sub = cmd
                .get_subcommands()
                .find(|c| c.get_name() == verb)
                .unwrap_or_else(|| panic!("no such subcommand: {verb}"));
            let mut real: Vec<String> =
                sub.get_all_aliases().map(|a| a.to_string()).collect();
            real.sort();

            // "—" is the table's way of saying "no aliases".
            let mut claimed: Vec<String> = if aliases == "—" {
                vec![]
            } else {
                aliases
                    .split(',')
                    .map(|a| a.trim().replace("<code>", "").replace("</code>", ""))
                    .collect()
            };
            claimed.sort();
            assert_eq!(real, claimed, "alias drift on verb `{verb}`");
        }
    }

    /// Every documented verb must actually reach the rendered page.
    #[test]
    fn every_documented_verb_appears_on_the_commands_page() {
        let doc = generate();
        for verb in documented_verbs() {
            assert!(
                doc.contains(&format!("<code>{verb}</code>")),
                "verb `{verb}` is in the VERBS table but never rendered onto the page"
            );
        }
    }

    /// Strip a `params`/`returns` cell down to the bare identifiers it names, so
    /// a test can compare the documented parameter names against the engine.
    /// `<code>x</code>, <code>y?</code>` → `["x", "y?"]`; the em-dash "no
    /// params" marker yields an empty list.
    #[cfg(test)]
    fn param_names(cell: &str) -> Vec<String> {
        cell.replace("<code>", "")
            .replace("</code>", "")
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && p != "—")
            .collect()
    }

    /// The `params` column claims which arguments each JSON API method takes and
    /// which of them are mandatory (no trailing `?`). Nothing checked either.
    ///
    /// Two real failures this guards, both silent today:
    ///
    /// 1. A row claims a **required** parameter the engine no longer demands.
    ///    Found exactly this on `task.list` (`filter`) and `report.summary`
    ///    (`group_by`) — both are optional in the engine and were documented as
    ///    required, sending readers hunting for an argument they do not need.
    /// 2. A row claims a required parameter under a **name the engine renamed**.
    ///    The engine's own "missing required field: X" message names the field
    ///    it wanted, so calling each method with `{}` and matching that message
    ///    against the documented name pins the spelling, not just the count.
    ///
    /// What this does NOT cover, stated plainly: the *optional* parameters
    /// (`project?`, `sort?`, `expected_rev?`, …) are unguarded. The engine reads
    /// them with `opt_str`-style lookups that succeed by ignoring anything they
    /// do not recognise, so a renamed optional param produces no error to
    /// observe — proving those names would mean a real call per parameter with a
    /// value round-tripped back out, which is an integration suite, not a
    /// doc-drift guard.
    #[test]
    fn documented_required_params_match_what_the_engine_demands() {
        let e = tasqx_core::Engine::open_in_memory().expect("in-memory store");

        for (method, params, _returns) in METHODS {
            let names = param_names(params);
            let required: Vec<&String> = names.iter().filter(|p| !p.ends_with('?')).collect();
            let outcome = tasqx_core::dispatch(&e, method, &serde_json::json!({}));

            match required.first() {
                // The table says every argument is optional. That is a checkable
                // claim: the method must succeed with no arguments at all.
                None => assert!(
                    outcome.is_ok(),
                    "`{method}` documents no required params, but rejects an empty call: {:?}",
                    outcome.err()
                ),
                // The table names a mandatory argument. The engine must refuse
                // the empty call *and* name that same argument when it does.
                Some(first) => {
                    let err = outcome.err().unwrap_or_else(|| {
                        panic!("`{method}` documents required param `{first}`, but an empty call succeeded")
                    });
                    assert!(
                        err.message.contains(first.as_str()),
                        "`{method}` documents required param `{first}`, but the engine's \
                         complaint names something else: {}",
                        err.message
                    );
                }
            }
        }
    }

    /// The `returns` column describes each method's response shape. Where it
    /// spells the shape as an explicit `{a, b}` brace list, those really must be
    /// top-level keys of the real response.
    ///
    /// Partial by construction, and worth naming precisely: this only covers the
    /// methods callable with no arguments, because those are the ones a
    /// doc-drift test can invoke without inventing fixture data. That is six of
    /// the twenty-three rows. The write methods' return shapes, and every prose
    /// `returns` cell that describes rather than enumerates ("The task, timer
    /// running."), stay unguarded — asserting on English is not a thing a test
    /// can do, and asserting on the write shapes needs a fixture store per
    /// method.
    ///
    /// The failure it does catch: a renamed response key. `{count, tasks}`
    /// becoming `{count, rows}` in the engine leaves the guide confidently
    /// telling every API client to read a field that is no longer there.
    #[test]
    fn documented_return_shapes_match_the_real_response_where_checkable() {
        let e = tasqx_core::Engine::open_in_memory().expect("in-memory store");
        let mut checked = 0;

        for (method, params, returns) in METHODS {
            // Only the no-required-params methods are callable here.
            if param_names(params).iter().any(|p| !p.ends_with('?')) {
                continue;
            }
            // Only the rows that enumerate a shape, e.g. "<code>{count, tasks}</code>".
            let Some(open) = returns.find('{') else { continue };
            let Some(close) = returns[open..].find('}') else { continue };
            let keys = param_names(&returns[open + 1..open + close]);
            if keys.is_empty() {
                continue;
            }

            let result = tasqx_core::dispatch(&e, method, &serde_json::json!({}))
                .unwrap_or_else(|err| panic!("`{method}` should be callable bare: {err:?}"));
            for key in &keys {
                assert!(
                    result.get(key).is_some(),
                    "the guide says `{method}` returns `{key}`, but the response has no such \
                     top-level key: {result}"
                );
            }
            checked += 1;
        }

        // Pin the coverage claim itself. If a future edit makes this loop skip
        // everything, the test would pass while guarding nothing.
        assert_eq!(
            checked, 6,
            "expected to check all 6 bare-callable return shapes; a row that stopped being \
             checkable is coverage lost silently"
        );
    }

    /// The Commands page's description column is rendered from `cmddoc`, not
    /// stored in [`VERBS`]. That single-sourcing is the whole point of deleting
    /// the old prose column, so it needs a guard of its own.
    ///
    /// The failure: [`verb_summary`] returns `""` for a verb `cmddoc` does not
    /// know, deliberately (the generator must not panic over a doc gap). Without
    /// this test that fallback is invisible — the page renders with a blank
    /// column and every other guard still passes, because they all check verb
    /// names and never the prose. This asserts the terminal's exact string
    /// reaches the page, which is only possible while there is one copy of it.
    #[test]
    fn the_commands_page_shows_the_same_summary_as_the_terminal() {
        let doc = generate();
        for verb in documented_verbs() {
            let summary = verb_summary(verb);
            assert!(
                !summary.is_empty(),
                "verb `{verb}` renders an empty description — cmddoc has no summary for it"
            );
            assert!(
                doc.contains(&esc(summary)),
                "verb `{verb}`'s -h summary ({summary:?}) never reaches the Commands page"
            );
        }
    }

    /// The API page's method table against the core's own capability report.
    #[test]
    fn documented_methods_match_core_capabilities() {
        let caps = tasqx_core::capabilities();
        let mut real: Vec<String> = caps["methods"]
            .as_array()
            .expect("capabilities.methods is an array")
            .iter()
            .map(|m| m.as_str().unwrap_or_default().to_string())
            .collect();
        let mut documented: Vec<String> =
            documented_methods().iter().map(|s| s.to_string()).collect();
        real.sort();
        documented.sort();
        assert_eq!(real, documented, "the JSON API page has drifted from core.capabilities");
    }

    /// Every documented method must reach the page too.
    #[test]
    fn every_documented_method_appears_on_the_api_page() {
        let doc = generate();
        for m in documented_methods() {
            assert!(
                doc.contains(&format!("<code>{m}</code>")),
                "method `{m}` is in the METHODS table but never rendered"
            );
        }
    }

    /// The verb table's method column must name a method the core actually has —
    /// the mapping in that column IS the contract the guide claims.
    #[test]
    fn verb_table_only_names_real_methods() {
        let methods = documented_methods();
        for (verb, _, method) in VERBS {
            // Verbs that frame their own transport have no single method.
            if method.starts_with('—') || method.starts_with('(') {
                continue;
            }
            let named = method.split(" + ").next().unwrap_or(method);
            assert!(
                methods.contains(&named),
                "verb `{verb}` claims method `{named}`, which core.capabilities does not list"
            );
        }
    }

    /// A smoke test on the settings section, and deliberately not more.
    ///
    /// Read this before trusting it: the table is GENERATED from
    /// `config::SETTINGS`, so both sides of the assertion come from one
    /// constant through one format string and a new setting can never be
    /// "missing". Proven by mutation — adding a fourth Setting with no guide
    /// entry leaves this green; deleting the `table_owned` call turns it red.
    ///
    /// That is the right state (generated docs cannot drift) but it makes this
    /// a check that the section still renders, NOT a coverage guard. An earlier
    /// version of this comment invoked the flags guard that "found eleven real
    /// gaps" as its peer, which presented a structurally impossible result as
    /// evidence of health. The real coverage guard for this area is
    /// `every_env_var_is_either_a_registered_setting_or_a_named_exception`,
    /// which is mutation-proven load-bearing.
    #[test]
    fn the_settings_section_renders_every_registered_setting() {
        let doc = generate();
        let missing: Vec<&str> = crate::config::SETTINGS
            .iter()
            .map(|s| s.key)
            .filter(|k| !doc.contains(&format!("<code>{k}</code>")))
            .collect();
        assert!(missing.is_empty(), "settings the guide never names: {missing:?}");
    }

    /// Every `TASQX_*` variable the code reads must be declared in SETTINGS or
    /// listed here as a deliberate exception. Without this, an env var that
    /// overrides behaviour can exist with nothing documenting it — which is
    /// exactly the state TASQX_FORCE_COLOR was in when this guard was written.
    #[test]
    fn every_env_var_is_either_a_registered_setting_or_a_named_exception() {
        // Not settings: these select a whole store/transport rather than tuning
        // behaviour, and giving them a config layer is a separate decision
        // (see the spec's "out of scope").
        const EXCEPTIONS: &[&str] = &[
            "TASQX_DB",
            "TASQX_SOCK",
            "TASQX_CONFIG_DIR",
            "TASQX_MCP_TOKEN",
            "TASQX_FORCE_COLOR",
            // Different in kind from the rest of this list: set by `build.rs`
            // and read by `env!` at compile time, so it is baked into the
            // binary and cannot be set by a user at all. A config layer for it
            // is not "out of scope" but meaningless.
            "TASQX_BUILD_ID",
        ];
        let sources = [
            include_str!("lib.rs"),
            include_str!("theme.rs"),
            include_str!("config.rs"),
        ];
        let mut found: Vec<String> = Vec::new();
        for src in sources {
            let mut rest = src;
            while let Some(i) = rest.find("TASQX_") {
                let tail = &rest[i..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                    .unwrap_or(tail.len());
                // `TASQX_` with nothing after it is prose, not a variable —
                // doc comments write `TASQX_*` and the `*` ends the scan.
                if end > "TASQX_".len() {
                    found.push(tail[..end].to_string());
                }
                rest = &tail[end..];
            }
        }
        found.sort();
        found.dedup();
        let registered: Vec<&str> = crate::config::SETTINGS.iter().filter_map(|s| s.env).collect();
        let orphans: Vec<&String> = found
            .iter()
            .filter(|v| !registered.contains(&v.as_str()) && !EXCEPTIONS.contains(&v.as_str()))
            .collect();
        assert!(orphans.is_empty(), "env vars with no setting and no exception: {orphans:?}");
    }

    /// `modify --clear` takes a closed set; the page prints it. If `main::CLEARABLE`
    /// gains a field, the docs must say so.
    #[test]
    fn documented_clear_fields_match_the_parser() {
        assert_eq!(
            crate::CLEARABLE.to_vec(),
            DOCUMENTED_CLEAR_FIELDS.to_vec(),
            "`modify --clear` accepts a different set than the docs claim"
        );
        let doc = generate();
        for f in DOCUMENTED_CLEAR_FIELDS {
            assert!(doc.contains(f), "clearable field `{f}` is not on the page");
        }
    }

    /// The Filters page must show the parser's grammar, not a copy of it.
    ///
    /// It used to show a copy, and the copy rotted exactly as copies do: it still
    /// claimed a tag took a bare word after quoted tags shipped, and it named a
    /// `WORD` symbol it never defined. Rendering the const removes the second
    /// copy; this asserts nobody quietly reintroduces one.
    #[test]
    fn the_filter_grammar_on_the_page_is_the_parsers_own() {
        let doc = generate();
        let rendered = crate::html::esc(tasqx_core::filter::GRAMMAR);
        assert!(
            doc.contains(&rendered),
            "the Filters page is not rendering `tasqx_core::filter::GRAMMAR` verbatim"
        );
    }

    // ---- self-containment ---------------------------------------------------

    /// The whole promise of the file: it opens anywhere, offline, forever. Any
    /// external reference — a CDN script, a web font, a remote image — breaks it.
    #[test]
    fn no_markup_leaks_into_the_page_as_visible_text() {
        // The self-containment and well-formedness guards both pass on an
        // over-escaped page — `&lt;code&gt;` is valid HTML, it just renders the tag
        // as literal text to the reader. Eleven leads shipped that way. Only a
        // check on the *rendered* text catches it.
        let doc = generate();
        // Only tag names that cannot double as a prose placeholder. `<p>` is out:
        // it is the value placeholder in `--project <p>`, and `<addr>`/`<ref>`/`<n>`
        // are the same kind of thing — those must STAY escaped.
        for tag in ["code", "em", "strong", "pre"] {
            let leaked = format!("&lt;{tag}&gt;");
            assert!(
                !doc.contains(&leaked),
                "escaped <{tag}> tag renders as literal text to the reader"
            );
        }
        assert!(!doc.contains("&lt;a href"), "escaped <a href> renders as literal text");
        assert!(doc.contains("&lt;addr&gt;"), "prose placeholders must stay escaped");
    }

    /// The filter page tells the reader which values `status:` accepts, and it
    /// listed four of the five — `backlog` was missing. That is not a drift risk,
    /// it was already wrong in shipped output: DESIGN.md defines `backlog` as a
    /// status and documents `tasqx ls status:backlog`, so the guide taught a
    /// reader there was no way to find work parked behind a future `wait:` or
    /// `scheduled:`. None of the twenty-odd docs guards enumerated statuses, so
    /// nothing noticed.
    ///
    /// Derived from `Status::ALL` rather than restating the names: a guard that
    /// hand-lists what it checks is the same parallel list it exists to police.
    #[test]
    fn the_filter_page_documents_every_status_value() {
        let doc = generate();
        let missing: Vec<&str> = tasqx_core::types::Status::ALL
            .into_iter()
            .map(|s| s.as_str())
            .filter(|name| !doc.contains(&format!("<code>{name}</code>")))
            .collect();
        assert!(
            missing.is_empty(),
            "statuses a user can filter on but the guide never names: {missing:?}"
        );
    }

    /// A reader who cannot see D24 in the guide has no way to explain a report
    /// count that looks too low — the tasks are still in the store, still listed
    /// by `tasqx list`, just absent from the roll-up. The VERBS/METHODS drift
    /// guards cannot catch this: the rule is prose, not a table they render from.
    #[test]
    fn reports_section_states_which_statuses_count() {
        let doc = generate();
        let reports = doc
            .split(&h3("Reports"))
            .nth(1)
            .expect("a Reports section")
            .split("<h3")
            .next()
            .unwrap();
        assert!(
            reports.contains("cancelled"),
            "the reports section must name the excluded status: {reports}"
        );
        assert!(
            reports.contains("--all"),
            "and the escape hatch from it: {reports}"
        );
    }

    /// The store path the guide prints must be the one the binary actually opens.
    /// It said `%APPDATA%\tasqx\tasks.db` while `db_path()` opens
    /// `%APPDATA%\tasqx\tasqx\data\tasks.db` — a reader following the page looked
    /// for their data in a directory that does not exist. The VERBS/METHODS drift
    /// guards cannot see this: a path is prose, not a table they render from.
    #[cfg(windows)]
    #[test]
    fn documented_store_path_matches_the_real_one() {
        let dirs = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
            .expect("a data dir on this platform");
        let real = dirs.data_dir().join("tasks.db");
        // Compare the tail below %APPDATA%, which is the part the page spells out.
        let tail: Vec<_> = real
            .components()
            .rev()
            .take(4)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\\");
        assert_eq!(tail, "tasqx\\tasqx\\data\\tasks.db", "db_path()'s shape changed");

        // The page holds the path as ordinary text (the `\\` in the source literal
        // is one backslash at runtime), so a plain substring check is the whole test.
        let doc = generate();
        assert!(
            doc.contains(&tail),
            "the guide does not print the real store path ({tail})"
        );
    }

    #[test]
    fn docs_page_is_self_contained() {
        let doc = generate();
        assert!(!doc.contains("http://"), "contains an http:// reference");
        assert!(!doc.contains("https://"), "contains an https:// reference");
        assert!(!doc.contains("src="), "contains src= (an external asset)");
        assert!(!doc.contains("@import"), "contains a CSS @import");
        assert!(!doc.contains("url("), "contains a CSS url() reference");
        assert!(!doc.contains("//fonts."), "contains a font host");
        assert!(!doc.contains("integrity="), "contains an SRI attr — implies a CDN");
        assert!(!doc.contains("<link"), "contains a <link> (stylesheet/font/icon)");
        assert!(!doc.contains("<iframe"), "contains an <iframe>");
    }

    /// Every `href` must be an in-page anchor. This is the rule `report --html`
    /// enforces by having no links at all; the guide has links, so it has to check
    /// them one by one instead.
    #[test]
    fn every_link_is_an_internal_anchor() {
        let doc = generate();
        for (i, _) in doc.match_indices("href=\"") {
            let rest = &doc[i + 6..];
            let end = rest.find('"').expect("an href must be terminated");
            let target = &rest[..end];
            assert!(
                target.starts_with('#'),
                "external link `{target}` — the guide must not reference anything off-file"
            );
            assert!(target.len() > 1, "empty anchor href");
        }
    }

    /// Exactly one well-formed document, with its CSS and JS inline.
    #[test]
    fn docs_page_is_one_well_formed_document() {
        let doc = generate();
        assert!(doc.starts_with("<!doctype html>"));
        assert!(doc.trim_end().ends_with("</html>"));
        assert_eq!(doc.matches("<html").count(), 1);
        assert_eq!(doc.matches("</html>").count(), 1);
        assert_eq!(doc.matches("<style>").count(), 1, "CSS is inline, once");
        assert_eq!(doc.matches("<script>").count(), 1, "JS is inline, once");
        assert_eq!(doc.matches("<script").count(), 1, "no second script tag");
    }

    /// Regression, found by driving the real page over `file://`: navigation used
    /// `history.pushState`, which throws a **SecurityError** on a `file://`
    /// document (its origin is `null`) — the exact transport `tasqx docs` opens.
    /// The symptom was quiet and nasty: the page still switched (the toggles ran
    /// first), so it *looked* fine, but the URL never updated, which meant no deep
    /// links and no back button — the one thing anchors are for. Navigation is
    /// hash-driven now; nothing here may reach for the History API again.
    #[test]
    fn navigation_does_not_use_the_history_api() {
        let doc = generate();
        assert!(
            !doc.contains("pushState"),
            "pushState throws a SecurityError on file://, which is how `tasqx docs` opens the guide"
        );
        assert!(!doc.contains("replaceState"), "replaceState throws on file:// for the same reason");
        assert!(
            doc.contains("addEventListener('hashchange'"),
            "navigation must be driven by hashchange, the only mechanism that works on file://"
        );
    }

    /// Both colour schemes, same as the report.
    #[test]
    fn docs_page_has_both_color_schemes() {
        let doc = generate();
        assert!(doc.contains(":root {"), "light scheme root vars");
        assert!(doc.contains("@media (prefers-color-scheme: dark)"), "dark scheme");
        assert!(doc.matches("--bg:").count() >= 2, "--bg for both schemes");
    }

    /// Every page in the nav exists as a section, and vice versa — a nav link to a
    /// missing page is a dead end the reader finds before we do.
    #[test]
    fn every_nav_link_has_a_page() {
        let doc = generate();
        for (id, _, _) in PAGES {
            assert!(doc.contains(&format!("id=\"{id}\"")), "no section for nav page `{id}`");
            assert!(doc.contains(&format!("href=\"#{id}\"")), "no nav link to page `{id}`");
        }
        assert_eq!(
            doc.matches("class=\"page\"").count(),
            PAGES.len(),
            "the number of rendered pages does not match PAGES"
        );
    }

    /// Wide content must scroll inside its own box. Both mechanisms must be present
    /// or a long table drags the whole page sideways on a phone.
    #[test]
    fn wide_content_scrolls_in_its_own_container() {
        let doc = generate();
        assert!(doc.contains(".tw { overflow-x: auto;"), "table wrapper scrolls");
        assert!(doc.contains("overflow-x: auto"), "pre blocks scroll");
        // Every table is wrapped in the scroller.
        assert_eq!(
            doc.matches("<table class=\"grid\">").count(),
            doc.matches("<div class=\"tw\">").count(),
            "a table is not wrapped in a scroll container"
        );
    }

    /// The guide is escaped by the same rule as the report: a `<` in terminal
    /// output is a character, never markup.
    #[test]
    fn snippet_output_is_escaped() {
        let snip = snippet("tasqx list \"a<b\"", "<script>alert(1)</script> & done");
        assert!(!snip.contains("<script>"), "raw markup survived into a snippet");
        assert!(snip.contains("&lt;script&gt;"));
        assert!(snip.contains("&amp; done"));
        assert!(snip.contains("a&lt;b"));
    }

    /// The page must be substantial — a guard against a refactor quietly rendering
    /// an empty shell that still passes every structural assertion above.
    /// Pull every `snippet()` block out of a page as (command, output) pairs.
    /// The page is the only source of truth here — the test reads what a reader
    /// reads, not some parallel list an author has to remember to update.
    fn snippets_of(page: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for chunk in page.split("<div class=\"snip\">").skip(1) {
            let Some(c0) = chunk.find("<pre class=\"cmd\"><code>") else { continue };
            let c0 = c0 + "<pre class=\"cmd\"><code>".len();
            let Some(c1) = chunk[c0..].find("</code></pre>") else { continue };
            let cmd = unesc(&chunk[c0..c0 + c1]);
            let body = match chunk.find("<pre class=\"out\"><code>") {
                Some(o0) => {
                    let o0 = o0 + "<pre class=\"out\"><code>".len();
                    match chunk[o0..].find("</code></pre>") {
                        Some(o1) => unesc(&chunk[o0..o0 + o1]),
                        None => String::new(),
                    }
                }
                None => String::new(),
            };
            out.push((cmd, body));
        }
        out
    }

    /// Inverse of `esc` for the entities `esc` emits — enough to compare text.
    fn unesc(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// The quickstart's ids must be the ids a reader actually gets. short_ids are
    /// assigned 1,2,3… in creation order, so the Nth `add` on the page is #N — and
    /// the page claimed the 2nd add printed "Added #3", because the blocks had been
    /// captured against a store seeded with extra out-of-band tasks. A reader
    /// following along desynchronised at step three: every later `why 1` / `done 4`
    /// / `dep 2 1` on the page pointed at something else in their store.
    #[test]
    fn quickstart_add_ids_are_the_ids_a_reader_gets() {
        let adds: Vec<(String, String)> = snippets_of(&page_install())
            .into_iter()
            .filter(|(cmd, _)| cmd.starts_with("tasqx add "))
            .collect();
        assert!(adds.len() >= 2, "the quickstart must actually add tasks");
        for (i, (cmd, out)) in adds.iter().enumerate() {
            let expected = format!("Added #{}", i + 1);
            assert!(
                out.starts_with(&expected),
                "quickstart add #{} ({cmd:?}) shows {:?} — short_ids are handed out in \
                 creation order, so it must show {expected:?}",
                i + 1,
                out.lines().next().unwrap_or("")
            );
        }
    }

    /// Nothing may appear in a quickstart output block that no quickstart command
    /// creates. The working-set table used to list "Write the user guide", a task
    /// added by no command on the page — a ghost row inherited from the scratch
    /// store the blocks were captured against, which also made the row count wrong.
    #[test]
    fn quickstart_output_never_shows_a_task_no_command_creates() {
        let snips = snippets_of(&page_install());
        // Titles the page creates: the second line of each `add` block echoes the
        // stored title back.
        let created: Vec<String> = snips
            .iter()
            .filter(|(cmd, _)| cmd.starts_with("tasqx add "))
            .filter_map(|(_, out)| out.lines().nth(1).map(|l| l.trim().to_string()))
            .collect();
        assert!(!created.is_empty(), "no add blocks found — did the page change shape?");

        // Rows of the bare-`tasqx` working-set table.
        let (_, table) = snips
            .iter()
            .find(|(cmd, _)| cmd.trim() == "tasqx")
            .expect("the quickstart must show the working set");
        let rows: Vec<&str> = table
            .lines()
            .filter(|l| l.starts_with("   ") && l.trim().chars().next().is_some_and(|c| c.is_ascii_digit()))
            .collect();

        for row in &rows {
            let named = created.iter().any(|t| row.contains(t.as_str()));
            assert!(named, "working-set row {row:?} shows a task no documented command creates");
        }
        // …and the count line must agree with the rows shown.
        assert!(
            table.contains(&format!("{} task(s)", rows.len())),
            "the table shows {} rows but its count line disagrees:\n{table}",
            rows.len()
        );
        assert_eq!(rows.len(), created.len(), "every created task should be in the working set");
    }


    /// D23: a documented command that *files* a task into a project may only
    /// name a project a documented `init` creates. The guide is one narrative
    /// against one store, so this reads every page's commands, not just the
    /// quickstart's.
    ///
    /// The blocks were captured when `project:` was free-form, so
    /// `project:home` appeared on two pages with no `init home` anywhere — a
    /// command that, under D23, exits 4 for any reader who types it. The page
    /// claims nothing on it is illustrative; this is the same class of rot D20's
    /// guards exist for, one rule later.
    #[test]
    fn every_documented_project_is_one_a_documented_init_creates() {
        let doc = generate();
        let snips = snippets_of(&doc);
        let commands: Vec<String> =
            snips.iter().flat_map(|(cmd, _)| cmd.lines()).map(|l| l.trim().to_string()).collect();

        // Projects the page creates: `tasqx init <name> [--desc ...]`.
        let created: Vec<String> = commands
            .iter()
            .filter_map(|c| c.strip_prefix("tasqx init "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_string)
            .collect();
        assert!(!created.is_empty(), "the guide must create a project somewhere");

        // Only the verbs that WRITE a project have to name a live one: `list`,
        // `export` and friends take a filter, and a filter may name anything.
        let writes: Vec<&String> = commands
            .iter()
            .filter(|c| {
                ["add", "a", "new", "modify", "mod", "m", "edit"]
                    .iter()
                    .any(|v| c.starts_with(&format!("tasqx {v} ")))
            })
            .collect();
        assert!(!writes.is_empty(), "the guide must show an add — did the page change shape?");

        let clean = |s: &str| s.trim_matches(|c| c == '"' || c == '\\' || c == '\'').to_string();
        let mut named: Vec<String> = Vec::new();
        for c in &writes {
            for tok in c.split_whitespace() {
                let tok = tok.trim_start_matches(['"', '\\']);
                if let Some(p) = tok.strip_prefix("project:").or_else(|| tok.strip_prefix("proj:")) {
                    named.push(clean(p));
                }
            }
            let mut it = c.split_whitespace();
            while let Some(tok) = it.next() {
                if tok == "--project" {
                    if let Some(p) = it.next() {
                        named.push(clean(p));
                    }
                }
            }
        }
        assert!(!named.is_empty(), "the guide must show a project: on an add — did the sugar change?");

        for p in &named {
            assert!(
                created.contains(p),
                "a documented command files a task into project {p:?}, which no documented \
                 `tasqx init` creates — under D23 that command exits 4 for the reader. \
                 Documented inits: {created:?}"
            );
        }
    }

    #[test]
    fn docs_page_has_real_content() {
        let doc = generate();
        assert!(doc.len() > 40_000, "the guide is suspiciously small: {} bytes", doc.len());
        assert!(doc.matches("class=\"snip\"").count() >= 25, "too few worked examples");
    }
}
