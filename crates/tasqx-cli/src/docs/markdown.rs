//! The wiki and the guides, compiled in and rendered as pages of the guide.
//!
//! `docs/wiki` and `docs/guides` are good prose that used to exist only on the
//! repository's web page. They are the same thing the rest of [`super`] is —
//! pages of one self-contained HTML file — so they are `include_str!`d here and
//! rendered at *generation* time, rather than copied into Rust string literals.
//! One artifact, one source: editing a wiki page changes the site, and there is
//! no second copy for the two to drift apart on.
//!
//! **The events are post-processed, not the HTML.** `pulldown-cmark` parses, but
//! what it would emit — `<h2>`, `<table>`, `<pre><code class="language-console">`
//! — is not this site's markup. The event stream is rewritten first, so a fence
//! becomes the site's `.snip` terminal block, a table its `table.grid` inside the
//! `.tw` scroller, and a heading an `h3`/`h4` with an id. Working on the event
//! stream rather than the produced string means no regex over HTML and no
//! second escaper: text still goes through the renderer's own escaping.
//!
//! **Every link is rewritten or the build stops.** A wiki page links to
//! `Memory.md` and `Finding-Tasks.md#tasqx-why`, which are file paths that mean
//! nothing inside a single HTML document. [`resolve`] turns each one into the
//! hash route of the page it became; a relative link naming neither a page nor a
//! known repository file is a **panic at generation time**, naming the file and
//! the link. The alternative — emitting it and hoping — is a dead link that only
//! a reader finds. External `http(s)` links cannot survive as links at all: the
//! file promises to reference nothing off itself, and `every_link_is_an_internal_anchor`
//! enforces it. They render as their own text with the URL in a `<code>` beside
//! it, which is what a reader needs and what a copy of the file keeps working.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::html::esc;

/// The wiki, in reading order — the order the sidebar lists them under
/// "Using tasqx" and the order prev/next walks.
///
/// Literal paths, one per file, because `include_str!` takes a literal: there
/// is no glob that could quietly stop covering a file. The guard
/// [`tests::every_markdown_file_is_embedded_or_deliberately_not`] lists the
/// directory at test time and fails on anything here that is not on disk, or on
/// disk and not here.
const WIKI: &[(&str, &str)] = &[
    (
        "Getting-Started.md",
        include_str!("../../../../docs/wiki/Getting-Started.md"),
    ),
    (
        "Projects.md",
        include_str!("../../../../docs/wiki/Projects.md"),
    ),
    (
        "Adding-and-Editing-Tasks.md",
        include_str!("../../../../docs/wiki/Adding-and-Editing-Tasks.md"),
    ),
    (
        "Finding-Tasks.md",
        include_str!("../../../../docs/wiki/Finding-Tasks.md"),
    ),
    (
        "Working-on-Tasks.md",
        include_str!("../../../../docs/wiki/Working-on-Tasks.md"),
    ),
    (
        "Dependencies.md",
        include_str!("../../../../docs/wiki/Dependencies.md"),
    ),
    (
        "Dates-Reminders-and-Recurrence.md",
        include_str!("../../../../docs/wiki/Dates-Reminders-and-Recurrence.md"),
    ),
    (
        "Dashboard-and-Live-View.md",
        include_str!("../../../../docs/wiki/Dashboard-and-Live-View.md"),
    ),
    (
        "Reports-and-Charts.md",
        include_str!("../../../../docs/wiki/Reports-and-Charts.md"),
    ),
    ("Memory.md", include_str!("../../../../docs/wiki/Memory.md")),
    (
        "AI-Agents-and-Automation.md",
        include_str!("../../../../docs/wiki/AI-Agents-and-Automation.md"),
    ),
    (
        "Import-and-Export.md",
        include_str!("../../../../docs/wiki/Import-and-Export.md"),
    ),
    (
        "Settings-and-Themes.md",
        include_str!("../../../../docs/wiki/Settings-and-Themes.md"),
    ),
    (
        "Shell-Completion.md",
        include_str!("../../../../docs/wiki/Shell-Completion.md"),
    ),
];

