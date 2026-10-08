//! The D41 memory subsystem: retrieval over docs and annotations.
//!
//! `memory.search` promises ranked hits, not a ranking algorithm: FTS5's bm25
//! (D41) and the static embedding model's meaning (D196, `vectors.rs`) are
//! two lists behind one wire shape, fused by reciprocal rank, and every hit
//! says which of them found it.

use super::*;

/// #229 item 4: validate a memory `id`'s SHAPE before the store is ever
/// asked about it — mirroring `resolve_ref_value_on`'s `bad_request` for a
/// task ref that cannot possibly be a short_id or a UUID. Every memory id
/// this engine ever mints is a UUID (`memory_add`'s `clock::uuid_v7()`), so a
/// string that fails to parse as one is a malformed request, not a store
/// miss — the two must not share one exit code (DESIGN.md's `2`
/// bad_request vs `4` not_found contract exists so a script can branch
/// without parsing JSON).
fn require_uuid_shape(id: &str) -> Result<(), ApiError> {
    if Uuid::parse_str(id).is_ok() {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "memory id is not a UUID: {id} — expected a UUID like the one memory.add returns"
    )))
}

/// The closed `scope` vocabulary for `memory.search`. **First entry is the
/// default.** Source of truth in the [`SUMMARY_GROUP_BY`] sense: the engine
/// validates against it, builds its refusal from it, and the MCP tool schema
/// renders its JSON-Schema `enum` from it.
pub const MEMORY_SCOPES: [&str; 3] = ["all", "docs", "annotations"];

/// Escape a plain-text query into FTS5 phrase terms.
///
/// FTS5 treats `-`, `.`, `:` and quotes as query syntax, so the verified
/// failure this defuses is a user typing `server-side` and being answered
/// with `no such column: side`. Every whitespace-separated word becomes a
/// quoted phrase (embedded `"` doubled per FTS5's own escape rule), joined by
/// implicit AND. Callers who *want* the operator grammar pass `raw:true` and
/// own the syntax errors.
///
/// Infallible: its one caller has the query from `req_str`, which refuses
/// blank and whitespace-only strings, so there is always a word to quote.
fn phrase_escape(query: &str) -> String {
    query
        .split_whitespace()
        .map(phrase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One word as an FTS5 phrase — the single escaping rule [`phrase_escape`]
/// and [`any_word_expr`] share, so the AND and its fallback cannot quote a
/// word two ways.
fn phrase(word: &str) -> String {
    format!("\"{}\"", word.replace('"', "\"\""))
}

/// D193's any-word expression for a plain query, or `None` when there is no
/// fallback worth running.
///
/// The words are the query's own, as typed, minus [`QUERY_STOPWORDS`] (a
/// filler word is in nearly every entry, so an OR that kept it would answer a
/// genuine miss with noise) and minus a word already taken in another case
/// (FTS5 folds case, so `SDK OR sdk` is one term said twice). A word with no
/// letter or digit is dropped too: it tokenizes to nothing.
///
/// `None` when nothing is left, or when what is left is one word and the AND
/// was that same word alone: an OR over it is the search that already ran.
fn any_word_expr(query: &str) -> Option<String> {
    let and_terms: std::collections::HashSet<String> =
        query.split_whitespace().map(str::to_lowercase).collect();
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    for (word, _) in content_words(query) {
        if seen.insert(word.to_lowercase()) {
            terms.push(phrase(word));
        }
    }
    if terms.is_empty() || (terms.len() == 1 && seen == and_terms) {
        return None;
    }
    Some(terms.join(" OR "))
}

/// The words of `query` a search can do something with: each
/// whitespace-separated word as typed, beside its bare form (surrounding
/// punctuation trimmed, lowercased), for every word whose bare form is not
/// empty and not one of [`QUERY_STOPWORDS`]. The one normalisation D193's
/// any-word OR, D136's derived brief expression and D196's content-word gate
/// share, so the three cannot disagree about what a word is.
pub(super) fn content_words(query: &str) -> impl Iterator<Item = (&str, String)> {
    query.split_whitespace().filter_map(|word| {
        let bare = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        (!bare.is_empty() && !QUERY_STOPWORDS.contains(&bare.as_str())).then_some((word, bare))
    })
}

/// English function words dropped from a derived query (D136), and from the
/// any-word fallback of a plain search (D193).
///
/// **This list cannot hide a document, and that is what makes it safe to be a
/// list.** The derived expression is a disjunction, so a term dropped here
/// still leaves every other term matching — the only documents it removes are
/// ones whose sole connection to the task is a function word, which were never
/// relevant. It is English-only, and the cost of that on a title in another
/// language is noise in the ranking, never a missed hit: the same cost as
/// having no list at all.
///
/// Kept deliberately short. A long stopword list starts making judgements
/// about which content words matter, and bm25 already does that better — a
/// term present in most documents contributes almost nothing to the score.
/// This list exists only so that a title made entirely of them produces no
/// query rather than one matching the whole store — and so that D193's OR,
/// which runs only after the all-words search found nothing, does not turn
/// a miss into every entry that says "the".
pub(super) const QUERY_STOPWORDS: [&str; 24] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "in", "is", "it", "of",
    "on", "or", "that", "the", "this", "to", "with", "we", "our",
];

/// The FTS5 columns `--raw` can name a `column:query` against, for the given
/// `scope`. Kept beside [`MEMORY_SCOPES`]'s table rather than derived from
/// `docs_fts`/`annotations_fts` at query time — sqlite's own column
/// introspection would need a second round trip for an error path that is
/// already reporting a failure.
fn fts5_columns_for(scope: &str) -> &'static str {
    match scope {
        // D135: `docs_fts.body` reads the derived `docs.search_body` through
        // the `docs_search` view, but keeps the column name D41 published.
        "docs" => "title, body",
        "annotations" => "body",
        _ => "title, body (docs) or body (annotations)",
    }
}

/// #228.5: wrap a raw-mode FTS5 failure so it teaches instead of just
/// refusing. `--raw`'s own help advertises "columns" as part of the syntax it
/// hands the caller, so `no such column: title` must say which ones this
/// scope actually has — the same standard every other tasqx error holds
/// (`unknown scope`, `unknown setting`, `invalid priority`, …) — rather than
/// stopping at sqlite's own bare message, which also has no obligation to
/// stay legible outside its own error taxonomy.
fn raw_fts5_error(e: &rusqlite::Error, scope: &str) -> String {
    let msg = e.to_string();
    if let Some(col) = msg
        .rsplit("no such column: ")
        .next()
        .filter(|_| msg.contains("no such column: "))
    {
        return format!(
            "invalid FTS5 query: no such column {col:?} (searchable columns for \
             scope {scope:?}: {})",
            fts5_columns_for(scope)
        );
    }
    format!("invalid FTS5 query: {msg}")
}

/// How `memory.search`'s lexical MATCH expression is made from `query`.
/// D193's any-word expression is not a mode of its own: tasqx builds it
/// ([`any_word_expr`]) and runs it as a plain one.
enum MatchMode {
    /// Every word a required phrase ([`phrase_escape`]).
    Plain,
    /// The caller's own FTS5 expression; its errors are theirs.
    Raw,
}

/// When D193's any-word list runs beside a plain query's all-words one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnyWord {
    /// Never: `raw`, the brief's derived expression, the graph's edges.
    Never,
    /// Only when the all-words search found nothing, and then in its place:
    /// D193 as it was, under `mode: lexical`.
    Fallback,
    /// Always, as a third list fused beside the other two (D196, #838).
    Fused,
}

/// The closed `mode` vocabulary for `memory.search` (D196). **First entry is
/// the default.** The engine validates against it and builds its refusal
/// from it, and the MCP tool schema renders its JSON-Schema `enum` from it.
pub const MEMORY_SEARCH_MODES: [&str; 3] = ["hybrid", "lexical", "semantic"];

/// `min_similarity`'s default: the floor a semantic hit's best chunk must
/// reach (D196). Static embeddings separate related from unrelated text
/// weakly, so the floor trades some paraphrase recall for silence on a
/// query about nothing the store holds.
pub const MEMORY_MIN_SIMILARITY: f64 = 0.30;

/// Reciprocal-rank fusion's constant: a hit at rank `r` of a list scores
/// `weight / (FUSION_K + r)` (D196, tuned in #838).
const FUSION_K: f64 = 20.0;

/// Each list's weight in the fusion (D196, tuned in #838): the all-words
/// list, D193's any-word list, and the semantic list.
const WEIGHT_ALL_WORDS: f64 = 1.0;
const WEIGHT_ANY_WORD: f64 = 0.35;
const WEIGHT_SEMANTIC: f64 = 1.3;

/// How deep the all-words list (and D193's fallback under `mode: lexical`)
/// takes part in the fusion, by bm25, and the semantic list by similarity: a
/// constant, so fusion work is bounded and nothing a hit says moves with
/// `limit`. `total`, `via` and `partial` still read every match (#838).
const ALL_WORDS_DEPTH: usize = 1000;

/// How deep D193's any-word list is taken in hybrid mode, by bm25: a
/// constant, so which hits it finds — and so `via` and `partial` — never
/// move with `limit`, and a common word never scans a whole store (#838).
const ANY_WORD_DEPTH: usize = 200;

/// How many words a snippet carries — FTS5's `snippet()` token count, and
/// the cut a semantic hit's best chunk gets, so both read alike.
const SNIPPET_TOKENS: usize = 12;

/// The three `mode`s of [`MEMORY_SEARCH_MODES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchMode {
    Hybrid,
    Lexical,
    Semantic,
}

impl SearchMode {
    fn parse(s: &str) -> Result<SearchMode, ApiError> {
        match s {
            "hybrid" => Ok(SearchMode::Hybrid),
            "lexical" => Ok(SearchMode::Lexical),
            "semantic" => Ok(SearchMode::Semantic),
            _ => Err(ApiError::bad_request(format!(
                "unknown mode `{s}` — accepted: {}",
                MEMORY_SEARCH_MODES.join(", ")
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SearchMode::Hybrid => "hybrid",
            SearchMode::Lexical => "lexical",
            SearchMode::Semantic => "semantic",
        }
    }
}

/// `min_similarity` when the caller named one: a number from 0 to 1.
fn min_similarity(p: &Value) -> Result<Option<f64>, ApiError> {
    match p.get("min_similarity") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            // `+ 0.0`: a floor of `-0.0` is zero, and echoes as `0.0`.
            Some(x) if (0.0..=1.0).contains(&x) => Ok(Some(x + 0.0)),
            _ => Err(ApiError::bad_request(format!(
                "`min_similarity` must be a number from 0 to 1 (default \
                 {MEMORY_MIN_SIMILARITY:.2}), not {v}"
            ))),
        },
    }
}

/// A bare word (see [`content_words`]) of three letters or more: what the
/// meaning side is worth running for, and what its vocabulary gate counts.
/// `D41` and `#607` have none, so an identifier is looked up by its
/// characters alone (D196).
fn is_content(bare: &str) -> bool {
    bare.chars().filter(|c| c.is_alphabetic()).count() >= 3
}

/// A hit's identity across the lists. Ordered `(kind, id)`, the tie order
/// D196 ends on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct HitKey {
    kind: vectors::Kind,
    id: String,
}

impl HitKey {
    fn parse(kind: &str, id: String) -> HitKey {
        HitKey {
            kind: if kind == "doc" {
                vectors::Kind::Doc
            } else {
                vectors::Kind::Annotation
            },
            id,
        }
    }
}

/// Which side found a fused hit. Declared in D196's tie order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Via {
    Both,
    Lexical,
    Semantic,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Both => "both",
            Via::Lexical => "lexical",
            Via::Semantic => "semantic",
        }
    }
}

/// One ranked list going into the fusion, and its weight.
struct Ranked<'a> {
    keys: &'a [HitKey],
    weight: f64,
}

/// One hit of the fused list.
#[derive(Debug)]
struct Fused {
    key: HitKey,
    via: Via,
    score: f64,
}

/// Weighted reciprocal-rank fusion (D196): a hit scores Σ weight/(k + rank)
/// over the lists it is on, ranks counted from 1 over each WHOLE list — so a
/// hit's score, and which side found it, never depend on how many hits the
/// caller asked for. Best first; a tie goes to `both` before `lexical`
/// before `semantic`, then to `(kind, id)`. One list alone keeps its order.
fn fuse(lists: &[Ranked<'_>], via_of: &dyn Fn(&HitKey) -> Via) -> Vec<Fused> {
    let mut acc: HashMap<&HitKey, f64> = HashMap::new();
    for list in lists {
        for (i, k) in list.keys.iter().enumerate() {
            *acc.entry(k).or_insert(0.0) += list.weight / (FUSION_K + (i + 1) as f64);
        }
    }
    let mut out: Vec<Fused> = acc
        .into_iter()
        .map(|(key, score)| Fused {
            key: key.clone(),
            via: via_of(key),
            score,
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.via.cmp(&b.via))
            .then_with(|| a.key.cmp(&b.key))
    });
    out
}