/// The guides, in reading order: the ones a person runs first, then the ones
/// about driving tasqx from an agent.
const GUIDES: &[(&str, &str)] = &[
    (
        "feature-development.md",
        include_str!("../../../../docs/guides/feature-development.md"),
    ),
    (
        "personal-gtd.md",
        include_str!("../../../../docs/guides/personal-gtd.md"),
    ),
    (
        "standup-reporting.md",
        include_str!("../../../../docs/guides/standup-reporting.md"),
    ),
    (
        "ai-agent-workflow.md",
        include_str!("../../../../docs/guides/ai-agent-workflow.md"),
    ),
    (
        "agent-starter-prompt.md",
        include_str!("../../../../docs/guides/agent-starter-prompt.md"),
    ),
    (
        "self-improving-agent.md",
        include_str!("../../../../docs/guides/self-improving-agent.md"),
    ),
    (
        "token-accounting.md",
        include_str!("../../../../docs/guides/token-accounting.md"),
    ),
];

/// Markdown files that exist and are deliberately **not** pages, with the
/// reason. Named rather than skipped silently, so the guard below still fails
/// the day a new file lands with nobody having decided anything about it.
const NOT_A_PAGE: &[(&str, &str)] = &[(
    "Home.md",
    "The wiki's front page is a table of contents for a site that has no \
     sidebar: forty rows of `command → page` links, plus four paragraphs \
     (task references, `--json`, exit codes, nothing is destroyed) that the \
     Commands page already documents from the code rather than from prose. \
     Here the sidebar lists every page and the reference pages are generated, \
     so rendering it would be a second, hand-maintained index of the same \
     guide — the drift this file exists to refuse.",
)];

/// A page of the guide whose body came from a markdown file.
pub(super) struct MdPage {
    /// The hash route: `wiki-finding-tasks`, `guide-token-accounting`.
    pub(super) id: String,
    /// The [`super::SECTIONS`] entry it is filed under.
    pub(super) section: &'static str,
    /// The file's `# ` heading, raw (the caller escapes it).
    pub(super) title: String,
    /// The rendered body, **without** the title: [`super::page_open`] emits the
    /// `h2`, so a page has exactly one.
    pub(super) body: String,
}

/// Every markdown page, rendered once.
///
/// Rendering parses every embedded file, and does it once per process rather
/// than once per [`super::PAGES`] lookup — `page_open` and the sidebar both walk the
/// table, and `generate` is called by three tests on top of the binary.
pub(super) static PAGES: LazyLock<Vec<MdPage>> = LazyLock::new(build);

fn build() -> Vec<MdPage> {
    let ids = id_map();
    let mut out = Vec::with_capacity(WIKI.len() + GUIDES.len());
    for (section, prefix, table) in [("using", "wiki", WIKI), ("guides", "guide", GUIDES)] {
        for (file, src) in table {
            let id = page_id(prefix, file);
            let (title, body) = render(&id, file, src, &ids);
            out.push(MdPage {
                id,
                section,
                title,
                body,
            });
        }
    }
    out
}

/// File name → the id of the page it became. Built before any rendering,
/// because a page links forward as often as back.
fn id_map() -> BTreeMap<&'static str, String> {
    let mut map = BTreeMap::new();
    for (prefix, table) in [("wiki", WIKI), ("guide", GUIDES)] {
        for (file, _) in table {
            map.insert(*file, page_id(prefix, file));
        }
    }
    map
}

/// `("wiki", "Finding-Tasks.md")` → `wiki-finding-tasks`.
fn page_id(prefix: &str, file: &str) -> String {
    let stem = file
        .strip_suffix(".md")
        .unwrap_or_else(|| panic!("`{file}` is not a markdown file"));
    format!("{prefix}-{}", stem.to_ascii_lowercase())
}

// ============================================================================
// Rendering
// ============================================================================

/// Render one file: its `# ` title, and the body HTML under it.
fn render(
    page_id: &str,
    file: &str,
    src: &str,
    ids: &BTreeMap<&'static str, String>,
) -> (String, String) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    let events: Vec<Event> = Parser::new_ext(src, opts).collect();

    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    let mut title: Option<String> = None;
    // The `# ` heading is the page title and is dropped from the body; while
    // this is set, inline text is collected into `title` instead of emitted.
    let mut in_title = false;
    let mut heading_tags: Vec<&'static str> = Vec::new();
    // What to emit when a link closes: `</a>` for a rewritten one, the URL for
    // an external one, nothing for a link that became plain text.
    let mut link_closers: Vec<String> = Vec::new();

    let mut i = 0;
    while i < events.len() {
        // The title is collected, not emitted: everything between `# ` and its
        // end belongs to the page's `h2`, which `page_open` draws.
        if in_title {
            match &events[i] {
                Event::Text(t) | Event::Code(t) => {
                    title.as_mut().expect("a title is open").push_str(t);
                }
                Event::End(TagEnd::Heading(_)) => in_title = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match &events[i] {
            Event::Start(Tag::Heading { level, .. }) => {
                if *level == HeadingLevel::H1 {
                    assert!(
                        title.is_none(),
                        "{file}: a second `# ` heading — the first one is the page title, \
                         so the rest of the file must start at `## `"
                    );
                    title = Some(String::new());
                    in_title = true;
                } else {
                    let end = heading_end(&events, i, file);
                    let text = inline_text(&events[i + 1..end]);
                    let anchor = format!("{page_id}--{}", gh_slug(&text));
                    let tag = shifted(*level);
                    // `unique` is applied to the whole id, not the slug, so two
                    // pages may both have a "tasqx why" heading.
                    let anchor = unique(anchor, &out);
                    out.push(html(format!("<{tag} id=\"{anchor}\">")));
                    heading_tags.push(tag);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                let tag = heading_tags.pop().expect("headings are balanced");
                out.push(html(format!("</{tag}>")));
            }

            Event::Start(Tag::Link { dest_url, .. }) => match resolve(file, page_id, dest_url, ids)
            {
                Target::Anchor(href) => {
                    out.push(html(format!("<a href=\"{href}\">")));
                    link_closers.push("</a>".to_string());
                }
                Target::External(url) => {
                    link_closers.push(format!(" (<code>{}</code>)", esc(&url)));
                }
                Target::Text => link_closers.push(String::new()),
            },
            Event::End(TagEnd::Link) => {
                let close = link_closers.pop().expect("links are balanced");
                if !close.is_empty() {
                    out.push(html(close));
                }
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split(',').next().unwrap_or("").trim().to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                let (text, next) = code_text(&events, i, file);
                out.push(html(code_block(&lang, &text)));
                i = next;
                continue;
            }

            // The site's table: the wrapper is what scrolls, so a wide table
            // never takes the page sideways. Only the outer tags are replaced —
            // the rows keep the renderer's own escaping.
            Event::Start(Tag::Table(_)) => {
                out.push(html("<div class=\"tw\"><table class=\"grid\">".to_string()));
            }
            Event::End(TagEnd::Table) => {
                out.push(html("</tbody></table></div>".to_string()));
            }

            Event::Start(Tag::BlockQuote(_)) => {
                let (kind, tag, next) = callout_kind(&events, i);
                out.push(html(callout_open(kind, tag)));
                if next > i + 1 {
                    // The `**Note**` marker was consumed; the paragraph that
                    // carried it is the callout's own first paragraph.
                    out.push(Event::Start(Tag::Paragraph));
                    if let Some(Event::Text(t)) = events.get(next) {
                        out.push(Event::Text(CowStr::from(t.trim_start().to_string())));
                        i = next + 1;
                        continue;
                    }
                }
                i = next;
                continue;
            }
            Event::End(TagEnd::BlockQuote(_)) => out.push(html("</div>".to_string())),

            Event::Start(Tag::Image { dest_url, .. }) => panic!(
                "{file}: the image `{dest_url}` cannot be rendered — the guide is one file \
                 with no external requests, so it carries no images. Capture the terminal \
                 into a code fence instead."
            ),

            // Raw HTML in a source file is shown, not run: this document is
            // assembled from trusted builders and a markdown file is the one
            // input that a hand could put a `<script>` in.
            Event::Html(s) | Event::InlineHtml(s) => out.push(Event::Text(s.clone())),

            other => out.push(other.clone()),
        }
        i += 1;
    }

    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, out.into_iter());
    let title = title.unwrap_or_else(|| {
        panic!("{file}: no `# ` heading — the page title and its sidebar label come from it")
    });
    (title, body)
}

/// An already-built fragment of the site's own markup, handed to the renderer
/// as-is. Everything reaching this is built by [`super`]'s escaping helpers.
fn html(s: String) -> Event<'static> {
    Event::Html(CowStr::from(s))
}

/// The index of the `End(Heading)` that closes the heading opened at `i`.
fn heading_end(events: &[Event], i: usize, file: &str) -> usize {
    events[i + 1..]
        .iter()
        .position(|e| matches!(e, Event::End(TagEnd::Heading(_))))
        .map(|p| i + 1 + p)
        .unwrap_or_else(|| panic!("{file}: an unterminated heading"))
}