/// What one `memory.search` looks through and how much it hands back: the
/// params every side reads, validated once.
struct SearchWindow<'a> {
    limit: usize,
    scope: String,
    project: Option<String>,
    include_unscoped: bool,
    exclude_task: Option<&'a str>,
}

impl<'a> SearchWindow<'a> {
    fn parse(p: &Value, exclude_task: Option<&'a str>) -> Result<SearchWindow<'a>, ApiError> {
        // Checked: a value above i64::MAX once wrapped negative, and SQLite
        // reads a negative LIMIT as UNLIMITED — the exact opposite of the
        // bound the caller asked for (review finding).
        let limit = opt_u64(p, "limit")?.unwrap_or(crate::engine::MEMORY_SEARCH_LIMIT);
        if i64::try_from(limit).is_err() {
            return Err(ApiError::bad_request(format!(
                "`limit` must be at most {}, or omitted for the default",
                i64::MAX
            )));
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let scope = opt_str(p, "scope")?.unwrap_or_else(|| MEMORY_SCOPES[0].to_string());
        if !MEMORY_SCOPES.contains(&scope.as_str()) {
            return Err(ApiError::bad_request(format!(
                "unknown scope `{scope}` — accepted: {}",
                MEMORY_SCOPES.join(", ")
            )));
        }
        let project = opt_str_nonempty(p, "project")?;
        // D136: a document with no project is GLOBAL knowledge — `tasqx memory
        // import docs/` sets no project on anything it imports, so a strict
        // project scope hides every ADR a reader fed the store. Opt-in and
        // additive, because D115's strict scope stays the right answer when a
        // caller is asking about one project's own notes.
        let include_unscoped = opt_bool(p, "include_unscoped")?.unwrap_or(false);
        if include_unscoped && project.is_none() {
            // D33: a value that changes nothing is refused rather than
            // accepted and ignored.
            return Err(ApiError::bad_request(
                "`include_unscoped` widens a `project` scope to documents that have no \
                 project, so it needs a `project` to widen from",
            ));
        }
        Ok(SearchWindow {
            limit,
            scope,
            project,
            include_unscoped,
            exclude_task,
        })
    }
}

/// How one search runs its sides.
struct Fusion<'a> {
    /// How the lexical expression is made; `None` runs no lexical side.
    lexical: Option<MatchMode>,
    any_word: AnyWord,
    /// The text the semantic side embeds; `None` runs no semantic side.
    meaning: Option<&'a str>,
    floor: f64,
    exclude_task: Option<&'a str>,
}

/// One lexical list: the expression that ran, and every entry it matched,
/// best bm25 first.
struct LexicalList {
    expr: String,
    /// The first entries by bm25, to the list's cap: what takes part in
    /// the fusion.
    keys: Vec<HitKey>,
    /// Every other entry the expression matched, past the cap: with `keys`,
    /// what `total`, `via` and `partial` read. Empty when nothing was cut.
    rest: HashSet<HitKey>,
}

/// The semantic side of one search, when it compared anything.
struct SemanticSide {
    /// Every entry at or above the floor, best first.
    keys: Vec<HitKey>,
    similarity: HashMap<HitKey, f64>,
    chunk: HashMap<HitKey, i64>,
}

impl SemanticSide {
    fn new(hits: Vec<vectors::SemanticHit>) -> SemanticSide {
        let mut side = SemanticSide {
            keys: Vec::with_capacity(hits.len()),
            similarity: HashMap::with_capacity(hits.len()),
            chunk: HashMap::with_capacity(hits.len()),
        };
        for h in hits {
            let key = HitKey {
                kind: h.kind,
                id: h.owner_id,
            };
            side.similarity.insert(key.clone(), h.similarity);
            side.chunk.insert(key.clone(), h.chunk);
            side.keys.push(key);
        }
        side
    }
}

/// A semantic-only hit's snippet: its best chunk, cut the way FTS5's
/// `snippet()` cuts a lexical one — [`SNIPPET_TOKENS`] words, `…` where text
/// was left out — without the doc's title, which the hit already carries.
fn chunk_snippet(kind: vectors::Kind, title: &str, ix: i64, chunk: &str) -> String {
    let body = match kind {
        vectors::Kind::Doc => chunk
            .strip_prefix(title.trim())
            .map(str::trim_start)
            .filter(|b| !b.is_empty())
            .unwrap_or(chunk),
        vectors::Kind::Annotation => chunk,
    };
    let words: Vec<&str> = body.split_whitespace().collect();
    let mut out = String::new();
    if ix > 0 {
        out.push('…');
    }
    out.push_str(&words[..words.len().min(SNIPPET_TOKENS)].join(" "));
    if words.len() > SNIPPET_TOKENS {
        out.push('…');
    }
    out
}

/// What [`Engine::session_rulings`] reads for an MCP session's `initialize`
/// (#96, D157).
pub struct SessionRulings {
    /// The inferred project and how it was inferred (`working directory` or
    /// `default project`); `None` when neither names one.
    pub project: Option<(String, &'static str)>,
    /// Standing docs: the project's first, then the unscoped ones, each
    /// newest-modified first.
    pub standing: Vec<SessionDoc>,
    /// The project's own non-standing docs, newest-modified first, capped at
    /// 256 rows (PR #48 review) — the section's own budget can never render
    /// that many anyway.
    pub topical: Vec<SessionDoc>,
    /// How many non-standing docs the project actually has, independent of
    /// the `topical` cap — the footer's "N more docs" count over `topical`
    /// alone would undercount once a project passes 256 (PR #48 review).
    pub topical_total: usize,
}

/// One doc as [`SessionRulings`] carries it.
pub struct SessionDoc {
    /// The doc's title.
    pub title: String,
    /// The doc's whole body, frontmatter included.
    pub body: String,
}

/// D174: a doc's `source` is its identity — the key `memory.import` replaces
/// by — so a write that would give `id` a source a DIFFERENT doc already holds
/// is a `conflict` naming that doc, never a second holder. Every door that
/// sets `source` from a caller (`memory.add`, `memory.update`, `store.import`)
/// asks here first; the partial UNIQUE index `idx_docs_source` is the
/// backstop that would otherwise answer with a raw constraint failure. An
/// empty source is no identity, exactly like none: the index leaves it out.
pub(crate) fn refuse_source_held_elsewhere(
    conn: &Connection,
    source: Option<&str>,
    id: &str,
) -> Result<(), ApiError> {
    let Some(source) = source.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let holder: Option<String> = conn
        .query_row(
            // `source <> ''` restates the index's predicate so the planner
            // can use it; the filter above already guarantees it holds.
            "SELECT id FROM docs WHERE source = ?1 AND source <> '' AND id <> ?2",
            params![source, id],
            |r| r.get(0),
        )
        .optional()?;
    match holder {
        None => Ok(()),
        Some(other) => Err(ApiError::conflict(format!(
            "source {source:?} already belongs to memory doc {other}: a source names one \
             doc (D174) — update {other} instead, or give {id} another source"
        ))),
    }
}

/// #972/D203: a `memory.import` replace keyed on a `source` whose stored doc
/// was read from a DIFFERENT file than this one is two files sharing one key
/// — two clones under the same directory name — not a re-import, and going
/// ahead would overwrite the other file's doc beyond `undo`. A `conflict`
/// naming the doc, its source and both files refuses it.
///
/// Only when both files are known and the stored one still exists here: an
/// incoming doc with no origin (a JSON-API caller) carries no claim, and a
/// stored file that is gone (the repo moved, or the store was synced from
/// another machine whose paths these are) is not a competing file, so both
/// replace as they always did. Legacy rows with no origin never reach here.
///
/// "Gone" is `try_exists` answering `Ok(false)`, not `exists()`: that one
/// says `false` on a metadata error too, and a present-but-unreadable file
/// would then lose its doc. An error refuses with `bad_request`, the code
/// `memory_doc::read_doc` gives a file it cannot read. Two files in one
/// repository — a linked worktree and its main checkout, which share keys
/// by design (D203) — are a re-import, not a competitor.
fn refuse_replace_from_another_file(
    id: &str,
    source: &str,
    stored: &str,
    incoming: Option<&str>,
) -> Result<(), ApiError> {
    let Some(incoming) = incoming.filter(|incoming| *incoming != stored) else {
        return Ok(());
    };
    match std::path::Path::new(stored).try_exists() {
        Ok(false) => Ok(()),
        Err(e) => Err(ApiError::bad_request(format!(
            "source {source:?} already belongs to memory doc {id}, read from {stored}, and \
             that file cannot be checked ({e}); this import read {incoming} (D203) — nothing \
             was imported, so the stored doc is not overwritten"
        ))),
        Ok(true) if same_repository(stored, incoming) => Ok(()),
        Ok(true) => Err(ApiError::conflict(format!(
            "source {source:?} already belongs to memory doc {id}, read from {stored}; \
             this import read {incoming}, a different file under the same key (D203) — \
             nothing was imported. Rename one checkout's directory, or `tasqx memory rm \
             {id}` if the stored doc is obsolete"
        ))),
    }
}

/// Whether `a` and `b` sit in one git repository: their toplevels resolve,
/// through a linked worktree's `commondir`, to the same main checkout — the
/// resolution `import_source` keys on (`memory_doc::repo_dir`). Outside a
/// git work tree, or where the chain cannot be read, there is no shared
/// repository to claim, so the answer is `false` and the guard stands.
fn same_repository(a: &str, b: &str) -> bool {
    let repo = |file: &str| {
        let toplevel = crate::memory_doc::git_toplevel(std::path::Path::new(file))?;
        let dir = crate::memory_doc::repo_dir(&toplevel);
        Some(std::fs::canonicalize(&dir).unwrap_or(dir))
    };
    matches!((repo(a), repo(b)), (Some(a), Some(b)) if a == b)
}

/// One document on its way into `docs`, as the two doors that land a file
/// both spell it: `memory.import`'s per-entry read of a batch, and
/// `memory.refresh`'s re-read of a file that changed under an existing doc.
///
/// `id` and `rev` are the CALLER's: an import looks the row up by `source`
/// (D174) and mints a UUID when there is none, while a refresh already holds
/// the row it re-read and must update THAT one — a doc with an origin file
/// and no source would otherwise be inserted a second time by a lookup that
/// found nothing.
struct DocWrite<'a> {
    id: &'a str,
    /// The revision this write lands at: the row's current one plus one on a
    /// replace, `0` on a brand-new doc (D143).
    rev: i64,
    title: &'a str,
    body: &'a str,
    source: Option<&'a str>,
    project: Option<&'a str>,
    origin_path: Option<&'a str>,
    origin_mtime: Option<i64>,
    origin_size: Option<i64>,
    /// The method the event's `via` names, and whether the row was already
    /// there — both things only the caller knows.
    via: &'a str,
    replaced: bool,
}

/// Land one document, in place when its id is already in `docs`.
///
/// ON CONFLICT DO UPDATE, never DELETE+INSERT (D41's own rule, learned the
/// hard way for the annotation upsert): a DELETE does not fire `docs_fts`'s
/// delete trigger for free, and re-doing it by hand here would be a second
/// copy of the exact bug that rule exists to prevent. The UPDATE path fires
/// `docs_fts_au` and keeps the index honest. `created` is deliberately absent
/// from the SET list, so a replace keeps the ORIGINAL creation date rather
/// than pretending the doc is new.
///
/// #101: `standing` is NOT in the SET list either — a re-imported directory
/// carries no opinion about whether a doc is standing, so the flag a person
/// set through `memory.update` survives the re-run that would otherwise
/// silently clear it.
///
/// #657: `project` follows the SAME rule, one way only: a write that names NO
/// `project` carries no opinion either, so a doc's existing scope survives a
/// re-run the way `standing` does. A brand-new doc's row has no prior
/// `project` to fall back to, so `?6` binds the given value (or SQL NULL,
/// unscoped, when omitted) directly on INSERT — the same default `memory.add`
/// gives. On a replace, `COALESCE(excluded.project, docs.project)` picks the
/// just-bound value when the caller named one and the row's OWN prior value
/// otherwise, in one statement: a caller who names a project has an opinion,
/// and that opinion moves the scope on re-import — a directory re-pointed at
/// `--project ledger` after landing unscoped is meant to land scoped, not
/// stay stuck at its first import's answer (D168). `memory.refresh` names
/// none, which is how a refreshed doc keeps the scope it had.
///
/// `rev` is in the SET list because a replace is a revision of the same
/// document (D143): a `memory.update` still holding the pre-import
/// `expected_rev` must conflict, not clobber the import. The caller computes
/// it from the row it looked up inside this same IMMEDIATE transaction, so
/// nothing can move the row between the lookup and the upsert.
///
/// #788/D180: the three origin columns ARE in the SET list, plainly and
/// without a COALESCE — the opposite rule from `standing` and `project`
/// above, because they describe THIS read of the file. A re-import that
/// carries no origin (a caller on the JSON API, not the CLI's importer) must
/// leave the row saying it has no origin rather than keeping a path and an
/// mtime from an import that happened on another machine a year ago.
fn upsert_doc(tx: &Transaction, ts: &str, d: &DocWrite<'_>) -> Result<(), ApiError> {
    // The CLI's own importer already cuts frontmatter before it ever reaches
    // this call (#228.4's throwaway agent-memory metadata), so `search_body`
    // equals `body` there; it only matters for a caller on the JSON API
    // directly, which gets `memory.add`'s same index guarantee without `body`
    // itself being touched.
    let search_body = crate::frontmatter::flatten(d.body).into_owned();
    tx.execute(
        "INSERT INTO docs \
         (id, source, title, body, search_body, project, rev, created, modified, \
          origin_path, origin_mtime, origin_size) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11) \
         ON CONFLICT(id) DO UPDATE SET \
         source=excluded.source, title=excluded.title, body=excluded.body, \
         search_body=excluded.search_body, \
         project=COALESCE(excluded.project, docs.project), \
         modified=excluded.modified, rev=excluded.rev, \
         origin_path=excluded.origin_path, origin_mtime=excluded.origin_mtime, \
         origin_size=excluded.origin_size",
        params![
            d.id,
            d.source,
            d.title,
            d.body,
            search_body,
            d.project,
            d.rev,
            ts,
            d.origin_path,
            d.origin_mtime,
            d.origin_size
        ],
    )?;
    insert_event(
        tx,
        Entity::Doc,
        d.id,
        "memory.add",
        &json!({
            "title": d.title,
            "source": d.source,
            "project": d.project,
            "via": d.via,
            "replaced": d.replaced,
            "rev": d.rev,
        }),
    )
}

/// A doc that names an origin file, as `memory.refresh` reads it back.
struct OriginDoc {
    id: String,
    source: Option<String>,
    rev: i64,
    title: String,
    body: String,
    origin_path: String,
    origin_mtime: Option<i64>,
    origin_size: Option<i64>,
}

impl Engine {
    // ---- memory.add ----------------------------------------------------------