/// The text of a run of inline events, for a heading's anchor.
fn inline_text(events: &[Event]) -> String {
    let mut out = String::new();
    for ev in events {
        match ev {
            Event::Text(t) | Event::Code(t) => out.push_str(t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            _ => {}
        }
    }
    out
}

/// GitHub's heading slug: lowercase, spaces to `-`, punctuation dropped,
/// `-` and `_` kept. `## tasqx why` → `tasqx-why`, which is what the wiki's own
/// `Finding-Tasks.md#tasqx-why` links were written against.
fn gh_slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == ' ' {
            out.push('-');
        } else if c == '-' || c == '_' {
            out.push(c);
        }
    }
    out
}

/// Keep an id unique within the page by appending `-1`, `-2`, … the way GitHub
/// does. Two `## tasqx why` headings on one page would otherwise give the
/// document two `id="…"` of the same name, and every jump would pick the first.
fn unique(anchor: String, out: &[Event]) -> String {
    let taken = |a: &str| {
        let needle = format!(" id=\"{a}\">");
        out.iter()
            .any(|e| matches!(e, Event::Html(h) if h.ends_with(&needle)))
    };
    if !taken(&anchor) {
        return anchor;
    }
    (1..)
        .map(|n| format!("{anchor}-{n}"))
        .find(|a| !taken(a))
        .expect("an unbounded range always yields a free name")
}

/// `##` was the file's second level and the page's `h2` is its title, so every
/// level moves down one to land on the site's type scale.
fn shifted(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h2",
        HeadingLevel::H2 => "h3",
        HeadingLevel::H3 => "h4",
        HeadingLevel::H4 => "h5",
        HeadingLevel::H5 | HeadingLevel::H6 => "h6",
    }
}

/// The text of the code block opened at `i`, and the index just past its end.
fn code_text(events: &[Event], i: usize, file: &str) -> (String, usize) {
    let mut text = String::new();
    let mut j = i + 1;
    loop {
        match events.get(j) {
            Some(Event::Text(t)) => text.push_str(t),
            Some(Event::End(TagEnd::CodeBlock)) => return (text, j + 1),
            Some(other) => panic!("{file}: unexpected {other:?} inside a code fence"),
            None => panic!("{file}: an unterminated code fence"),
        }
        j += 1;
    }
}

/// A fence, as the site draws it: `console` is a terminal, everything else is a
/// plain block labelled with its language.
fn code_block(lang: &str, text: &str) -> String {
    let text = text.trim_end_matches('\n');
    match lang {
        "console" => console_block(text),
        "" => super::pre_plain(text),
        other => pre_lang(other, text),
    }
}

/// A ```console fence as one or more of the site's command blocks.
///
/// The wiki writes a fence two ways. Most are commands to run, one per line and
/// no prompt, which is one snippet with no output. A few show a session: a
/// `$ ` line is the command, the lines under it are what it printed. A second
/// `$ ` line after output starts a new block rather than appending to the first
/// command, because the output between them belongs to the command above it.
fn console_block(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    if !lines.iter().any(|l| l.starts_with("$ ")) {
        return super::snippet(text, "");
    }
    let mut out = String::new();
    let mut cmd: Vec<&str> = Vec::new();
    let mut output: Vec<&str> = Vec::new();
    for line in lines {
        match line.strip_prefix("$ ") {
            Some(rest) => {
                if !output.is_empty() {
                    flush(&mut out, &mut cmd, &mut output);
                }
                cmd.push(rest);
            }
            None => output.push(line),
        }
    }
    flush(&mut out, &mut cmd, &mut output);
    out
}

fn flush(out: &mut String, cmd: &mut Vec<&str>, output: &mut Vec<&str>) {
    let printed = output.join("\n");
    let printed = printed.trim_matches('\n');
    if cmd.is_empty() {
        // Output with no command above it: terminal text, not a session.
        if !printed.is_empty() {
            out.push_str(&super::term_block(&esc(printed)));
        }
    } else {
        out.push_str(&super::snippet(&cmd.join("\n"), printed));
    }
    cmd.clear();
    output.clear();
}

/// [`super::pre_plain`] with the fence's language on it, so a JSON block is not
/// silently the same thing as a block of output.
fn pre_lang(lang: &str, text: &str) -> String {
    format!(
        "<pre class=\"plain\" data-lang=\"{}\"><code>{}</code></pre>",
        esc(lang),
        esc(text),
    )
}

/// The opening half of [`super::note`] / [`super::warn`], for a blockquote
/// whose own paragraphs are the body. Kept identical to them by
/// [`tests::a_callout_opens_the_same_way_the_pages_own_callouts_do`].
fn callout_open(kind: &str, tag: &str) -> String {
    format!("<div class=\"callout {kind}\"><span class=\"tag\">{tag}</span>")
}

/// Which callout a blockquote becomes, and the index of the next event to
/// render.
///
/// A blockquote is the markdown for an aside, and the site draws asides as
/// callouts. `> **Warning** …` (or `**Careful**`) is the louder one; anything
/// else is a note. A leading marker is consumed — it is the label the callout
/// already prints — which is what moves the returned index past it.
fn callout_kind(events: &[Event], i: usize) -> (&'static str, &'static str, usize) {
    const NOTE: (&str, &str) = ("note", "Note");
    let plain = (NOTE.0, NOTE.1, i + 1);
    let marked = matches!(
        (
            events.get(i + 1),
            events.get(i + 2),
            events.get(i + 3),
            events.get(i + 4),
        ),
        (
            Some(Event::Start(Tag::Paragraph)),
            Some(Event::Start(Tag::Strong)),
            Some(Event::Text(_)),
            Some(Event::End(TagEnd::Strong)),
        )
    );
    if !marked {
        return plain;
    }
    let Some(Event::Text(word)) = events.get(i + 3) else {
        unreachable!("the shape was matched above");
    };
    let word = word.trim().trim_end_matches([':', '.', '!']).to_lowercase();
    match word.as_str() {
        "warning" | "careful" => ("warn", "Careful", i + 5),
        "note" => (NOTE.0, NOTE.1, i + 5),
        _ => plain,
    }
}

// ============================================================================
// Links
// ============================================================================

/// Where a markdown link ends up.
enum Target {
    /// An in-page anchor, including the leading `#`.
    Anchor(String),
    /// Off the file: the text stays, the URL rides beside it in a `<code>`.
    External(String),
    /// A file in the repository with no page here: the link text, unlinked.
    Text,
}

/// Files in the repository that a guide links to and that are not pages of this
/// guide. Each one's link text already names the path, so dropping the link
/// costs a reader nothing; inventing a page for it would cost them a wrong one.
const REPO_FILES_WITHOUT_A_PAGE: &[&str] = &[
    "../../.claude/skills/tasqx-workflow/SKILL.md",
    "../../.claude/skills/retro/SKILL.md",
];