    /// `memory.add` — store one knowledge document. Params: `title`, `body`,
    /// optional `source`, optional `project` (#134), optional `standing`
    /// (#101). Returns its new id.
    ///
    /// A doc is standalone, not attached to a task: annotations already cover
    /// "a note about this task", and [`Entity::Doc`] exists so the two stay
    /// distinguishable in the event log.
    ///
    /// `project` is free-standing scoping, not a foreign key onto
    /// `projects`: a doc worth keeping can outlive the project it was
    /// written about, or apply to none at all. Omitting it leaves the doc
    /// global/unscoped rather than defaulting it onto whatever project is
    /// current — the same reasoning `task.add`'s `default_project` explicitly
    /// does NOT extend to memory.
    ///
    /// Task #12/D135: `body` is stored exactly as given — `memory.get`,
    /// `memory show` and `store.export` all see it byte for byte, and a
    /// `store.export`/`store.import` round-trip stays lossless. What a
    /// leading YAML frontmatter block gets is a SEPARATE, derived
    /// `search_body` ([`crate::frontmatter::flatten`]), read only by
    /// `docs_fts` (see `crate::storage::migrate_memory`) — so `memory.add`
    /// is the one write door every client shares (CLI, MCP's
    /// `tasqx_add_memory`, `memory.import`'s per-doc insert) is also the one
    /// place that keeps the index in step with the body, without the body
    /// itself ever losing a byte of what was written.
    pub fn memory_add(&self, p: &Value) -> Result<Value, ApiError> {
        let title = req_str(p, "title")?;
        let body = req_str(p, "body")?;
        let search_body = crate::frontmatter::flatten(&body).into_owned();
        let source = opt_str(p, "source")?;
        let project = opt_str_nonempty(p, "project")?;
        // #101/D156: a standing doc belongs in every session of its scope
        // until it is retracted — a correction given once, not something a
        // keyword search has to rediscover. Default false, so every existing
        // caller keeps writing ordinary memory.
        let standing = opt_bool(p, "standing")?.unwrap_or(false);

        let id = crate::clock::uuid_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        refuse_source_held_elsewhere(&tx, source.as_deref(), &id)?;
        tx.execute(
            "INSERT INTO docs \
             (id, source, title, body, search_body, project, standing, created, modified) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![id, source, title, body, search_body, project, standing, ts],
        )?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.add",
            &json!({ "title": title, "source": source, "project": project, "standing": standing }),
        )?;
        let mut out = json!({ "id": id, "title": title, "project": project, "standing": standing, "created": ts });
        if standing {
            // The soft cap, counted inside the same transaction as the INSERT
            // so the number cannot name a set the write is not part of.
            // `project IS ?1` and not `=`: the unscoped scope is NULL, which
            // `=` matches nothing at all — every global standing doc would
            // count as the first one forever.
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM docs WHERE standing = 1 AND project IS ?1",
                params![project],
                |r| r.get(0),
            )?;
            // A hint, never a refusal: the engine cannot know which of the
            // rulings is the redundant one, and the key is ABSENT rather than
            // null under the cap, so a client tests presence and not value.
            if count > STANDING_SOFT_CAP as i64 {
                out["hint"] = json!(format!(
                    "{count} standing docs for this scope, over the soft cap of \
                     {STANDING_SOFT_CAP}: merge or retract with tasqx_update_memory \
                     standing:false (D156)"
                ));
            }
        }
        tx.commit()?;