fn resolve(file: &str, page_id: &str, dest: &str, ids: &BTreeMap<&'static str, String>) -> Target {
    if dest.starts_with("http://") || dest.starts_with("https://") {
        return Target::External(dest.to_string());
    }
    if let Some(anchor) = dest.strip_prefix('#') {
        // A link within the page: its ids carry the page id, so this one must too.
        return Target::Anchor(format!("#{page_id}--{anchor}"));
    }
    let (path, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (dest, None),
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    if let Some(id) = ids.get(name) {
        return Target::Anchor(match anchor {
            Some(a) => format!("#{id}--{a}"),
            None => format!("#{id}"),
        });
    }
    if name == "README.md" {
        // The README's job here is the Overview page's job there.
        return Target::Anchor("#overview".to_string());
    }
    if let Some((_, why)) = NOT_A_PAGE.iter().find(|(f, _)| *f == name) {
        panic!(
            "{file}: the link `{dest}` points at `{name}`, which is deliberately not a \
             page of this guide: {why}"
        );
    }
    if REPO_FILES_WITHOUT_A_PAGE.contains(&path) {
        return Target::Text;
    }
    panic!(
        "{file}: the link `{dest}` names neither a page of the guide nor a file in \
         REPO_FILES_WITHOUT_A_PAGE. Add the target to WIKI/GUIDES, or name it there \
         with the reason it has no page — a link that is emitted unresolved is a dead \
         one only a reader finds."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn docs_dir(which: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs")
            .join(which)
    }

    fn on_disk(which: &str) -> BTreeSet<String> {
        let names: BTreeSet<String> = fs::read_dir(docs_dir(which))
            .expect("the docs directory exists")
            .map(|e| e.expect("a readable dir entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .map(|p| {
                p.file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        // Floor: an empty listing must not read as "everything is embedded".
        assert!(
            names.len() >= 7,
            "docs/{which} holds only {} markdown files — where did the rest go?",
            names.len()
        );
        names
    }

    /// The whole point of a compiled-in table: it can go stale, and a file
    /// nobody embedded is a page the site silently does not have.
    ///
    /// Both directions. A file on disk that is neither embedded nor named in
    /// [`NOT_A_PAGE`] fails, and so does a table row whose file is gone — the
    /// second one is a compile error first (`include_str!`), but only for as
    /// long as the row keeps naming the same path.
    #[test]
    fn every_markdown_file_is_embedded_or_deliberately_not() {
        let mut declared: BTreeSet<String> = WIKI.iter().map(|(f, _)| (*f).to_string()).collect();
        for (f, _) in NOT_A_PAGE {
            declared.insert((*f).to_string());
        }
        assert_eq!(
            declared,
            on_disk("wiki"),
            "docs/wiki and the WIKI/NOT_A_PAGE tables disagree"
        );
        let guides: BTreeSet<String> = GUIDES.iter().map(|(f, _)| (*f).to_string()).collect();
        assert_eq!(
            guides,
            on_disk("guides"),
            "docs/guides and the GUIDES table disagree"
        );
    }

    /// Every file on disk is embedded verbatim — the table's second column is
    /// the file, not a paraphrase of it that an edit could leave behind.
    #[test]
    fn the_embedded_text_is_the_file_on_disk() {
        for (which, table) in [("wiki", WIKI), ("guides", GUIDES)] {
            for (file, src) in table {
                let path = docs_dir(which).join(file);
                let disk = fs::read_to_string(&path).expect("the embedded file is readable");
                assert_eq!(&disk, *src, "{file} is embedded from somewhere else");
            }
        }
    }

    #[test]
    fn a_page_id_is_the_file_name() {
        assert_eq!(page_id("wiki", "Finding-Tasks.md"), "wiki-finding-tasks");
        assert_eq!(
            page_id("wiki", "AI-Agents-and-Automation.md"),
            "wiki-ai-agents-and-automation"
        );
        assert_eq!(
            page_id("guide", "token-accounting.md"),
            "guide-token-accounting"
        );
    }

    /// The anchors the wiki's own links were written against are GitHub's, so
    /// this has to be GitHub's scheme and not [`super::slug`]'s.
    #[test]
    fn heading_slugs_are_the_ones_the_wikis_links_name() {
        assert_eq!(gh_slug("tasqx why"), "tasqx-why");
        assert_eq!(gh_slug("The filter language"), "the-filter-language");
        assert_eq!(
            gh_slug("tasqx report --outcomes"),
            "tasqx-report---outcomes"
        );
        assert_eq!(
            gh_slug("Dates, Reminders and Recurrence"),
            "dates-reminders-and-recurrence"
        );
        assert_eq!(
            gh_slug("Step 0: Delegate to a fork"),
            "step-0-delegate-to-a-fork"
        );
        assert_eq!(gh_slug("Token_accounting"), "token_accounting");
    }

    fn render_one(id: &str, src: &str) -> (String, String) {
        let ids = id_map();
        render(id, "test.md", src, &ids)
    }

    #[test]
    fn the_title_is_the_h1_and_it_leaves_the_body() {
        let (title, body) = render_one("wiki-x", "# Finding Tasks\n\nProse.\n");
        assert_eq!(title, "Finding Tasks");
        assert!(
            !body.contains("Finding Tasks"),
            "the title is in the body: {body}"
        );
        assert!(body.contains("<p>Prose.</p>"));
    }

    /// `##` is the page's first *section*, and the page's `h2` is its title —
    /// so the levels move down one or the sections compete with the title.
    #[test]
    fn heading_levels_shift_down_one_and_carry_a_prefixed_id() {
        let (_, body) = render_one("wiki-x", "# T\n\n## tasqx why\n\n### Deeper\n");
        assert!(
            body.contains("<h3 id=\"wiki-x--tasqx-why\">tasqx why</h3>"),
            "{body}"
        );
        assert!(
            body.contains("<h4 id=\"wiki-x--deeper\">Deeper</h4>"),
            "{body}"
        );
    }

    /// Two headings of the same name on one page would give the document two
    /// ids alike, and every jump would land on the first.
    #[test]
    fn a_repeated_heading_gets_a_distinct_id() {
        let (_, body) = render_one("wiki-x", "# T\n\n## tasqx why\n\n## tasqx why\n");
        assert!(body.contains("id=\"wiki-x--tasqx-why\""), "{body}");
        assert!(body.contains("id=\"wiki-x--tasqx-why-1\""), "{body}");
    }

    #[test]
    fn a_link_to_another_page_becomes_that_pages_hash_route() {
        let (_, body) = render_one("wiki-x", "# T\n\nSee [Memory](Memory.md).\n");
        assert!(
            body.contains("<a href=\"#wiki-memory\">Memory</a>"),
            "{body}"
        );
    }

    #[test]
    fn a_link_to_a_heading_carries_the_pages_prefix() {
        let (_, body) = render_one("wiki-x", "# T\n\n[why](Finding-Tasks.md#tasqx-why)\n");
        assert!(
            body.contains("<a href=\"#wiki-finding-tasks--tasqx-why\">why</a>"),
            "{body}"
        );
    }

    /// `../guides/…`, `../wiki/…`, `docs/wiki/…`: the directory prefix is the
    /// repository's layout, which the one-file guide does not have.
    #[test]
    fn a_link_through_a_directory_finds_the_same_page() {
        for dest in [
            "../guides/ai-agent-workflow.md",
            "docs/guides/ai-agent-workflow.md",
            "ai-agent-workflow.md",
        ] {
            let (_, body) = render_one("wiki-x", &format!("# T\n\n[a]({dest})\n"));
            assert!(
                body.contains("href=\"#guide-ai-agent-workflow\""),
                "{dest} → {body}"
            );
        }
    }

    #[test]
    fn a_link_inside_the_page_is_prefixed_too() {
        let (_, body) = render_one("wiki-projects", "# T\n\n[use](#tasqx-use)\n");
        assert!(
            body.contains("href=\"#wiki-projects--tasqx-use\""),
            "{body}"
        );
    }

    #[test]
    fn the_readme_becomes_the_overview_page() {
        let (_, body) = render_one("wiki-x", "# T\n\n[README](../../README.md)\n");
        assert!(body.contains("href=\"#overview\""), "{body}");
    }

    /// The file references nothing off itself, so an external link cannot stay
    /// a link. The reader still gets the address.
    #[test]
    fn an_external_link_becomes_text_with_its_url_beside_it() {
        let (_, body) = render_one(
            "wiki-x",
            "# T\n\n[MCP](https://modelcontextprotocol.io) is it.\n",
        );
        assert!(!body.contains("<a "), "an external href survived: {body}");
        assert!(
            body.contains("MCP (<code>https://modelcontextprotocol.io</code>)"),
            "{body}"
        );
    }

    #[test]
    fn a_repo_file_with_no_page_keeps_its_text_and_loses_its_link() {
        let (_, body) = render_one(
            "guide-x",
            "# T\n\n[`SKILL.md`](../../.claude/skills/tasqx-workflow/SKILL.md)\n",
        );
        assert!(!body.contains("<a "), "{body}");
        assert!(body.contains("<code>SKILL.md</code>"), "{body}");
    }

    /// Nothing dangles silently: a relative link nobody decided about stops the
    /// build, naming the file and the link.
    #[test]
    #[should_panic(expected = "names neither a page of the guide")]
    fn an_unknown_relative_link_is_a_build_time_panic() {
        render_one("wiki-x", "# T\n\n[gone](Nowhere.md)\n");
    }

    /// A link to a file that was *decided* against says so, rather than
    /// reading as a file nobody has embedded yet.
    #[test]
    #[should_panic(expected = "deliberately not a page")]
    fn a_link_to_a_file_that_is_not_a_page_says_why() {
        render_one("wiki-x", "# T\n\n[home](Home.md)\n");
    }

    #[test]
    #[should_panic(expected = "carries no images")]
    fn an_image_is_a_build_time_panic() {
        render_one("wiki-x", "# T\n\n![shot](../img/dash.png)\n");
    }

    /// A ```console fence of bare commands is one command block, prompt drawn
    /// by the page rather than typed into the file.
    #[test]
    fn a_console_fence_of_commands_is_one_snippet() {
        let html = console_block("tasqx init work\ntasqx add Buy milk");
        assert_eq!(html.matches("class=\"snip\"").count(), 1, "{html}");
        assert!(
            html.contains("tasqx init work\ntasqx add Buy milk"),
            "{html}"
        );
        assert!(!html.contains("class=\"out\""), "invented output: {html}");
    }

    /// A session — `$ cmd` then what it printed — is the command block with its
    /// output, and a second command after output starts a second block.
    #[test]
    fn a_console_session_splits_into_command_and_output() {
        let html = console_block("$ tasqx why 42\n#42  Ship it\n\n$ tasqx next\n#42");
        assert_eq!(html.matches("class=\"snip\"").count(), 2, "{html}");
        assert!(
            html.contains("<pre class=\"cmd\"><code>tasqx why 42</code></pre>"),
            "{html}"
        );
        assert!(html.contains("#42  Ship it"), "{html}");
        assert!(
            !html.contains("$ tasqx"),
            "the prompt is drawn, not copied: {html}"
        );
    }

    #[test]
    fn a_fenced_language_labels_a_plain_block() {
        let html = code_block("json", "{\"a\": 1}\n");
        assert!(html.contains("data-lang=\"json\""), "{html}");
        assert!(html.contains("class=\"plain\""), "{html}");
        assert!(html.contains("{&quot;a&quot;: 1}"), "escaped: {html}");
    }

    #[test]
    fn an_unlabelled_fence_is_a_plain_block() {
        let html = code_block("", "PROJECT  CLOSED\nwork  4\n");
        assert_eq!(html, super::super::pre_plain("PROJECT  CLOSED\nwork  4"));
    }

    /// A table is the site's grid inside the scroller, or a wide one takes the
    /// whole page sideways on a phone.
    #[test]
    fn a_table_is_the_sites_grid_in_its_own_scroller() {
        let (_, body) = render_one("wiki-x", "# T\n\n| A | B |\n|---|---|\n| 1 | 2 |\n");
        assert!(
            body.contains("<div class=\"tw\"><table class=\"grid\">"),
            "{body}"
        );
        assert_eq!(
            body.matches("<div class=\"tw\">").count(),
            body.matches("<table").count()
        );
        assert!(body.contains("</tbody></table></div>"), "{body}");
        assert!(body.contains("<th>A</th>"), "{body}");
    }

    #[test]
    fn a_blockquote_is_a_callout() {
        let (_, body) = render_one("wiki-x", "# T\n\n> Mind the gap.\n");
        assert!(body.contains("<div class=\"callout note\">"), "{body}");
        assert!(body.contains("Mind the gap."), "{body}");
    }

    #[test]
    fn a_marked_blockquote_takes_its_tone_and_drops_the_marker() {
        let (_, body) = render_one("wiki-x", "# T\n\n> **Warning** it bites.\n");
        assert!(body.contains("<div class=\"callout warn\">"), "{body}");
        assert!(!body.contains("<strong>"), "the marker survived: {body}");
        assert!(body.contains("it bites."), "{body}");
    }

    /// Two spellings of one piece of markup drift. This one is checked against
    /// the page's own callout builder rather than restated.
    #[test]
    fn a_callout_opens_the_same_way_the_pages_own_callouts_do() {
        assert!(super::super::note("x").starts_with(&callout_open("note", "Note")));
        assert!(super::super::warn("x").starts_with(&callout_open("warn", "Careful")));
    }

    /// Raw HTML in a source file is shown as text, never as markup: these files
    /// are hand-edited, and this is the one input to the document that is not a
    /// builder in `docs.rs`.
    #[test]
    fn raw_html_in_a_file_is_rendered_as_text() {
        let (_, body) = render_one("wiki-x", "# T\n\n<script>alert(1)</script>\n");
        assert!(!body.contains("<script>"), "{body}");
        assert!(body.contains("&lt;script&gt;"), "{body}");
    }

    /// Every page renders, has a title, and is not empty — the cheapest check
    /// that the twenty-one real files still parse.
    #[test]
    fn every_page_renders_with_a_title_and_a_body() {
        assert_eq!(PAGES.len(), WIKI.len() + GUIDES.len());
        for p in PAGES.iter() {
            assert!(!p.title.trim().is_empty(), "{} has no title", p.id);
            assert!(
                p.body.len() > 200,
                "{} rendered {} bytes",
                p.id,
                p.body.len()
            );
            assert!(
                p.id.starts_with("wiki-") || p.id.starts_with("guide-"),
                "unexpected page id {}",
                p.id
            );
        }
    }
}