        Ok(out)
    }

    // ---- memory.import -------------------------------------------------------

    /// Bulk-load docs in ONE transaction with replace-by-source semantics.
    ///
    /// This exists because the CLI's first import looped over files calling
    /// `memory.add` per file (review finding): a mid-directory failure
    /// committed a silent partial import, and re-running the command
    /// duplicated every doc that had already landed. Here a bad entry rolls
    /// the whole batch back, and a doc whose `source` matches an existing one
    /// REPLACES it — re-importing an edited directory converges instead of
    /// accumulating.
    pub fn memory_import(&self, p: &Value) -> Result<Value, ApiError> {
        let docs = req_array(p, "docs").map_err(|e| {
            ApiError::bad_request(format!(
                "{} — memory.import requires a `docs` array",
                e.message
            ))
        })?;
        // #657: one project for the whole batch. A directory import is one
        // source of docs — asking a caller to repeat the same `project` on
        // every entry would earn nothing `memory.add --project` does not
        // already cover one doc at a time. Validated like every other
        // project-taking write (D23): an unknown name is refused before
        // anything is written, inside the same transaction the docs land in.
        let project = opt_str_nonempty(p, "project")?;

        // D174: a source names one doc, so a batch naming it twice asks for
        // two docs on one identity — it used to insert the first and replace
        // it with the second, reporting two entries for one id. Refused whole,
        // before the write lock is taken. A malformed `source` is left to the
        // per-doc validation below, and so is `""`, which is no identity.
        let mut seen = HashSet::new();
        let mut twice: Vec<&str> = Vec::new();
        let sources = docs.iter().filter_map(|d| d.get("source")?.as_str());
        for src in sources.filter(|s| !s.is_empty()) {
            if !seen.insert(src) && !twice.contains(&src) {
                twice.push(src);
            }
        }
        if !twice.is_empty() {
            return Err(ApiError::bad_request(format!(
                "memory.import names the same source more than once: {} — a source \
                 names one doc (D174), so send each file once",
                twice.join(", ")
            )));
        }

        let ts = now();
        let tx = self.begin_mutation()?;
        if let Some(name) = &project {
            require_live_project(&tx, name)?;
        }
        let mut out = Vec::new();
        let mut replaced = 0i64;
        // #972: each doc a replace overwrote, with the title it had — the
        // count alone never said WHICH doc's text was just lost.
        let mut replaced_docs = Vec::new();
        for dv in docs {
            let dv = import_shape("", "doc", dv)?;
            import_keys(
                "",
                "doc",
                dv,
                &[
                    "title",
                    "body",
                    "source",
                    "origin_path",
                    "origin_mtime",
                    "origin_size",
                ],
            )?;
            let title = req_str(dv, "title")?;
            let body = req_str(dv, "body")?;
            let source = opt_str_nonempty(dv, "source")?;
            // #788/D180: where the file was and what it looked like when it
            // was read. Optional on every entry — the JSON API is reachable
            // by a caller with no filesystem at all — and never identity:
            // `source` is what a re-import keys on (D174), these three are
            // only what a later freshness check (#789) compares against.
            let origin_path = opt_str_nonempty(dv, "origin_path")?;
            let origin_mtime = opt_i64(dv, "origin_mtime")?;
            let origin_size = opt_i64(dv, "origin_size")?;
            // A doc whose `source` matches an existing row is a RE-IMPORT of
            // the same logical document (a directory re-run after an edit),
            // not a new one — so it UPDATES that row rather than deleting and
            // re-minting (#178/#198). The DELETE-then-INSERT this replaces
            // always struck a fresh UUIDv7, so `memory show <id>`, an
            // annotation citing the doc, and MCP's `tasqx_get_memory` all
            // 404'd the moment ANY re-run happened — announced nowhere, and
            // at scale on a real store one source had been silently
            // overwritten 34 times.
            let existing: Option<(String, i64, String, Option<String>)> = match &source {
                Some(src) => tx
                    .query_row(
                        "SELECT id, rev, title, origin_path FROM docs WHERE source = ?1",
                        params![src],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?,
                None => None,
            };
            if let Some((id, _, _, Some(stored))) = &existing {
                refuse_replace_from_another_file(
                    id,
                    source.as_deref().unwrap_or_default(),
                    stored,
                    origin_path.as_deref(),
                )?;
            }
            let is_replace = existing.is_some();
            let (id, rev) = match existing {
                Some((id, cur_rev, previous_title, _)) => {
                    replaced_docs.push(json!({
                        "id": id,
                        "source": source,
                        "previous_title": previous_title,
                    }));
                    (id, cur_rev + 1)
                }
                None => (crate::clock::uuid_v7().to_string(), 0),
            };
            // The upsert itself, its comment and its event live in
            // `upsert_doc` — shared with `memory.refresh` (#789), which
            // lands a re-read file through the same statement so the two
            // doors cannot drift on what a replace keeps.
            upsert_doc(
                &tx,
                &ts,
                &DocWrite {
                    id: &id,
                    rev,
                    title: &title,
                    body: &body,
                    source: source.as_deref(),
                    project: project.as_deref(),
                    origin_path: origin_path.as_deref(),
                    origin_mtime,
                    origin_size,
                    via: "memory.import",
                    replaced: is_replace,
                },
            )?;
            if is_replace {
                replaced += 1;
            }
            out.push(json!({
                "id": id,
                "title": title,
                "source": source,
                "project": project,
                "replaced": is_replace,
                "_rev": rev,
            }));
        }
        tx.commit()?;

        Ok(json!({
            "imported": out.len(),
            "replaced": replaced,
            "docs": out,
            "replaced_docs": replaced_docs,
        }))
    }

    // ---- memory.refresh ------------------------------------------------------

    /// Has the file at `origin_path` changed since the doc was read off it?
    ///
    /// `None` when the file is gone or unreadable — the one answer that is
    /// not about content, and the reason this is not a `bool`: a deleted file
    /// must be REPORTED, never mistaken for a doc that needs rewriting or one
    /// that is up to date.
    ///
    /// The stored `origin_mtime` and `origin_size` (D180) are the fast
    /// filter: both matching what the filesystem says now ends the question
    /// without opening the file, which is what lets a whole store be checked
    /// on a turn. The byte compare is the truth. So a `touch` is not a
    /// change: the mtime moves, the fast filter misses, the file is re-read
    /// through [`crate::memory_doc::read_doc`] — the same reader the import
    /// used, so frontmatter and a BOM are cut the same way — and the derived
    /// title and body compare equal.
    ///
    /// A doc whose stored mtime is null (a platform that would not answer for
    /// it) never takes the fast path: two unknowns are not a match.
    pub fn origin_changed(
        &self,
        origin_path: &str,
        origin_mtime: Option<i64>,
        origin_size: Option<i64>,
        title: &str,
        body: &str,
    ) -> Option<bool> {
        let path = std::path::Path::new(origin_path);
        let meta = std::fs::metadata(path).ok()?;
        if origin_mtime.is_some()
            && origin_mtime == crate::memory_doc::unix_seconds(&meta)
            && origin_size == i64::try_from(meta.len()).ok()
        {
            return Some(false);
        }
        let (fresh_title, fresh_body) = crate::memory_doc::read_doc(path).ok()?;
        Some(fresh_title != title || fresh_body != body)
    }

    /// `memory.refresh` — re-read every doc whose origin file changed, and
    /// name the ones whose file is gone.
    ///
    /// Params: `dry_run` (default false) does every check and writes nothing.
    ///
    /// Only docs holding an `origin_path` are looked at, so a doc written by
    /// `memory.add` — no file behind it — is never touched and never counted.
    /// A changed file is landed through the same `upsert_doc` a re-import
    /// uses: the doc keeps its id and creation date, bumps its `rev` (D143),
    /// keeps its `project` and `standing`, and restates all three origin
    /// columns from the metadata of the read that just happened (D180).
    ///
    /// **Nothing is ever deleted.** A file that has vanished is reported
    /// under `missing`, with the doc left exactly as it was: the file may be
    /// on a machine this store was copied off, or behind an unmounted
    /// volume, and a sweep that removed knowledge for either would be a data
    /// loss nobody asked for. Retiring the doc stays `tasqx memory rm`, a
    /// human act.
    pub fn memory_refresh(&self, p: &Value) -> Result<Value, ApiError> {
        let dry_run = opt_bool(p, "dry_run")?.unwrap_or(false);
        let ts = now();
        let tx = self.begin_mutation()?;
        let docs: Vec<OriginDoc> = {
            let mut stmt = tx.prepare(
                "SELECT id, source, rev, title, body, origin_path, origin_mtime, origin_size \
                 FROM docs WHERE origin_path IS NOT NULL ORDER BY created, id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(OriginDoc {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    rev: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    origin_path: r.get(5)?,
                    origin_mtime: r.get(6)?,
                    origin_size: r.get(7)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let checked = docs.len();
        let mut refreshed = Vec::new();
        let mut missing = Vec::new();
        let mut unchanged = 0i64;
        for d in &docs {
            match self.origin_changed(
                &d.origin_path,
                d.origin_mtime,
                d.origin_size,
                &d.title,
                &d.body,
            ) {
                None => missing.push(json!({
                    "id": d.id,
                    "source": d.source,
                    "origin_path": d.origin_path,
                })),
                Some(false) => unchanged += 1,
                Some(true) => {
                    let path = std::path::Path::new(&d.origin_path);
                    let (title, body) = crate::memory_doc::read_doc(path)?;
                    // Read AFTER the file, not before: the numbers have to
                    // describe the bytes just stored, or the next sweep
                    // compares a fresh doc against a stale stamp and
                    // refreshes it again forever.
                    let meta = std::fs::metadata(path).ok();
                    if !dry_run {
                        upsert_doc(
                            &tx,
                            &ts,
                            &DocWrite {
                                id: &d.id,
                                rev: d.rev + 1,
                                title: &title,
                                body: &body,
                                source: d.source.as_deref(),
                                // No opinion: the doc keeps the scope it has.
                                project: None,
                                origin_path: Some(&d.origin_path),
                                origin_mtime: meta
                                    .as_ref()
                                    .and_then(crate::memory_doc::unix_seconds),
                                origin_size: meta
                                    .as_ref()
                                    .and_then(|m| i64::try_from(m.len()).ok()),
                                via: "memory.refresh",
                                replaced: true,
                            },
                        )?;
                    }
                    refreshed.push(json!({ "id": d.id, "source": d.source }));
                }
            }
        }
        tx.commit()?;

        Ok(json!({
            "checked": checked,
            "refreshed": refreshed,
            "missing": missing,
            "unchanged": unchanged,
        }))
    }

    // ---- memory.search -------------------------------------------------------

    /// `memory.search` — ranked retrieval over docs and annotations, by words
    /// and by meaning (D196). Params: `query`, `limit` (default 10), `scope`
    /// (one of [`MEMORY_SCOPES`], default `all`), `raw`, optional `project`
    /// (#134) and `include_unscoped` (D136), `mode` (one of
    /// [`MEMORY_SEARCH_MODES`], default `hybrid`) and `min_similarity` (0 to
    /// 1, default [`MEMORY_MIN_SIMILARITY`]).
    ///
    /// The words side is D41's: `raw:false` (the default) escapes the query
    /// into FTS5 phrases, so ordinary text containing `-` or `:` is a search
    /// rather than a syntax error, and `raw:true` hands the FTS5 operator
    /// grammar to the caller, who then owns its errors — which is why a
    /// refused raw query is `bad_request` and not `internal`. `raw` is
    /// lexical by definition, so beside `mode: hybrid` or `semantic` it is
    /// refused rather than half-honoured.
    ///
    /// In hybrid mode three lists are fused (`fuse`): every word required,
    /// D193's any word, and meaning. A hit only the any-word list found says
    /// `partial: true`, and `relaxed` is true when one is on the page, so a
    /// hit holding some of the words is never passed off as one holding them
    /// all. Under `mode: lexical` D193 is as it was: the any-word OR runs only
    /// when no entry holds every word, in its place, and says `relaxed`.
    /// `raw` never widens: the caller wrote the grammar.
    ///
    /// The semantic side (`Engine::semantic_pass`) runs only when the query
    /// has a content word — three letters or more, not a stopword — so an
    /// identifier such as `D41` or `#607` is looked up by its characters
    /// alone.
    ///
    /// #132: `total` is the size of the union of every list's matches before
    /// the window, and `has_more` is `total` against what came back.
    pub fn memory_search(&self, p: &Value) -> Result<Value, ApiError> {
        let raw = opt_bool(p, "raw")?.unwrap_or(false);
        let mode = match opt_str(p, "mode")? {
            None if raw => SearchMode::Lexical,
            None => SearchMode::Hybrid,
            Some(m) => SearchMode::parse(&m)?,
        };
        if raw && mode != SearchMode::Lexical {
            return Err(ApiError::bad_request(format!(
                "`raw` is an FTS5 expression, which only the words side runs: it takes \
                 `mode: lexical` (or no `mode`), not `mode: {}` (D196)",
                mode.as_str()
            )));
        }
        let floor = min_similarity(p)?;
        if floor.is_some() && mode == SearchMode::Lexical {
            // D33: a value that changes nothing is refused, not ignored.
            return Err(ApiError::bad_request(
                "`min_similarity` is the floor of the meaning side, which `mode: lexical` \
                 (and `raw`) does not run — use `mode: hybrid` or `semantic`, or drop it",
            ));
        }
        let query = req_str(p, "query")?;
        self.search_fused(
            p,
            &Fusion {
                lexical: match mode {
                    SearchMode::Semantic => None,
                    _ if raw => Some(MatchMode::Raw),
                    _ => Some(MatchMode::Plain),
                },
                any_word: match mode {
                    _ if raw => AnyWord::Never,
                    SearchMode::Hybrid => AnyWord::Fused,
                    SearchMode::Lexical => AnyWord::Fallback,
                    SearchMode::Semantic => AnyWord::Never,
                },
                meaning: (mode != SearchMode::Lexical).then_some(query.as_str()),
                floor: floor.unwrap_or(MEMORY_MIN_SIMILARITY),
                exclude_task: None,
            },
        )
    }

    /// The engine's own path into `memory.search`, widened with two things
    /// no public param exposes: a task whose own annotations are excluded
    /// from every list before the limit and the count run, not after, and a
    /// meaning query separate from the lexical expression.
    ///
    /// `task.brief`'s `derived_memory` passes its raw OR expression as
    /// `query` and the task's title, tags and project as `meaning` — the
    /// refusal of `raw` beside hybrid is for callers, and here the engine
    /// wrote both halves (D196). The task's own notes are not knowledge FOUND
    /// for it (#607), and a caller-visible `exclude_task` parameter would be
    /// one more thing `memory.search` documents for a filter nobody outside
    /// this one caller has a reason to ask for. Filtering the ALREADY-LIMITED
    /// page instead (the first cut of this fix) undercounted both the page
    /// and `total` whenever the task's own notes were dense enough to fill
    /// the slots a sibling's ruling needed — the exact D69 problem D147
    /// itself was ruled against — so the exclusion has to run inside the
    /// queries the limit and the count both read.
    ///
    /// `graph.query` passes no `meaning`: its edges claim the title's words,
    /// so it searches by words alone, all of them, with no any-word list.
    pub(crate) fn memory_search_excluding(
        &self,
        p: &Value,
        exclude_task_id: Option<&str>,
        meaning: Option<&str>,
    ) -> Result<Value, ApiError> {
        let raw = opt_bool(p, "raw")?.unwrap_or(false);
        self.search_fused(
            p,
            &Fusion {
                lexical: Some(if raw {
                    MatchMode::Raw
                } else {
                    MatchMode::Plain
                }),
                any_word: AnyWord::Never,
                meaning,
                floor: MEMORY_MIN_SIMILARITY,
                exclude_task: exclude_task_id,
            },
        )
    }

    /// One search as `f` describes it: its lists, fused, and the page cut
    /// from the fusion. Only the page's rows are read whole.
    fn search_fused(&self, p: &Value, f: &Fusion<'_>) -> Result<Value, ApiError> {
        let window = SearchWindow::parse(p, f.exclude_task)?;

        // The meaning side runs only for a content word (an identifier is
        // looked up by its characters), and — #838's vocabulary gate — only
        // when the model knows at least half of the query's content words as
        // whole tokens: text it spells out in pieces, or has no words for,
        // embeds to a direction near everything, and its "matches" are noise
        // above the floor. It counts as having run only when it compared
        // something; `semantic_skipped` says why when it did not.
        //
        // Content words are the search's own ([`content_words`], three
        // letters or more), and each is asked of the model on its own, so the
        // counts always agree with whether meaning was worth running.
        let profile = f.meaning.map(|m| {
            let words: Vec<String> = content_words(m)
                .filter(|(_, bare)| is_content(bare))
                .map(|(_, bare)| bare)
                .collect();
            let known = words
                .iter()
                .filter(|w| crate::embed::is_known_word(w))
                .count();
            (words.len(), known)
        });
        let skipped = |code: &str, reason: &str| {
            let (content, known) = profile.unwrap_or((0, 0));
            json!({
                "code": code,
                "reason": reason,
                "content_words": content,
                "known_words": known,
            })
        };
        let mut semantic_skipped = Value::Null;
        let query_vector = match (f.meaning, profile) {
            (Some(_), Some((0, _))) => {
                semantic_skipped = skipped("no_content_word", "no word of three letters or more");
                None
            }
            (Some(_), Some((content, known))) if known * 2 < content => {
                semantic_skipped = skipped("unknown_words", "not enough words the model knows");
                None
            }
            (Some(m), Some(_)) => match crate::embed::embed_query(m) {
                Some(v) => Some(crate::embed::QueryVector::new(&v)),
                None => {
                    semantic_skipped = skipped("unknown_words", "no word the model knows");
                    None
                }
            },
            _ => None,
        };
        // Caps are the hybrid fusion's alone: every other search ranks every
        // match, as D41 always has.
        let hybrid = f.any_word == AnyWord::Fused;
        let mut semantic_pass = query_vector.as_ref().map(|q| {
            self.semantic_pass(
                q,
                &vectors::SemanticFilter {
                    scope: &window.scope,
                    project: window.project.as_deref(),
                    include_unscoped: window.include_unscoped,
                    exclude_task: f.exclude_task,
                    min_similarity: f.floor,
                    depth: usize::MAX,
                },
            )
        });
        let semantic = semantic_pass
            .as_mut()
            .filter(|pass| pass.compared > 0)
            .map(|pass| SemanticSide::new(std::mem::take(&mut pass.hits)));
        if let (Some(pass), None) = (&semantic_pass, &semantic) {
            semantic_skipped = if pass.failed {
                skipped(
                    "unavailable",
                    "the meaning index could not be read this time",
                )
            } else {
                skipped("no_vectors", "nothing indexed to compare against")
            };
        }
        let cap = |n: usize| hybrid.then_some(n);

        let mut fell_back = false;
        let (all_words, any_word) = match &f.lexical {
            None => (None, None),
            Some(mode) => {
                let query = req_str(p, "query")?;
                let raw = matches!(mode, MatchMode::Raw);
                // Hybrid asks for every CONTENT word (a hit is not partial
                // for lacking a "the"), and runs the any-word list only when
                // there are two of them to choose between. `mode: lexical`
                // keeps the words as typed: D193 as it was.
                let content: Vec<&str> = {
                    let mut seen = HashSet::new();
                    content_words(&query)
                        .filter(|(_, bare)| seen.insert(bare.clone()))
                        .map(|(word, _)| word)
                        .collect()
                };
                let expr = if raw {
                    query.clone()
                } else if f.any_word == AnyWord::Fused && !content.is_empty() {
                    content
                        .iter()
                        .map(|w| phrase(w))
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    phrase_escape(&query)
                };
                let all = self.lexical_list(&window, expr, raw, cap(ALL_WORDS_DEPTH))?;
                let any = match f.any_word {
                    AnyWord::Never => None,
                    AnyWord::Fallback if !all.keys.is_empty() => None,
                    AnyWord::Fallback => any_word_expr(&query)
                        .map(|e| self.lexical_list(&window, e, false, None))
                        .transpose()?,
                    AnyWord::Fused if content.len() < 2 => None,
                    AnyWord::Fused => any_word_expr(&query)
                        .map(|e| self.lexical_list(&window, e, false, cap(ANY_WORD_DEPTH)))
                        .transpose()?,
                };
                fell_back = f.any_word == AnyWord::Fallback && any.is_some();
                (Some(all), any)
            }
        };

        let all_set: HashSet<&HitKey> = all_words
            .iter()
            .flat_map(|l| l.keys.iter().chain(&l.rest))
            .collect();
        let any_set: HashSet<&HitKey> = any_word
            .iter()
            .flat_map(|l| l.keys.iter().chain(&l.rest))
            .collect();
        let sem_set: HashSet<&HitKey> = semantic.iter().flat_map(|s| &s.keys).collect();
        // `total` counts what every word and meaning matched. An entry that
        // holds only some of the words fills a page beyond them but is not
        // counted — unless nothing else matched, when the any-word matches
        // ARE the answer (D193's fallback).
        let counted: HashSet<&HitKey> = if all_set.is_empty() && sem_set.is_empty() {
            any_set.clone()
        } else {
            all_set.union(&sem_set).copied().collect()
        };
        let total = counted.len();

        let mut lists = Vec::new();
        if let Some(l) = &all_words {
            lists.push(Ranked {
                keys: &l.keys,
                weight: WEIGHT_ALL_WORDS,
            });
        }
        if let Some(l) = &any_word {
            lists.push(Ranked {
                keys: &l.keys,
                // In D193's fallback the any-word list is the only one, and
                // its weight moves nothing.
                weight: WEIGHT_ANY_WORD,
            });
        }
        if let Some(s) = &semantic {
            lists.push(Ranked {
                // The same constant depth as the all-words list: the fusion
                // does bounded work, and `total` still counts every match.
                keys: &s.keys[..s.keys.len().min(cap(ALL_WORDS_DEPTH).unwrap_or(usize::MAX))],
                weight: WEIGHT_SEMANTIC,
            });
        }
        // `via` is read from every match, not from the capped lists the
        // fusion ranks: an entry past a cap that another side found is still
        // `both`.
        let via_of = |k: &HitKey| {
            let lexical = all_set.contains(k) || any_set.contains(k);
            match (lexical, sem_set.contains(k)) {
                (true, true) => Via::Both,
                (true, false) => Via::Lexical,
                _ => Via::Semantic,
            }
        };
        let page: Vec<Fused> = fuse(&lists, &via_of)
            .into_iter()
            .take(window.limit)
            .collect();

        // Whole rows for the page alone, each snippet cut by the expression
        // that found it: all the words when it holds them, any word when not.
        let partial = |k: &HitKey| !all_set.contains(k) && any_set.contains(k);
        // One MATCH pass for every lexical row on the page: the any-word
        // expression matches whatever the all-words one does, so it cuts a
        // snippet for both kinds of hit.
        let lexical_ids: Vec<&HitKey> = page
            .iter()
            .map(|h| &h.key)
            .filter(|k| all_set.contains(k) || any_set.contains(k))
            .collect();
        let mut rows: HashMap<HitKey, Value> = match (&any_word, &all_words) {
            (Some(l), _) => self.lexical_rows(&window, &l.expr, false, &lexical_ids)?,
            (None, Some(l)) => {
                let raw = matches!(f.lexical, Some(MatchMode::Raw));
                self.lexical_rows(&window, &l.expr, raw, &lexical_ids)?
            }
            (None, None) => HashMap::new(),
        };
        let below_floor: HashMap<(vectors::Kind, &str), f64> = match &query_vector {
            Some(q) if semantic.is_some() => {
                let keys: HashSet<(vectors::Kind, &str)> = page
                    .iter()
                    .filter(|h| !sem_set.contains(&h.key))
                    .map(|h| (h.key.kind, h.key.id.as_str()))
                    .collect();
                if keys.is_empty() {
                    HashMap::new()
                } else {
                    match &semantic_pass {
                        Some(pass) => self.semantic_similarities(q, &keys, pass),
                        None => HashMap::new(),
                    }
                }
            }
            _ => HashMap::new(),
        };

        let mut hits = Vec::with_capacity(page.len());
        let mut counted_shown = 0;
        for fused in &page {
            let row = match rows.remove(&fused.key) {
                Some(row) => Some(row),
                None => self.semantic_only_row(&fused.key, semantic.as_ref())?,
            };
            // An entry deleted between the list and the row has no row;
            // this connection cannot race itself, but a row is not assumed.
            let Some(mut row) = row else { continue };
            let similarity = semantic
                .as_ref()
                .and_then(|s| s.similarity.get(&fused.key).copied())
                .or_else(|| {
                    below_floor
                        .get(&(fused.key.kind, fused.key.id.as_str()))
                        .copied()
                });
            let is_partial = partial(&fused.key);
            if counted.contains(&fused.key) {
                counted_shown += 1;
            }
            row["via"] = json!(fused.via.as_str());
            row["partial"] = json!(is_partial);
            row["similarity"] = json!(similarity);
            row["score"] = json!(fused.score);
            // D154's contract: a number, lower is better, not a threshold.
            // Negated, so a client sorting ascending gets the result's order.
            row["rank"] = json!(-fused.score);
            hits.push(row);
        }

        // D69: `matched` is the words expression that decided the words
        // side — D193's OR when it stood in for the all-words search, as it
        // always has; null when no words expression ran (`mode: semantic`).
        let any_word_ran = any_word.is_some();
        let matched = if fell_back {
            any_word.as_ref().map(|l| l.expr.clone())
        } else {
            all_words.as_ref().map(|l| l.expr.clone())
        };
        Ok(json!({
            "count": hits.len(),
            "total": total,
            // Whether a COUNTED match was left behind (#132): partial hits
            // past `total` fill a page without making it longer.
            "has_more": counted_shown < total,
            "hits": hits,
            "matched": matched,
            // The any-word expression, when it ran (#838).
            "matched_any": any_word.map(|l| l.expr),
            // D193: no entry held every word, and the any-word list ran —
            // a fact about the store, not about which hits the page holds.
            "relaxed": fell_back || (all_set.is_empty() && any_word_ran),
            // D196: the meaning side's model and floor, or null when it did
            // not run or compared nothing.
            "semantic": semantic.as_ref().map(|_| json!({
                "model": crate::embed::MODEL_ID,
                "min_similarity": f.floor,
            })),
            // #838: why meaning did not run when it was asked for — null
            // when it ran, and when it was not asked (`mode: lexical`).
            "semantic_skipped": semantic_skipped,
        }))
    }

    /// The FTS5 arms of a lexical query over `w`, joined for its scope, with
    /// `docs_cols`/`ann_cols` selected and `extra` appended to each arm's
    /// WHERE. Named params: `:match`, and `:project`/`:exclude_task` when
    /// [`Self::lexical_params`] binds them.
    fn lexical_sql(
        w: &SearchWindow,
        docs_cols: &str,
        ann_cols: &str,
        extra: (&str, &str),
    ) -> String {
        // #134: a doc's own `project` column vs. its task's `project` for an
        // annotation. `IS NULL`, not `IS NOT :project`: the widening admits
        // documents belonging to NO project, never another project's.
        let (d_where, a_where) = match (&w.project, w.include_unscoped) {
            (None, _) => ("", ""),
            (Some(_), true) => (
                " AND (d.project = :project OR d.project IS NULL)",
                " AND (t.project = :project OR t.project IS NULL)",
            ),
            (Some(_), false) => (" AND d.project = :project", " AND t.project = :project"),
        };
        // The exclusion runs INSIDE the annotation arm, ahead of the page and
        // the count both — never as a filter over a page already cut.
        let exclude = if w.exclude_task.is_some() {
            " AND a.task_id <> :exclude_task"
        } else {
            ""
        };
        let docs = format!(
            "SELECT {docs_cols} FROM docs_fts JOIN docs d ON d.rowid = docs_fts.rowid \
             WHERE docs_fts MATCH :match{d_where}{}",
            extra.0
        );
        let ann = format!(
            "SELECT {ann_cols} FROM annotations_fts \
             JOIN annotations a ON a.rowid = annotations_fts.rowid \
             JOIN tasks t ON t.id = a.task_id \
             WHERE annotations_fts MATCH :match{a_where}{exclude}{}",
            extra.1
        );
        match w.scope.as_str() {
            "docs" => docs,
            "annotations" => ann,
            _ => format!("{docs} UNION ALL {ann}"),
        }
    }

    /// The params [`Self::lexical_sql`]'s text reads, beside `:match`.
    fn lexical_params<'p>(
        w: &'p SearchWindow,
        expr: &'p dyn rusqlite::ToSql,
    ) -> Vec<(&'static str, &'p dyn rusqlite::ToSql)> {
        let mut named: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":match", expr)];
        if let Some(proj) = &w.project {
            named.push((":project", proj));
        }
        // Bound only when the arm that reads it is part of the SQL: binding a
        // name the statement does not have is its own error.
        if w.scope != "docs" {
            if let Some(ex) = &w.exclude_task {
                named.push((":exclude_task", ex));
            }
        }
        named
    }

    /// A lexical list: every entry `expr` matches in `w`, best bm25 first.
    /// Keys only — the rows a page shows are read by [`Self::lexical_rows`].
    fn lexical_list(
        &self,
        w: &SearchWindow,
        expr: String,
        raw: bool,
        cap: Option<usize>,
    ) -> Result<LexicalList, ApiError> {
        // `bm25()` is aliased `score`, not `rank`: `rank` is a live column on
        // every FTS5 table and shadowing it inside a compound SELECT is asking
        // for a quiet resolution surprise. Lower bm25 = better, so ORDER BY
        // ASC, then `(kind, id)`: equal scores must not fall to rowid order,
        // which an export and import renumbers (#838).
        let limit = cap.map_or(String::new(), |n| format!(" LIMIT {n}"));
        let sql = format!(
            "{} ORDER BY score, kind, id{limit}",
            Self::lexical_sql(
                w,
                "d.id AS id, 'doc' AS kind, bm25(docs_fts) AS score",
                "a.id AS id, 'annotation' AS kind, bm25(annotations_fts) AS score",
                ("", ""),
            )
        );
        let named = Self::lexical_params(w, &expr);
        let run = |sql: &str| -> rusqlite::Result<Vec<HitKey>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(named.as_slice(), |r| {
                Ok(HitKey::parse(&r.get::<_, String>(1)?, r.get(0)?))
            })?;
            rows.collect()
        };
        let keys = match run(&sql) {
            Ok(keys) => keys,
            // In raw mode the MATCH expression is caller input, so a query
            // SQLite refuses is the caller's error — surfaced with SQLite's
            // own message, never as ok-empty and never as `internal`. #228.5:
            // a `col:query` typo names the columns this scope does have.
            Err(e) if raw => return Err(ApiError::bad_request(raw_fts5_error(&e, &w.scope))),
            Err(e) => return Err(e.into()),
        };
        // A list that filled its cap may have more: the rest of the match
        // set, read without bm25 or an ORDER BY.
        let rest: HashSet<HitKey> = if cap.is_some_and(|n| keys.len() == n) {
            let ranked: HashSet<&HitKey> = keys.iter().collect();
            run(&Self::lexical_sql(
                w,
                "d.id AS id, 'doc' AS kind",
                "a.id AS id, 'annotation' AS kind",
                ("", ""),
            ))?
            .into_iter()
            .filter(|k| !ranked.contains(k))
            .collect()
        } else {
            HashSet::new()
        };
        drop(named);
        Ok(LexicalList { expr, keys, rest })
    }

    /// The whole rows of `keys`, as `expr` matches them: the snippet cut
    /// around what matched, `stale` read off the doc's origin (#790).
    fn lexical_rows(
        &self,
        w: &SearchWindow,
        expr: &str,
        raw: bool,
        keys: &[&HitKey],
    ) -> Result<HashMap<HitKey, Value>, ApiError> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let ids = json!(keys.iter().map(|k| k.id.as_str()).collect::<Vec<_>>()).to_string();
        let sql = Self::lexical_sql(
            w,
            &format!(
                "d.id AS id, 'doc' AS kind, d.title AS title, d.source AS source, \
                 snippet(docs_fts, 1, '', '', '…', {SNIPPET_TOKENS}) AS snip, \
                 0.0 AS score, d.standing AS standing, d.project AS project, \
                 d.origin_path AS origin_path, d.origin_mtime AS origin_mtime, \
                 d.origin_size AS origin_size, d.body AS body"
            ),
            &format!(
                "a.id AS id, 'annotation' AS kind, t.title AS title, \
                 'task:#' || t.short_id AS source, \
                 snippet(annotations_fts, 0, '', '', '…', {SNIPPET_TOKENS}) AS snip, \
                 0.0 AS score, NULL AS standing, t.project AS project, \
                 NULL AS origin_path, NULL AS origin_mtime, NULL AS origin_size, NULL AS body"
            ),
            (
                " AND d.id IN (SELECT value FROM json_each(:ids))",
                " AND a.id IN (SELECT value FROM json_each(:ids))",
            ),
        );
        let expr = expr.to_string();
        let mut named = Self::lexical_params(w, &expr);
        named.push((":ids", &ids));
        let run = || -> rusqlite::Result<HashMap<HitKey, Value>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(named.as_slice(), |r| self.hit_row(r))?;
            rows.collect()
        };
        match run() {
            Ok(rows) => Ok(rows),
            Err(e) if raw => Err(ApiError::bad_request(raw_fts5_error(&e, &w.scope))),
            Err(e) => Err(e.into()),
        }
    }

    /// One hit as the lexical arms select it (the column order of
    /// `lexical_rows`), keyed. `rank` here is a placeholder; the fusion
    /// sets it.
    fn hit_row(&self, r: &rusqlite::Row<'_>) -> rusqlite::Result<(HitKey, Value)> {
        let id: String = r.get(0)?;
        let kind: String = r.get(1)?;
        let title: String = r.get(2)?;
        let origin_path: Option<String> = r.get(8)?;
        let origin_mtime: Option<i64> = r.get(9)?;
        let origin_size: Option<i64> = r.get(10)?;
        let body: Option<String> = r.get(11)?;
        // #790: a doc hit says whether the file it was imported from has
        // moved on since — computed for the page alone, never a store-wide
        // scan. `null` for an annotation (no such file) and for a doc
        // `memory.add` wrote (no `origin_path` to compare).
        let stale = if kind == "doc" {
            origin_path.as_deref().and_then(|path| {
                self.origin_changed(
                    path,
                    origin_mtime,
                    origin_size,
                    &title,
                    body.as_deref().unwrap_or(""),
                )
            })
        } else {
            None
        };
        let row = json!({
            "id": id,
            "kind": kind,
            "title": title,
            "source": r.get::<_, Option<String>>(3)?,
            "snippet": r.get::<_, String>(4)?,
            "rank": r.get::<_, f64>(5)?,
            // #101: a doc hit says whether it is standing; an annotation hit
            // is `null`, because the flag is a property of a doc.
            "standing": r.get::<_, Option<i64>>(6)?.map(|n| n != 0),
            // #657: which project this hit belongs to — `null` for global
            // knowledge.
            "project": r.get::<_, Option<String>>(7)?,
            "stale": stale,
        });
        Ok((HitKey::parse(&kind, id), row))
    }

    /// A hit only the semantic side found: the same row a lexical hit has,
    /// read by id, with its best chunk as the snippet (D196).
    fn semantic_only_row(
        &self,
        key: &HitKey,
        semantic: Option<&SemanticSide>,
    ) -> Result<Option<Value>, ApiError> {
        let sql = match key.kind {
            vectors::Kind::Doc => {
                "SELECT d.id, 'doc', d.title, d.source, '', 0.0, d.standing, d.project, \
                 d.origin_path, d.origin_mtime, d.origin_size, d.body FROM docs d WHERE d.id = ?1"
            }
            vectors::Kind::Annotation => {
                "SELECT a.id, 'annotation', t.title, 'task:#' || t.short_id, '', 0.0, NULL, \
                 t.project, NULL, NULL, NULL, NULL FROM annotations a \
                 JOIN tasks t ON t.id = a.task_id WHERE a.id = ?1 AND a.removed IS NULL"
            }
        };
        let Some((_, mut row)) = self
            .conn
            .query_row(sql, params![key.id], |r| self.hit_row(r))
            .optional()?
        else {
            return Ok(None);
        };
        let chunk = semantic.and_then(|s| s.chunk.get(key).copied());
        if let Some(ix) = chunk {
            if let Some(text) = self.semantic_snippet(key.kind, &key.id, ix) {
                let title = row["title"].as_str().unwrap_or_default().to_string();
                row["snippet"] = json!(chunk_snippet(key.kind, &title, ix, &text));
            }
        }
        Ok(Some(row))
    }

    // ---- memory.get ----------------------------------------------------------

    /// `memory.get` — read one knowledge document whole, by the `id`
    /// `memory.search` printed.
    ///
    /// The subsystem could store prose and could not hand it back. A search hit
    /// carries a `snippet()` excerpt — measured at 60 to 88 characters on real
    /// documents — plus an id that no verb accepted, so a doc written over MCP
    /// in the morning was, by the afternoon, findable and unreadable. Its
    /// author had no route to it at all; a reader on the same machine had
    /// `store.export`, which dumps every task, project and document in the
    /// store to recover one of them, and which `UNEXPOSED_METHODS` withholds
    /// from MCP for exactly that size. That table also already concedes the
    /// client is elsewhere — it withholds `memory.import` because "the
    /// filesystem the CLI reads is not the one an MCP client is on".
    ///
    /// This is D64's asymmetry one noun over. That entry fixed writing without
    /// retracting; writing without reading was the other half, and a retrieval
    /// surface whose whole value is that what it returns can be trusted is
    /// worth nothing if what it returns is 8% of the document.
    ///
    /// **Docs only.** An annotation id is refused with the route that does
    /// work rather than served here: annotations already have a read path
    /// (`memory.search` names their task as `task:#<short_id>`, and `task.get`
    /// returns the bodies), and a second spelling of a reachable behaviour is
    /// the drift D30 warns about. The refusal names `task.get`, because an id
    /// that is real and rejected with a bare "not found" is the worst of both.
    pub fn memory_get(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
        let found = self
            .conn
            .query_row(
                "SELECT id, source, title, body, created, modified, project, rev, standing, \
                 origin_path, origin_mtime, origin_size \
                 FROM docs WHERE id = ?1",
                params![id],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "source": r.get::<_, Option<String>>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "body": r.get::<_, String>(3)?,
                        "created": r.get::<_, String>(4)?,
                        "modified": r.get::<_, String>(5)?,
                        "project": r.get::<_, Option<String>>(6)?,
                        "_rev": r.get::<_, i64>(7)?,
                        // #101: read as an integer, because SQLite has no
                        // boolean type of its own.
                        "standing": r.get::<_, i64>(8)? != 0,
                        // #788/D180: null on every doc nobody imported from a
                        // file — stated, not omitted, so a client can tell
                        // "no origin" from "this build does not know".
                        "origin_path": r.get::<_, Option<String>>(9)?,
                        "origin_mtime": r.get::<_, Option<i64>>(10)?,
                        "origin_size": r.get::<_, Option<i64>>(11)?,
                    }))
                },
            )
            .optional()?;
        if let Some(doc) = found {
            return Ok(doc);
        }
        // The id might be a real annotation, which `memory.search` returns
        // alongside docs and which this method deliberately does not serve.
        let annotation_task: Option<i64> = self
            .conn
            .query_row(
                "SELECT t.short_id FROM annotations a JOIN tasks t ON t.id = a.task_id \
                 WHERE a.id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Err(match annotation_task {
            Some(short_id) => ApiError::not_found(
                format!(
                    "{id} is an annotation on task #{short_id}, not a memory doc — \
                     read it with task.get on #{short_id}"
                ),
                Some(json!({ "id": id, "task": short_id })),
            ),
            None => ApiError::not_found(format!("no memory doc with id {id}"), None),
        })
    }

    // ---- memory.remove -------------------------------------------------------

    /// `memory.remove` — delete one doc by `id`. An id that matches nothing is
    /// `not_found`, not a silent no-op: "I deleted it" and "there was nothing
    /// there" are different answers to the caller.
    pub fn memory_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
        let tx = self.begin_mutation()?;
        // D197: the event names the doc's `source` as well as its id, because
        // two stores that each imported one file hold it under two ids (D183)
        // and a merge carrying this removal finds the other copy by its
        // source; `project` scopes the event in a filtered export, the way
        // the doc itself was scoped (D171).
        let held: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT source, project FROM docs WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((source, project)) = held else {
            return Err(ApiError::not_found(
                format!("no memory doc with id {id}"),
                None,
            ));
        };
        delete_doc(&tx, &id)?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.remove",
            &json!({ "id": id, "source": source, "project": project }),
        )?;
        tx.commit()?;
        Ok(json!({ "id": id, "removed": true }))
    }

    // ---- memory.list -----------------------------------------------------

    /// `memory.list` — browse memory docs without already knowing a literal
    /// word inside one (#133). Params: `limit`, `offset`, optional `project`
    /// (#134), optional `standing` (#101). Newest-modified-first by default —
    /// the recency a caller browsing "what's in here" wants, and the same axis
    /// `memory.update` moves a doc along.
    ///
    /// Same `{count, total, next_offset}` shape as `task.list` (D70):
    /// `total` is matched rows before the window, `next_offset` is the value
    /// to pass back to keep walking and is `null` once nothing is left. This
    /// is the browse counterpart to `memory.search` — that one requires a
    /// query, this one requires none.
    pub fn memory_list(&self, p: &Value) -> Result<Value, ApiError> {
        let project = opt_str_nonempty(p, "project")?;
        // #101/D156: omitted lists every doc, `true` only the standing ones,
        // `false` only the rest — a filter, not a sort, so "what is standing
        // here?" is one call whose answer does not depend on recency.
        let standing = opt_bool(p, "standing")?;
        let offset = opt_u64(p, "offset")?.unwrap_or(0);
        let offset_i64 = i64::try_from(offset).map_err(|_| {
            ApiError::bad_request(format!(
                "`offset` must be at most {}, or omitted for the default",
                i64::MAX
            ))
        })?;
        let limit = opt_u64(p, "limit")?;
        let limit_i64 = match limit {
            Some(n) => Some(i64::try_from(n).map_err(|_| {
                ApiError::bad_request(format!(
                    "`limit` must be at most {}, or omitted for the default",
                    i64::MAX
                ))
            })?),
            None => None,
        };

        // Two independent narrowings, so the clause is assembled from
        // whichever were given rather than enumerated as four cases. The
        // `standing` half is a literal and not a bound param: it is already a
        // decided bool here, and binding it would put the flag in the param
        // list of every statement, including the calls that name none.
        let mut conds: Vec<&str> = Vec::new();
        if project.is_some() {
            conds.push("project = :project");
        }
        if let Some(flag) = standing {
            conds.push(if flag { "standing = 1" } else { "standing = 0" });
        }
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let count_sql = format!("SELECT COUNT(*) FROM docs {where_clause}");
        // Newest-modified first (D115 #133), `id DESC` as the tiebreak two
        // docs written the same instant still need so they list in one
        // deterministic order (UUIDv7 ids sort chronologically, so it is
        // also a secondary recency signal, not an arbitrary one) — the same
        // reasoning `task.list`'s `compare_by` always ends on a tiebreak.
        // It settles tie order only: a doc updated between two OFFSET pages
        // still moves across the boundary, as on any offset-paged list.
        //
        // The key is `modified` with its `Z` stripped, not the column itself
        // (D144). `util::now` prints a variable-length fractional second —
        // trailing zeros trimmed, absent at a whole second — and under
        // BINARY collation `'Z'` sorts above `'.'` and every digit, so
        // `...10Z` > `...10.9Z` and `...10.12Z` > `...10.123Z`: an older doc
        // above a newer one. Without the terminator a trimmed decimal
        // fraction compares correctly as a plain prefix string (`10` <
        // `10.9`, `10.12` < `10.123`). `id` alone cannot replace it, unlike
        // D142's annotations: `modified` moves on `memory.update` and on an
        // import that replaces by source, and this list promises to show
        // that.
        let row_sql = format!(
            "SELECT id, title, source, project, created, modified, rev, \
                    substr(body, 1, 160) AS preview, length(body) AS body_len, standing \
             FROM docs {where_clause} \
             ORDER BY rtrim(modified, 'Z') DESC, id DESC \
             LIMIT :limit OFFSET :offset"
        );

        let named: Vec<(&str, &dyn rusqlite::ToSql)> = match &project {
            Some(proj) => vec![(":project", proj)],
            None => vec![],
        };
        let total: i64 = self
            .conn
            .query_row(&count_sql, named.as_slice(), |r| r.get(0))?;

        let mut row_params = named.clone();
        // `LIMIT -1` is SQLite's own "unlimited" spelling, used because the
        // param binder needs a concrete value: `limit` omitted must return
        // everything from `offset` on, mirroring `task.list`'s `Option<u64>`
        // without a limit clause at all — sending 0 or a negative literal
        // instead would either answer nothing or (per `memory_search`'s own
        // comment above) answer everything for the wrong reason.
        let limit_bound: i64 = limit_i64.unwrap_or(-1);
        row_params.push((":limit", &limit_bound));
        row_params.push((":offset", &offset_i64));

        let mut stmt = self.conn.prepare(&row_sql)?;
        let rows = stmt.query_map(row_params.as_slice(), |r| {
            let body_len: i64 = r.get(8)?;
            let preview: String = r.get(7)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "source": r.get::<_, Option<String>>(2)?,
                "project": r.get::<_, Option<String>>(3)?,
                "created": r.get::<_, String>(4)?,
                "modified": r.get::<_, String>(5)?,
                "_rev": r.get::<_, i64>(6)?,
                "body_preview": preview,
                "body_truncated": body_len > 160,
                // #101: every row says so, filtered or not — a browse page
                // that carried the flag only when it was asked to filter on
                // it could not show which of a mixed page is standing.
                "standing": r.get::<_, i64>(9)? != 0,
            }))
        })?;
        let docs: Vec<Value> = rows.collect::<Result<_, _>>()?;

        let next_offset = if limit_i64 == Some(0) {
            Value::Null
        } else {
            match offset + docs.len() as u64 {
                reached if (reached as i64) < total => json!(reached),
                _ => Value::Null,
            }
        };

        Ok(json!({
            "count": docs.len(),
            "total": total,
            "next_offset": next_offset,
            "docs": docs,
        }))
    }

    // ---- session rulings (#96, D157) ----------------------------------------

    /// The docs an MCP session is handed at `initialize` (#96, D157): the
    /// inferred project, its standing docs plus the unscoped standing ones,
    /// and the project's own non-standing docs as topical fill.
    ///
    /// Inside a git repo the project is the first of the git toplevel's name
    /// and the main checkout's name (`repo_dir`, for a linked task worktree
    /// whose basename is `<id>-<slug>`) that is a non-archived project; the
    /// workdir's own name is not consulted, so `<repo>/src` is not claimed by
    /// a project called `src`. Outside any repo it is the workdir's own name.
    /// Else the store's default project, else none. Never a bare ancestor
    /// (D218, #810): `/home/runner/work/<repo>` must not be claimed by a
    /// project called `home` or `work`.
    /// Not a dispatched method: its one reader is the MCP handshake.
    pub fn session_rulings(
        &self,
        workdir: Option<&std::path::Path>,
    ) -> Result<SessionRulings, ApiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM projects WHERE archived = 0")?;
        let names: HashSet<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let from_dir = workdir.and_then(|dir| {
            let top = crate::memory_doc::git_toplevel(&dir.join("_"));
            let main = top.as_deref().map(crate::memory_doc::repo_dir);
            let hit = (if top.is_some() {
                [top.as_deref(), main.as_deref()]
            } else {
                [Some(dir), None]
            })
            .into_iter()
            .flatten()
            .filter_map(|a| a.file_name()?.to_str())
            .find(|n| names.contains(*n))
            .map(|n| (n.to_string(), "working directory"));
            hit
        });
        // PR #48 review: a default naming a project this same query just
        // proved archived (or gone) must not be trusted any further than
        // `from_dir` is — checked against the same non-archived `names` set
        // rather than taking `default_project` at its word.
        let project = match from_dir {
            Some(p) => Some(p),
            None => self
                .default_project()?
                .filter(|n| names.contains(n))
                .map(|n| (n, "default project")),
        };
        let name = project.as_ref().map(|(n, _)| n.as_str());

        let docs = |sql: &str| -> Result<Vec<SessionDoc>, ApiError> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map(params![name], |r| {
                Ok(SessionDoc {
                    title: r.get(0)?,
                    body: r.get(1)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        };
        // D144's newest-modified key. `project = NULL` matches nothing, so
        // with no project inferred only the unscoped standing docs remain.
        // PR #48 review: `substr(body, 1, 4096)` — the gist this feeds is
        // the first paragraph, cut at 240 bytes, and 4 KB comfortably covers
        // a frontmatter block ahead of it without loading a doc whole.
        let standing = docs(
            "SELECT title, substr(body, 1, 4096) FROM docs \
             WHERE standing = 1 AND (project = ?1 OR project IS NULL) \
             ORDER BY project IS NULL, rtrim(modified, 'Z') DESC, id DESC",
        )?;
        // Never unscoped: `memory import` stores every ADR unscoped, so the
        // newest unscoped docs are an arbitrary slice (D157).
        // PR #48 review: `LIMIT 256` bounds what session start ever reads —
        // 256 is well above the most entries a 3,072-byte section could ever
        // render, since every rendered entry is at least 4 bytes ("\n- " plus
        // a title).
        let topical = docs(
            "SELECT title, substr(body, 1, 4096) FROM docs \
             WHERE standing = 0 AND project = ?1 \
             ORDER BY rtrim(modified, 'Z') DESC, id DESC \
             LIMIT 256",
        )?;
        let topical_total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM docs WHERE standing = 0 AND project = ?1",
            params![name],
            |r| r.get(0),
        )?;
        Ok(SessionRulings {
            project,
            standing,
            topical,
            topical_total: topical_total as usize,
        })
    }

    // ---- memory.update -----------------------------------------------------

    /// `memory.update` — replace a doc's `title`/`body` in place (#135).
    /// Params: `id`, optional `title`, optional `body`, optional `source`,
    /// optional `project`, optional `standing` (#101), optional
    /// `expected_rev`. At least one of
    /// `title`/`body`/`source`/`project`/`standing` must be given.
    ///
    /// `memory.remove` is genuinely permanent — no `undo`, nothing in the
    /// event log to reconstruct the body from — so a correction had only two
    /// routes: add a second doc (the stale one still pollutes every later
    /// search) or delete-then-recreate (loses the id and the revision
    /// history, and risks the permanent half landing with nothing to follow
    /// it). This is the missing third route: an in-place UPDATE, so the id
    /// stays stable for anything that already cited it.
    ///
    /// `expected_rev` is `task.modify`'s optimistic-concurrency guard,
    /// unchanged: supplied and mismatched is a `conflict` naming both
    /// revs, never a silent overwrite.
    pub fn memory_update(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
        let title = opt_str_nonempty(p, "title")?;
        // Task #12/D135: `body` is stored as given, same as `memory.add` —
        // only `search_body` (below) is derived from it, so a doc corrected
        // to open with a fresh frontmatter block gets the index kept in
        // step without the stored body ever being rewritten.
        let body = opt_str_nonempty(p, "body")?;
        let source = opt_str(p, "source")?;
        let project = opt_str_nonempty(p, "project")?;
        // #101/D156: this is the retraction half of the flag — `standing:
        // false` is how a ruling stops being re-read every session, and it is
        // a change in its own right, so an update that names ONLY it is a
        // complete request and not the "nothing to do" refusal below.
        let standing = opt_bool(p, "standing")?;
        if title.is_none()
            && body.is_none()
            && source.is_none()
            && project.is_none()
            && standing.is_none()
        {
            return Err(ApiError::bad_request(
                "memory.update requires at least one of `title`, `body`, `source`, `project`, \
                 `standing`",
            ));
        }
        let expected_rev = opt_i64(p, "expected_rev")?;

        let tx = self.begin_mutation()?;
        let current = tx
            .query_row(
                "SELECT title, body, source, project, rev, standing FROM docs WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)? != 0,
                    ))
                },
            )
            .optional()?;
        let Some((cur_title, cur_body, cur_source, cur_project, cur_rev, cur_standing)) = current
        else {
            return Err(ApiError::not_found(
                format!("no memory doc with id {id}"),
                None,
            ));
        };

        if let Some(exp) = expected_rev {
            if exp != cur_rev {
                return Err(ApiError::new(
                    crate::ErrorCode::Conflict,
                    // CLI vocabulary here; the MCP transport rewrites this
                    // clause to its own tool and parameter from `data` (#613,
                    // `mcp::mcp_surface_message`), exactly as for `task.modify`.
                    format!(
                        "expected_rev {exp} but doc is at rev {cur_rev}: re-read it with \
                         `tasqx memory show {id} --json` and retry with --expected-rev {cur_rev}"
                    ),
                    Some(json!({ "expected": exp, "current": cur_rev, "id": id })),
                ));
            }
        }

        let new_title = title.clone().unwrap_or(cur_title);
        let new_body = body.clone().unwrap_or(cur_body);
        let new_search_body = crate::frontmatter::flatten(&new_body).into_owned();
        let new_source = source.clone().or(cur_source);
        refuse_source_held_elsewhere(&tx, new_source.as_deref(), &id)?;
        let new_project = project.clone().or(cur_project);
        // #101: unnamed leaves the flag exactly as it was — the same "absent
        // is not `false`" the four fields above hold.
        let new_standing = standing.unwrap_or(cur_standing);
        let new_rev = cur_rev + 1;
        let ts = now();

        tx.execute(
            "UPDATE docs SET title = ?1, body = ?2, search_body = ?3, source = ?4, \
             project = ?5, standing = ?6, rev = ?7, modified = ?8 WHERE id = ?9",
            params![
                new_title,
                new_body,
                new_search_body,
                new_source,
                new_project,
                new_standing,
                new_rev,
                ts,
                id
            ],
        )?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.update",
            &json!({
                "title": &new_title,
                "source": &new_source,
                "project": &new_project,
                "standing": new_standing,
                "rev": new_rev,
            }),
        )?;
        tx.commit()?;

        Ok(json!({
            "id": id,
            "title": new_title,
            "source": new_source,
            "project": new_project,
            "standing": new_standing,
            "_rev": new_rev,
            "modified": ts,
        }))
    }
}

/// Delete doc `id` and its links: `memory.remove`'s write, shared with the
/// merge that carries one (D197).
///
/// D160: a doc is the one graph node this engine HARD deletes, so its
/// explicit links go with it, in this transaction. The `links` table carries
/// no foreign key — its endpoints are polymorphic, so SQLite cannot be asked
/// to enforce this — which makes the cascade the engine's job and leaving it
/// out exactly the dangling-edge shape D12 fixed for `dependencies`:
/// invisible to every reader that resolves both ends, still in the file, and
/// impossible to remove by id nobody can list. Annotations are NOT cascaded:
/// `annotation.remove` leaves a tombstone (D113), so the node is still there
/// to point at. Answers whether a doc was deleted.
pub(super) fn delete_doc(tx: &rusqlite::Transaction, id: &str) -> Result<bool, ApiError> {
    let n = tx.execute("DELETE FROM docs WHERE id = ?1", params![id])?;
    tx.execute(
        "DELETE FROM links WHERE (from_type = 'memory' AND from_id = ?1) \
            OR (to_type = 'memory' AND to_id = ?1)",
        params![id],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::{
        chunk_snippet, content_words, fuse, is_content, vectors, HitKey, Ranked, Via, FUSION_K,
        WEIGHT_ALL_WORDS, WEIGHT_ANY_WORD, WEIGHT_SEMANTIC,
    };
    use rusqlite::params;
    use serde_json::json;

    /// #229 item 4: a malformed identifier was `bad_request` for a task
    /// `ref` (`resolve_ref_value_on`) but `not_found` for a memory id — a
    /// script that retries on `not_found` (transient/wrong id) and gives up
    /// on `bad_request` (malformed input) got the opposite treatment
    /// depending only on which entity it asked about, contradicting
    /// DESIGN.md's stated exit-code-per-class contract. `notauuid` cannot be
    /// a UUID, so it is the same failure as a malformed task ref.
    #[test]
    fn a_malformed_memory_id_is_bad_request_not_not_found() {
        let e = crate::Engine::open_in_memory().unwrap();

        let get_err = e
            .memory_get(&json!({ "id": "notauuid" }))
            .expect_err("a non-UUID id must be refused");
        assert_eq!(
            get_err.code,
            crate::ErrorCode::BadRequest,
            "shape is wrong, not merely absent: {}",
            get_err.message
        );
        assert!(
            get_err.message.contains("UUID"),
            "must say what shape was expected: {}",
            get_err.message
        );

        let rm_err = e
            .memory_remove(&json!({ "id": "notauuid" }))
            .expect_err("a non-UUID id must be refused");
        assert_eq!(
            rm_err.code,
            crate::ErrorCode::BadRequest,
            "{}",
            rm_err.message
        );

        // A well-formed but nonexistent UUID is still genuinely not_found —
        // only the SHAPE check moved, not the "no such doc" answer.
        let real_uuid = crate::clock::uuid_v7().to_string();
        let missing = e.memory_get(&json!({ "id": real_uuid })).unwrap_err();
        assert_eq!(
            missing.code,
            crate::ErrorCode::NotFound,
            "{}",
            missing.message
        );
    }

    /// `n` docs titled `doc {i}` with body `body {i}`, returned as ids in
    /// write order — UUIDv7, so id order is write order.
    fn add_docs(e: &crate::Engine, n: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..n {
            let out = e
                .memory_add(&json!({ "title": format!("doc {i}"), "body": format!("body {i}") }))
                .unwrap();
            ids.push(out["id"].as_str().unwrap().to_string());
        }
        ids
    }

    /// Stamp each `id` with its paired `modified` text directly — writing it
    /// this way is the only way to reproduce a specific spelling without
    /// racing a clock.
    fn stamp_docs(e: &crate::Engine, ids: &[String], stamps: &[&str]) {
        assert_eq!(ids.len(), stamps.len());
        for (i, (id, ts)) in ids.iter().zip(stamps.iter()).enumerate() {
            let n = e
                .conn
                .execute(
                    "UPDATE docs SET modified = ?1 WHERE id = ?2",
                    params![ts, id],
                )
                .unwrap();
            assert_eq!(n, 1, "doc {i} must exist exactly once to be stamped");
        }
    }

    /// The `docs` array's `id` strings, in order.
    fn listed_ids(out: &serde_json::Value) -> Vec<String> {
        out["docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// memory.list is newest-modified first, and the variable-length `modified`
    /// text may not decide it (D142's defect, on docs).
    ///
    /// `util::now` prints a jiff Timestamp whose fractional second is trimmed —
    /// entirely absent on a whole second — and under SQLite's BINARY collation
    /// 'Z' (0x5A) sorts above '.' and above every digit, so `...10Z` >
    /// `...10.1Z` > `...10.12Z` > `...10.123Z` while each is later than the
    /// last. Ordering by that text put a doc stamped on a whole second above
    /// every doc written after it in the same second; the paging test in
    /// tests/memory.rs failed once under load for exactly this reason.
    #[test]
    fn memory_list_orders_by_modified_instant_not_the_variable_length_text() {
        let e = crate::Engine::open_in_memory().unwrap();
        let ids = add_docs(&e, 4);

        // Chronological, and deliberately fully INVERTED in BINARY text
        // order: 'Z' beats '1', '2' and '3'.
        let stamps = [
            "2026-01-01T00:00:10Z",
            "2026-01-01T00:00:10.1Z",
            "2026-01-01T00:00:10.12Z",
            "2026-01-01T00:00:10.123Z",
        ];
        stamp_docs(&e, &ids, &stamps);

        let out = e.memory_list(&json!({})).unwrap();
        let listed = listed_ids(&out);
        let expected: Vec<String> = ids.iter().rev().cloned().collect();
        assert_eq!(
            listed, expected,
            "the newest-modified doc must come first regardless of how its stamp's text sorts"
        );

        let page = e.memory_list(&json!({ "limit": 1 })).unwrap();
        let page_ids = listed_ids(&page);
        assert_eq!(
            page_ids,
            vec![ids[3].clone()],
            "the first page must hold the doc modified last, not the whole-second one"
        );
    }

    /// memory.list is newest-MODIFIED first, not newest-created (D115 #133,
    /// D144): a `memory.update` moves a doc to the top even though its UUIDv7
    /// id stays where it was minted. Ordering by `id` alone would read as
    /// correct on every fresh store and only lie once a doc is edited.
    #[test]
    fn memory_list_puts_an_updated_doc_first_even_though_its_id_is_the_oldest() {
        let e = crate::Engine::open_in_memory().unwrap();
        let ids = add_docs(&e, 3);

        // Strictly increasing, and in write order, so the baseline (before
        // the update below) is unambiguous regardless of how fast the
        // machine that runs this test is.
        let stamps = [
            "2026-01-01T00:00:10Z",
            "2026-01-01T00:00:11Z",
            "2026-01-01T00:00:12Z",
        ];
        stamp_docs(&e, &ids, &stamps);

        // Update the OLDEST id (doc 0) — memory.update sets util::now(),
        // which is later than the crafted stamps, but this pins it
        // explicitly so the test does not depend on the wall clock.
        e.memory_update(&json!({ "id": ids[0], "body": "edited" }))
            .unwrap();
        stamp_docs(&e, &ids[..1], &["2026-01-01T00:00:13Z"]);

        let out = e.memory_list(&json!({})).unwrap();
        let listed = listed_ids(&out);
        assert_eq!(
            listed,
            vec![ids[0].clone(), ids[2].clone(), ids[1].clone()],
            "the edited doc surfaces first; the rest stay newest-modified first"
        );

        let page = e.memory_list(&json!({ "limit": 1 })).unwrap();
        let page_ids = listed_ids(&page);
        assert_eq!(page_ids, vec![ids[0].clone()]);
    }

    /// PR #48 review: `session_rulings` must not trust a default naming a
    /// project the non-archived set no longer contains. `project.archive`
    /// clears a default it owns (D22) and the storage-open repair fixes a
    /// stale one, so reaching this state needs writing `config` directly —
    /// the way a store predating either safeguard would carry it.
    #[test]
    fn session_rulings_ignores_a_default_project_that_no_longer_exists() {
        let e = crate::Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "alpha" })).unwrap();
        e.memory_add(&json!({
            "title": "alpha rule", "body": "scoped", "project": "alpha", "standing": true
        }))
        .unwrap();
        e.project_archive(&json!({ "name": "alpha" })).unwrap();
        e.conn
            .execute(
                "INSERT INTO config (key, value) VALUES ('default_project', 'alpha') \
                 ON CONFLICT(key) DO UPDATE SET value = 'alpha'",
                [],
            )
            .unwrap();

        let r = e.session_rulings(None).unwrap();
        assert!(r.project.is_none(), "stale default must not be trusted");
        assert!(r.standing.is_empty());
    }

    fn has_content_word(query: &str) -> bool {
        content_words(query).any(|(_, bare)| is_content(&bare))
    }

    /// [`fuse`] with `via` read from which lists hold a key: `true` marks a
    /// words list.
    fn fuse_lists(lists: &[(Ranked<'_>, bool)]) -> Vec<super::Fused> {
        let on = |lexical: bool| -> std::collections::HashSet<&HitKey> {
            lists
                .iter()
                .filter(|(_, l)| *l == lexical)
                .flat_map(|(r, _)| r.keys)
                .collect()
        };
        let (lex, sem) = (on(true), on(false));
        let ranked: Vec<Ranked<'_>> = lists
            .iter()
            .map(|(r, _)| Ranked {
                keys: r.keys,
                weight: r.weight,
            })
            .collect();
        fuse(&ranked, &|k| match (lex.contains(k), sem.contains(k)) {
            (true, true) => Via::Both,
            (true, false) => Via::Lexical,
            _ => Via::Semantic,
        })
    }

    fn key(kind: &str, id: &str) -> HitKey {
        HitKey::parse(kind, id.to_string())
    }

    fn words(keys: &[HitKey]) -> (Ranked<'_>, bool) {
        (
            Ranked {
                keys,
                weight: WEIGHT_ALL_WORDS,
            },
            true,
        )
    }

    fn any(keys: &[HitKey]) -> (Ranked<'_>, bool) {
        (
            Ranked {
                keys,
                weight: WEIGHT_ANY_WORD,
            },
            true,
        )
    }

    fn meaning(keys: &[HitKey]) -> (Ranked<'_>, bool) {
        (
            Ranked {
                keys,
                weight: WEIGHT_SEMANTIC,
            },
            false,
        )
    }

    /// D196's tie order: equal fused scores go to `both`, then `lexical`,
    /// then `semantic`, then `(kind, id)` — never to whichever the map
    /// happened to yield first.
    #[test]
    fn fusion_ties_go_to_both_then_lexical_then_semantic_then_kind_and_id() {
        // Equal weights make a tie: first on the words list and first on an
        // equally weighted meaning list.
        let l = [key("doc", "z-lex")];
        let m = [key("annotation", "a-sem")];
        let fused = fuse_lists(&[
            words(&l),
            (
                Ranked {
                    keys: &m,
                    weight: WEIGHT_ALL_WORDS,
                },
                false,
            ),
        ]);
        let order: Vec<(&str, Via)> = fused.iter().map(|f| (f.key.id.as_str(), f.via)).collect();
        assert_eq!(
            order,
            [("z-lex", Via::Lexical), ("a-sem", Via::Semantic)],
            "lexical wins a tie with semantic, whatever the ids"
        );
        assert_eq!(fused[0].score, fused[1].score);

        // Two lists over the same two keys in crossed order and equal
        // weight: every score ties, so the tie falls to (kind, id).
        let l = [key("doc", "b"), key("annotation", "c")];
        let m = [key("annotation", "c"), key("doc", "b")];
        let fused = fuse_lists(&[
            words(&l),
            (
                Ranked {
                    keys: &m,
                    weight: WEIGHT_ALL_WORDS,
                },
                false,
            ),
        ]);
        let order: Vec<&str> = fused.iter().map(|f| f.key.id.as_str()).collect();
        assert_eq!(order, ["c", "b"], "annotation sorts before doc");
        assert!(fused.iter().all(|f| f.via == Via::Both));
    }

    /// #838's tuned weights: meaning outweighs words at the same rank, and
    /// the any-word list is a tiebreaker, never a rival — its first hit
    /// scores below the all-words list's tenth.
    #[test]
    fn the_tuned_weights_rank_meaning_then_every_word_then_any_word() {
        let l: Vec<HitKey> = (0..10).map(|i| key("doc", &format!("w{i}"))).collect();
        let a = [key("doc", "a0")];
        let m = [key("doc", "m0")];
        let fused = fuse_lists(&[words(&l), any(&a), meaning(&m)]);
        let order: Vec<&str> = fused.iter().map(|f| f.key.id.as_str()).collect();
        assert_eq!(order.first(), Some(&"m0"));
        assert_eq!(order.last(), Some(&"a0"));
        assert_eq!(fused[0].score, WEIGHT_SEMANTIC / (FUSION_K + 1.0));
        // An entry on both lexical lists and the meaning list is `both`.
        let fused = fuse_lists(&[words(&l[..1]), any(&l[..1]), meaning(&l[..1])]);
        assert_eq!(fused[0].via, Via::Both);
        assert_eq!(
            fused[0].score,
            WEIGHT_ALL_WORDS / (FUSION_K + 1.0)
                + WEIGHT_ANY_WORD / (FUSION_K + 1.0)
                + WEIGHT_SEMANTIC / (FUSION_K + 1.0)
        );
    }

    /// A list of one side alone keeps its own order, so `mode: lexical`
    /// returns bm25's.
    #[test]
    fn fusion_of_one_list_keeps_its_order() {
        let keys: Vec<HitKey> = ["q", "a", "m", "b"].iter().map(|i| key("doc", i)).collect();
        let fused = fuse_lists(&[words(&keys)]);
        let order: Vec<&str> = fused.iter().map(|f| f.key.id.as_str()).collect();
        assert_eq!(order, ["q", "a", "m", "b"]);
    }

    #[test]
    fn a_content_word_has_three_letters_and_is_not_a_stopword() {
        for q in [
            "D41", "#607", "v2.3.1", "the and", "a to", "D41 #607", "SQ1",
        ] {
            assert!(!has_content_word(q), "{q}");
        }
        for q in ["SSO", "sign-in", "the release", "D41 rules", "\"ledger\""] {
            assert!(has_content_word(q), "{q}");
        }
    }

    #[test]
    fn a_chunk_snippet_is_cut_like_an_fts5_one() {
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen";
        assert_eq!(
            chunk_snippet(vectors::Kind::Annotation, "", 0, long),
            "one two three four five six seven eight nine ten eleven twelve…"
        );
        assert_eq!(
            chunk_snippet(vectors::Kind::Annotation, "", 2, "short\n\ntext"),
            "…short text",
            "a later chunk is text left out before it"
        );
        assert_eq!(
            chunk_snippet(vectors::Kind::Doc, "Title", 0, "Title\n\nthe body"),
            "the body",
            "the title the hit already carries is not repeated"
        );
        assert_eq!(
            chunk_snippet(vectors::Kind::Doc, "Title", 0, "Title"),
            "Title",
            "a doc with no body is its title"
        );
    }
}
