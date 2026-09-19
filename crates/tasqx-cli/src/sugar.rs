//! Inline capture sugar for `tasqx add` / `modify` (DESIGN.md §5, §10).
//!
//! Extracts structured fields from the free-text title, leaving the remaining
//! words as the title:
//!  * `+tag` — stored lowercased by the core; a tag containing whitespace
//!    (`+"needs paint"`) is split out whole here and then refused there (D172).
//!  * `project:<name>` / `proj:<name>`
//!  * `!<prio>` (`!high`, `!h`)
//!  * date keys `due:` / `scheduled:` / `wait:` — values are **natural-language
//!    date expressions** resolved later by [`tasqx_core::datetime`]; quote to
//!    include spaces, e.g. `due:"in 3 days"` or `due:friday`.
//!  * recurrence keys `repeat:` / `every:` — a D2 rule string, e.g.
//!    `repeat:"every 3 days"` or `every:"weekly on mon,wed,fri"`.
//!  * reminder key `remind:` — either a `due`-anchored offset (`remind:-1h`,
//!    `remind:-30m`) or an absolute date expression (`remind:"friday 9am"`).
//!    §9's quiet-by-default rule means this key is the *only* thing that puts a
//!    task on the reminder heap.
//!  * estimate keys `est:` / `estimate:` — a human duration (`est:4h`,
//!    `est:1h30m`), resolved by [`tasqx_core::datetime::parse_duration`].
//!
//! Quoting is ONE rule, not a write-side dialect: this module splits with
//! [`tasqx_core::filter::split_words`], the scanner the read-side grammar
//! documents and owns. So `\"` is a literal quote and `\\` a literal backslash
//! here exactly as in a filter, an unterminated quote is refused here exactly as
//! there, and any value `filter::quote` can emit is a value `add` can type.
//! Before they shared a scanner they disagreed: `add '+say"hi'` stored the tag
//! `sayhi` under a zero exit code, and a value containing a quote could not be
//! written at all — which made the escape the read side documents unmatchable.
//!
//! Two spellings reach the same value, as on the read side: the shell-stripped
//! `project:"Home Renovation"` (the shell eats the quotes and the argument
//! boundary tells us it is one value) and the literal `project:"Home
//! Renovation"` passed through intact. The one value that cannot use the first
//! spelling is one containing a `"` — an element carrying a literal quote goes
//! to the scanner on both sides, so `project:'My "Big" Project'` is three tokens
//! on both. Its spelling is the escaped one, `project:"My \"Big\" Project"`, and
//! that is the same on both sides too.
//!
//! A key needs a value to be a key, and its colon must stand alone. `due:` with
//! nothing after it is the word `due:`, and `recur::advance_once` is a Rust
//! path, not the recurrence rule `:advance_once` — every one of these keys is
//! also a plausible first segment of a module path, which is the vocabulary this
//! project's own tasks are written in. A `+` naming no tag is likewise the
//! character `+`, as in `Display + Error`. Sugar that declines a token LEAVES
//! it, in the title, spelled as typed; it never consumes and discards one.
//!
//! Explicit flags win over inline sugar. Date/recurrence/reminder/estimate
//! *values* are carried out verbatim (unparsed); the caller resolves them through
//! the one core parser so sugar and flags share identical parsing.
//!
//! `add` and `modify` share this parser exactly (DESIGN.md §12-D13): the same
//! token means the same thing in both, and only the *absence* of a token differs
//! — for `add` it means "no value", for `modify` it means "leave this field
//! alone". Clearing is therefore not expressible here; it is `--clear <field>`.

use tasqx_core::{ApiError, Priority};

// The token recogniser lives in core so `task.add`/`task.modify` can name the
// sugar an MCP/JSON title carries without restating its grammar (#621).
pub(crate) use tasqx_core::sugar::ValueKey;
use tasqx_core::sugar::{is_sugar_token, split_key, tag_of, VALUE_KEYS};

pub struct ParsedAdd {
    pub title: String,
    pub project: Option<String>,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    /// Raw NL date expressions (unresolved) — the caller parses these.
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub wait: Option<String>,
    /// Raw recurrence rule string (unvalidated) — the core validates it.
    pub recurrence: Option<String>,
    /// Raw reminder spec (unvalidated): a `due`-anchored offset or an absolute
    /// date expression. The core validates + normalizes it (§9).
    pub remind: Option<String>,
    /// Raw estimate (unparsed), e.g. `4h` — the caller resolves it to ISO-8601.
    pub estimate: Option<String>,
    /// Raw tracked-time correction (unparsed), e.g. `2h30m` — `modify`-only
    /// (D98): there is no inline sugar for it, so this is always exactly
    /// [`AddFlags::tracked`] passed through, never filled by the scanner
    /// below.
    pub tracked: Option<String>,
    /// [`AddFlags::budget_tokens`] passed through. There is no sugar key for
    /// it, so the scanner never fills this.
    pub budget_tokens: Option<i64>,
    /// The project name came from an UNQUOTED `project:`/`proj:` sugar token,
    /// which ends at the first space. If the core then cannot find that name we
    /// genuinely do not know whether it is a typo or the first word of a longer
    /// name the tokenizer never saw, and the message must not pick one and state
    /// it as fact — `project:My "Big" Project` used to answer `no project named
    /// My (create it with `tasqx init My`)` about a project that existed.
    /// False when the name was quoted or came from `--project`: those are whole
    /// by construction, so a miss there really is a typo.
    ///
    /// Also false when NOTHING followed the token. The tokenizer can only cut a
    /// name short by leaving the rest of it in a LATER token, so `add task
    /// project:Zzz` — where `project:` is last — had nothing to lose, and
    /// hedging there described a cut that could not have happened and sent the
    /// user looking for a longer name that never existed.
    pub project_may_be_truncated: bool,
}

/// Which verb is driving [`parse_add`].
///
/// Every rule in this module is identical between `add` and `modify` (D13) —
/// this is the one deliberate exception, and it exists because of what a
/// leftover title word MEANS on each verb. On `add` there is no prior title
/// to lose, so a `key:value`-shaped word neither verb recognises is harmless
/// prose that still gets stored, exit 0 — D45's fall-through, narrowed by
/// D108 to add a stderr warning (never a refusal) so the mistake is at least
/// visible: a blanket refusal here would break real prose this project's own
/// tasks are written in (`recur::advance_once`, `note:`, `C:\path`, a ratio).
/// On `modify` that same word REPLACES whatever the task was already called,
/// `undo` cannot reach a `modify` event's replaced value (D54 — it records
/// only what was SET), and `status:`/`priority:` are real, documented FILTER
/// grammar, so the vocabulary itself teaches the mistake — sharp enough there
/// to earn an outright refusal (D83) rather than a warning. See
/// [`declined_key_shape`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseContext {
    Add,
    Modify,
}

/// Flags supplied explicitly on the command line. Each wins over inline sugar.
#[derive(Default)]
pub struct AddFlags {
    pub project: Option<String>,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub wait: Option<String>,
    pub repeat: Option<String>,
    pub remind: Option<String>,
    pub estimate: Option<String>,
    /// `modify`-only (D98); `add` always passes `None`. See
    /// [`ParsedAdd::tracked`].
    pub tracked: Option<String>,
    /// D139's size gauge. A flag only — there is deliberately no capture-sugar
    /// key for it: the sugar exists for what a person types dozens of times a
    /// day, and a token budget is not that.
    pub budget_tokens: Option<i64>,
}

/// The key spellings alone, for the docs drift guard: the guide's `add` table
/// restates these, and a restated list nothing compares is how `recur:` shipped
/// as a parser alias no documentation named.
#[cfg(test)]
pub(crate) fn value_key_spellings() -> Vec<&'static str> {
    VALUE_KEYS.iter().map(|(k, _)| *k).collect()
}

/// The clap positionals whose words reach [`parse_add`], as
/// `(command path, positional id)`.
///
/// A registry in the shape [`crate::argv::FILTER_COMMANDS`] uses, and for the
/// same reason: the set exists — `lib.rs` calls [`parse_add`] from `run_add` and
/// `run_modify` and nowhere else — but until now it was expressed only as two
/// call sites, which is a set nothing can iterate. Shell completion needs to
/// iterate it, because the sugar prefix dispatcher must be attached to every
/// member and a member with no dispatcher is `tasqx add +<TAB>` silently
/// offering nothing.
///
/// Kept honest in the direction that matters by
/// `complete::candidates::tests::every_sugar_positional_offers_sugar`, which
/// pairs this list against a SECOND, independent signal — the positionals whose
/// help text says `inline sugar` — and fails the build when they disagree. A
/// third verb taking sugar tomorrow lands in one of the two and is a red build
/// either way; adding it to both is the fix, and the guard says so.
///
/// The path, not the bare verb name: `memory add` is also called `add` and also
/// declares a positional called `title`, so a bare name is not an identity in
/// this tree.
///
/// `#[cfg(test)]` for the same reason [`value_key_spellings`] is, and it is
/// worth saying why that is not a dodge. Nothing on the command path iterates
/// this set — `lib.rs` reaches [`parse_add`] by calling it, twice, directly — so
/// compiling the constant into the binary would add a `dead_code` allow and no
/// behaviour. Its authority comes from WHERE it lives, next to the parser whose
/// call sites it names, which is where somebody adding a third one is already
/// looking. What must not be hand-kept is a list of verbs *inside the test*,
/// and that is precisely what this exists to avoid.
#[cfg(test)]
pub(crate) const SUGAR_POSITIONALS: [(&str, &str); 2] = [("add", "title"), ("modify", "rest")];

/// Parse argv words plus explicit flags into structured fields.
///
/// Taking the argv **slice** rather than a pre-joined string is load-bearing.
/// `tasqx modify 4 repeat:"every 3 days"` reaches us as the single argv element
/// `repeat:every 3 days` — the shell already consumed the quotes and told us,
/// by way of the argument boundary, that this is one value. Joining argv back
/// into a string and re-splitting on whitespace threw that away and silently
/// mis-parsed: `project:"my big project"` set the project to `my` and renamed
/// the task to `big project`, with no error at all.
///
/// So: an element that opens with a value key that OWNS a value — or with `+`,
/// which is a value key spelled without a colon (C2) — and carries spaces (and
/// no embedded quotes of its own) is honored whole. Everything else is tokenized
/// as before, which keeps the classic one-big-quoted-string capture form —
/// `add "Ship it due:friday +api"` — parsing exactly as it always has.
/// "Owns a value" is [`split_key`]'s judgement and not `starts_with`'s: an
/// element the loop will hand to the title must not first be honored whole here.
///
/// The trade: `add "due:friday Ship it"` (a value key opening a quoted title)
/// now reads the whole remainder as the date and fails with a clean
/// `could not parse date: "friday Ship it"`. Title-first is the documented and
/// universally exemplified form; a loud error on the rare inversion is a better
/// deal than silent corruption on the common quoted value.
///
/// C6: an unusable token is refused, never dropped. `!urgent` used to be
/// consumed by the `!` branch, fail `normalize_prio`, and vanish — not applied,
/// not reported, not even left in the title. See [`normalize_prio`].
///
/// C6 has a quieter twin, and every arm below now obeys it: a token a sugar arm
/// declines must reach the TITLE. The arms used to claim a token on its first
/// character and then drop it on failing their own inner check, so a bare `+`
/// and a valueless `due:` were deleted outright at exit 0. The claim and the
/// check are now one decision — [`tag_of`] and [`split_key`] — so there is no
/// longer a state between "claimed" and "used". Only `!` still refuses loudly
/// instead, because a bang-word has no escape into title text.
pub fn parse_add(
    args: &[String],
    flags: AddFlags,
    ctx: ParseContext,
) -> Result<ParsedAdd, ApiError> {
    let mut title_words: Vec<String> = Vec::new();
    let mut tags: Vec<String> = flags.tags;
    let mut project = flags.project;
    let mut priority = flags.priority;
    let mut due = flags.due;
    let mut scheduled = flags.scheduled;
    let mut wait = flags.wait;
    // `every:X` is a shorthand for the rule `every X`; `repeat:X` is the full rule.
    let mut recurrence = flags.repeat;
    let mut remind = flags.remind;
    let mut estimate = flags.estimate;
    let mut project_may_be_truncated = false;

    // Collected rather than streamed because the truncation hedge is a question
    // about POSITION: a `project:` token can only have been cut short if some
    // later token holds the rest of the name, and that is unknowable until the
    // end of the stream.
    let toks = tokenize_argv(args)?;
    let last_tok = toks.len().saturating_sub(1);

    for (i, SugarTok { text: tok, quoted }) in toks.into_iter().enumerate() {
        if let Some(lit) = escaped_sugar_char(&tok) {
            // `\!`/`\+` (audit-2026-09 #2): the one backslash is consumed and
            // the sugar character underneath reaches the title literally,
            // mirroring the `\"` the manual already documents for quotes.
            title_words.push(lit.to_string());
        } else if let Some(tag) = tag_of(&tok) {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        } else if let Some((key, v)) = split_key(&tok) {
            match key {
                ValueKey::Project => {
                    if project.is_none() {
                        project = Some(v.to_string());
                        // Unquoted AND something follows it: only then is there
                        // a word the tokenizer could have taken off the end of
                        // this name.
                        project_may_be_truncated = !quoted && i < last_tok;
                    }
                }
                ValueKey::Due => set_if_empty(&mut due, v),
                ValueKey::Scheduled => set_if_empty(&mut scheduled, v),
                ValueKey::Wait => set_if_empty(&mut wait, v),
                ValueKey::Repeat => set_if_empty(&mut recurrence, v),
                // `every:X` is the rule `every X`; the key carries the tail only.
                // Not routed through `set_if_empty`, which would build the rule
                // string before discovering an explicit `--repeat` already won.
                ValueKey::Every => {
                    if recurrence.is_none() {
                        recurrence = Some(format!("every {v}"));
                    }
                }
                ValueKey::Remind => set_if_empty(&mut remind, v),
                ValueKey::Estimate => set_if_empty(&mut estimate, v),
            }
        } else if let Some(p) = tok.strip_prefix('!') {
            // Validated even when an explicit --priority already won, so a typo
            // is never excused by the flag that happened to outrank it: the flag
            // decides which *valid* value applies, not whether the token parses.
            let v = normalize_prio(p).ok_or_else(|| bad_priority(p))?;
            if priority.is_none() {
                priority = Some(v);
            }
        } else {
            // #156: on `modify` a leftover word shaped exactly like a declined
            // sugar key does not get to become the title — see
            // `declined_key_shape` for what "exactly like" means.
            if ctx == ParseContext::Modify {
                if let Some(ident) = declined_key_shape(&tok) {
                    return Err(unrecognised_modify_field(&tok, ident));
                }
            } else if let Some(ident) = declined_key_shape(&tok) {
                // #139/D108: `add` keeps D45's fall-through — the same word is
                // harmless prose here (there is no prior title a typo could
                // destroy, unlike `modify`) — but silence cost nothing to fix:
                // a stderr warning names what looked like sugar without
                // refusing the command or changing its exit code, so
                // `recur::advance_once`, `note:`, a Windows path or a ratio
                // typed as a title word still store exactly as typed.
                eprintln!("{}", declined_key_warning(&tok, ident));
            }
            title_words.push(tok);
        }
    }

    Ok(ParsedAdd {
        title: title_words.join(" "),
        project,
        priority,
        tags,
        due,
        scheduled,
        wait,
        recurrence,
        remind,
        estimate,
        tracked: flags.tracked,
        budget_tokens: flags.budget_tokens,
        project_may_be_truncated,
    })
}

fn set_if_empty(slot: &mut Option<String>, v: &str) {
    if slot.is_none() && !v.is_empty() {
        *slot = Some(v.to_string());
    }
}

/// A token opening with `\!` or `\+` — an escaped sugar character — with the
/// one leading backslash consumed, or `None` if it names no escape.
///
/// The `!` and `+` sugar characters are otherwise unrepresentable at the start
/// of a title word (audit-2026-09 #2): `!important thing` opens the priority
/// sugar and refuses on the bad value, and the only workaround left the
/// backslash in the stored title. This mirrors the `\"` escape
/// `tasqx_core::filter::split_words` already documents, one level up: it fires
/// before `tag_of`/`split_key`/the priority prefix are consulted, so the
/// escaped token never reaches those branches at all.
fn escaped_sugar_char(tok: &str) -> Option<&str> {
    let rest = tok.strip_prefix('\\')?;
    rest.starts_with(['!', '+']).then_some(rest)
}

/// The tag names a `tasqx tag` / `tasqx untag` argument list denotes.
///
/// The two surfaces must not disagree about what a tag IS, so this routes the
/// `+`-prefixed spelling through [`tag_of`] — the same one decision `add` and
/// `modify` make about their sugar — rather than growing a second stripper. A
/// word without the `+` is the tag itself: on `tag`/`untag` the positional has
/// no title to fall through to, so `tasqx tag 42 api` and `tasqx tag 42 +api`
/// name one tag, and both are spellings a user will reach for.
///
/// Two departures from the sugar path, both because there is no title here:
///
///  * A bare `+` is **refused**, naming the word. C6's rule sends a token the
///    sugar declines to the TITLE, which is why `tasqx add "Display + Error"`
///    keeps its `+`. There is nowhere for it to go on this verb, so the choices
///    are a refusal or a silent deletion, and a silently deleted argument on a
///    write is the failure this repository keeps paying for.
///  * The empty string is refused for the same reason, rather than reaching the
///    core to come back as "`tags` contains an empty string" — which is true but
///    does not say which of the words the shell handed over was empty.
///
/// Names come back in the core's stored spelling (D172), and duplicates
/// collapse, exactly as they do in [`parse_add`]: `tag 42 api API` is one tag,
/// and the order the user typed is preserved.
pub fn tag_arguments(words: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for word in words {
        let name = match word.starts_with('+') {
            true => tag_of(word).ok_or_else(|| {
                ApiError::bad_request(
                    "a bare `+` names no tag — write the tag after it, as `+api` or `api`",
                )
            })?,
            false => word.as_str(),
        };
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "an empty tag name was given — drop the argument rather than passing \"\"",
            ));
        }
        out.push(name.to_string());
    }
    // D172: the core's spelling, so the echo marks the tag that was stored.
    tasqx_core::storage::normalize_tags(out)
}

/// The sugar VALUE this module would take out of a FINISHED word, or `None` if
/// it would take none and leave the word in the title.
///
/// # Why the completer has to ask
///
/// `complete::candidates` builds a candidate by composing a spelling with a name
/// out of the store — `"project:"` + `":x"` — and a composed word is not
/// automatically a word this parser accepts. That one is `project::x`, which
/// [`split_key`] refuses as a Rust path, so accepting the candidate files the
/// task under the DEFAULT project and leaves the word in the title, at exit 0.
/// Measured, with a store holding a project named `:x`: the completion feature
/// built to prevent exactly that failure was manufacturing it.
///
/// So the completer gates every candidate on this rather than on a character
/// allowlist of its own. The allowlist answers a different question — can a
/// SHELL deliver this as one word — and the two are independent: `:x` is
/// perfectly deliverable and is still not sugar-safe after a `project:`.
///
/// Read out of [`tag_of`] and [`split_key`], the same two functions the parse
/// path dispatches on, so a refusal added to either is honoured by the menu the
/// day it lands. Restating their rules in the completer is the drift shape this
/// repository keeps paying for, and the `::` rule in particular has already
/// moved once.
///
/// The whitespace and quoting cases [`tokenize_argv`] handles are deliberately
/// not considered here: `complete::candidates::deliverable_as_one_word` has
/// already excluded every name containing either, so a word reaching this
/// function is one [`tokenize`] would hand over intact.
pub(crate) fn parsed_value_of(word: &str) -> Option<&str> {
    tag_of(word).or_else(|| split_key(word).map(|(_, value)| value))
}

/// What a sugar word the user is still TYPING is reaching for, for the shell
/// completion dispatcher (`complete::candidates`).
///
/// The `'static` spelling is carried because the candidate REPLACES the whole
/// word: completing `proj:wo` has to answer `proj:work`, not `project:work`, or
/// Tab rewrites what the user chose to type. Reading the spelling back out of
/// [`VALUE_KEYS`] rather than re-deriving it at the call site is what keeps the
/// two aliases from needing two code paths.
pub(crate) enum Partial<'a> {
    /// `+`, then whatever has been typed of the tag name.
    Tag(&'a str),
    /// A value key, the spelling it was typed with, and the value so far.
    Key(ValueKey, &'static str, &'a str),
    /// `!`, then whatever has been typed of the priority.
    Priority(&'a str),
}

/// Classify a PARTIAL word — the one under the cursor — or `None` when it is
/// ordinary title text.
///
/// # Why this is not [`split_key`] plus [`tag_of`]
///
/// Those two answer "what IS this token", and their refusals are load-bearing on
/// the parse path: a bare `+` names no tag and a valueless `due:` names no date,
/// so both are left in the title (C6). This answers a different question —
/// "what could COME NEXT here" — and for that question a bare `+` and a bare
/// `project:` are the most informative words a user can type. They are the
/// moment the menu is wanted, not the moment it is refused.
///
/// So the empty value is accepted here and nowhere else. Everything else is the
/// same judgement, read out of the same table: the key is resolved ONCE by first
/// prefix match against [`VALUE_KEYS`] (longest-first, so `estimate:` is not
/// read as the estimate `imate:`), and `::` is still a Rust path rather than a
/// key — `project::config<TAB>` must not offer projects any more than
/// `add "see project::config"` may set one.
///
/// A word with no recognised prefix is `None`, and the dispatcher offers
/// nothing for it. Completing bare title words against the sugar vocabulary
/// would put a menu in front of somebody typing prose.
pub(crate) fn classify_partial(word: &str) -> Option<Partial<'_>> {
    if let Some(tag) = word.strip_prefix('+') {
        return Some(Partial::Tag(tag));
    }
    if let Some(prio) = word.strip_prefix('!') {
        return Some(Partial::Priority(prio));
    }
    let (key, spelling, value) = VALUE_KEYS
        .iter()
        .find_map(|&(spelling, key)| Some((key, spelling, word.strip_prefix(spelling)?)))?;
    (!value.starts_with(':')).then_some(Partial::Key(key, spelling, value))
}

/// Turn argv into sugar tokens, respecting boundaries the shell already drew.
///
/// An element like `repeat:every 3 days` only exists because the user wrote
/// `repeat:"every 3 days"` and the shell stripped the quotes — re-splitting it
/// would discard their intent. An element that still carries its own quotes
/// (`repeat:"every 3 days"` passed through literally) goes to [`tokenize`],
/// which understands them.
fn tokenize_argv(args: &[String]) -> Result<Vec<SugarTok>, ApiError> {
    let mut out = Vec::new();
    for arg in args {
        // `split_key`, not a bare `starts_with` over VALUE_KEYS: an element the
        // parse loop will hand to the title must not first be honored whole as a
        // "value". Otherwise `add "fix recur::advance_once and bound it"` became
        // ONE title word carrying the whole element, since the value it was
        // honored as was then declined downstream.
        let shell_quoted_value = !arg.contains('"')
            && arg.chars().any(char::is_whitespace)
            && (split_key(arg).is_some() || is_spaced_tag(arg));
        if shell_quoted_value {
            // The shell drew this boundary, so the value is quoted in every
            // sense that matters here — nothing about it was guessed.
            out.push(SugarTok {
                text: arg.clone(),
                quoted: true,
            });
        } else if arg.contains('"') && !could_contain_sugar(arg) {
            // #140: a literal `"` is grammar to the ONE shared scanner (D30)
            // — it opens a quoted span there and nothing tells it "this one
            // is just a quote the user typed". That is right for a sugar
            // VALUE (`project:"Home Renovation"`) and wrong for prose that
            // never meant to delimit anything: `add 'He said "hello" to me'`
            // silently dropped both quotes at exit 0, and the escape the
            // resulting "unterminated quote" error recommends does not even
            // work outside an already-open span — `\"` mid-prose opens a NEW
            // quote (the backslash is an ordinary character until then),
            // which then runs off the end of input and refuses the whole
            // command. An element with no `+`, `!` or `VALUE_KEYS` spelling
            // anywhere cannot contain sugar no matter how its quotes scan, so
            // it is never handed to the scanner: whatever quotes or
            // backslashes were typed reach the title exactly as typed.
            out.push(SugarTok {
                text: arg.clone(),
                quoted: false,
            });
        } else {
            let toks = tokenize(arg)?;
            // D36's STORAGE half — "accepted values are stored as given; the
            // trim decides validity, not storage" — which held on the JSON door
            // and not here. An element carrying no sugar IS the title text the
            // user typed, so it is handed over whole instead of as words that
            // the title branch later rejoins with a single space. Rejoining did
            // not merely trim the ends: it rewrote the MIDDLE of the title
            // (`a    b` became `a b`, a literal tab became a space), so
            // `tasqx add "  x  "` and `task.add {"title":"  x  "}` wrote
            // different bytes for one intent — the CLI-vs-API divergence this
            // project keeps paying for.
            //
            // Deliberately narrow. The moment ANY token in the element is sugar
            // the classic one-big-quoted-string capture form is in play
            // (`add "Ship it due:friday +api"`), and there the words are all
            // that can honestly be reconstructed — the sugar tokens are being
            // removed from the middle, so the original spacing no longer
            // describes the title that remains.
            //
            // An element that tokenizes to NOTHING (whitespace only) must keep
            // falling through: dropping it is what makes `tasqx add "   "`
            // reach `req_str` empty and be refused, which D36 requires.
            //
            // The element ITSELF is tested, not only its words, because it is
            // the element that gets pushed. `+ foo` tokenizes to two innocent
            // words (`+` names no tag, `foo` is a word) yet is sugar whole — the
            // loop would read the pushed element as the tag `" foo"`, which is
            // the very mint-a-space-tag outcome `is_spaced_tag` exists to stop.
            let is_pure_title = !arg.contains('"')
                && !toks.is_empty()
                && !is_sugar_token(arg)
                && !toks.iter().any(|t| is_sugar_token(&t.text));
            if is_pure_title {
                out.push(SugarTok {
                    text: arg.clone(),
                    quoted: false,
                });
            } else {
                out.extend(toks);
            }
        }
    }
    Ok(out)
}

/// One sugar token, plus whether any part of it arrived quoted.
///
/// The flag exists for exactly one consumer: an unquoted `project:` value ends
/// at a space the user may well have typed, so a name that fails to resolve may
/// be a FRAGMENT rather than a typo, and the error has to say which it cannot
/// tell. See `ParsedAdd::project_may_be_truncated`.
struct SugarTok {
    text: String,
    quoted: bool,
}

/// Could ANY word inside `arg` possibly be sugar? A cheap, purely syntactic
/// pre-check used only to decide whether `arg`'s own `"` characters need the
/// quote-aware scanner at all (#140): sugar always opens with `+`, `!`, or one
/// of [`VALUE_KEYS`]'s spellings, so an element containing none of those
/// cannot yield a sugar token no matter how its quotes are read, and its
/// quotes are therefore just prose. Deliberately a substring test, not a
/// per-word one — cheaper, and a false positive here only means the existing
/// (unchanged) scanner path runs, never that a quote is mishandled.
fn could_contain_sugar(arg: &str) -> bool {
    arg.contains('+')
        || arg.contains('!')
        || VALUE_KEYS
            .iter()
            .any(|(spelling, _)| arg.contains(spelling))
}

/// `+tag` is a value key without the colon, so it obeys the same whole-element
/// rule as one.
///
/// `add "painting job" +"needs paint"` reaches us as the element `+needs paint`;
/// re-splitting it stored the tag `needs` and, because the leftover word fell
/// through to the title branch, silently renamed the task to `painting job
/// paint`. On `modify` the same split rewrote the title to `job` outright.
///
/// Stricter than [`tag_of`] on exactly one point: the `+` must be followed by
/// actual content, not by the space itself. `+ foo` does name no tag either way,
/// but honouring it WHOLE here would mint the tag `" foo"` instead.
fn is_spaced_tag(arg: &str) -> bool {
    tag_of(arg).is_some_and(|t| !t.starts_with(char::is_whitespace))
}

/// Whitespace-split, but keep double-quoted spans together and strip the quotes,
/// so `due:"in 3 days"` becomes the single token `due:in 3 days`.
///
/// C8: this is [`tasqx_core::filter::split_words`] and nothing else — the ONE
/// quoting rule, owned by the read side's grammar, which documents it and is
/// tested against it. It used to be a second tokenizer written here, and the two
/// disagreed about the same syntax: `"` was a pure delimiter with no escapes, so
/// `add '+say"hi'` stored the tag `sayhi` and `project:"My \"Big\" Project"`
/// stored `My \Big\ Project`. A value containing a quote could not be written at
/// all, which made the escape the read side documents unmatchable for tags.
///
/// Fallible for the reason the read side is: an unterminated quote is refused,
/// not closed at end of input. Guessing where the user meant it to end is
/// exactly how `sayhi` got stored under a zero exit code.
fn tokenize(raw: &str) -> Result<Vec<SugarTok>, ApiError> {
    let words = tasqx_core::filter::split_words(raw, "task text").map_err(ApiError::bad_request)?;
    Ok(words
        .into_iter()
        .map(|w| SugarTok {
            text: w.text,
            quoted: w.quoted,
        })
        .collect())
}

/// A single WORD (no internal whitespace) shaped exactly like an attempted
/// `key:value` sugar token that [`split_key`] already declined — one colon,
/// a lowercase-ASCII-and-underscore key, a non-empty value with no further
/// colon in it — or `None` when it is not that shape. Returns the key.
///
/// By construction anything this returns `Some` for is NOT a real key: a
/// spelling in [`VALUE_KEYS`] with a real value would already have been
/// claimed by `split_key` earlier in the same loop and never reached this
/// arm. The `::`-doubled spelling (D45, `recur::advance_once`) is excluded on
/// purpose by requiring exactly one colon, so a Rust path keeps reading as
/// prose on `modify` exactly as it does on `add`.
///
/// The no-whitespace requirement excludes the whole-element verbatim capture
/// form (`tokenize_argv`'s `is_pure_title` path): a multi-word sentence that
/// happens to contain a colon somewhere (`"note: check this"`) is
/// unambiguously a sentence, not a mistyped key, and only a single suspicious
/// WORD earns the scrutiny — see [`ParseContext::Modify`].
fn declined_key_shape(word: &str) -> Option<&str> {
    if word.chars().any(char::is_whitespace) {
        return None;
    }
    let (ident, value) = word.split_once(':')?;
    let is_ident = !ident.is_empty() && ident.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    (is_ident && !value.is_empty() && !value.contains(':')).then_some(ident)
}

/// #156: `modify 42 status:done` used to answer `Modified #42 / title <-
/// status:done` at exit 0, silently replacing whatever the task was called.
/// `undo` cannot reach it — a `modify` event records only the values that
/// were SET (D54), never the ones they replaced — so the loss is permanent,
/// not merely silent. The list of known keys is read out of [`VALUE_KEYS`]
/// rather than retyped, so it cannot go stale the way a second copy would.
fn unrecognised_modify_field(word: &str, ident: &str) -> ApiError {
    let known: Vec<&str> = VALUE_KEYS.iter().map(|(s, _)| *s).collect();
    ApiError::bad_request(format!(
        "unknown field {ident:?} in `{word}` — modify's inline sugar is {}, plus +tag and !prio; \
         `status` moves through start/stop/done/cancel, not modify. If this was meant as the new \
         title, add another word so it cannot be misread as a key.",
        known.join(", ")
    ))
}

/// #139/D108: `add`'s non-fatal twin of [`unrecognised_modify_field`] — a
/// stderr note, not a refusal, printed once per declined word and never
/// altering `title_words` or the exit code. A plain function (rather than
/// inlining the `format!` at the one `eprintln!` call site) so the message
/// itself is asserted in tests without capturing stderr.
fn declined_key_warning(word: &str, ident: &str) -> String {
    format!(
        "warning: {word:?} looks like sugar but {ident:?} isn't a recognized field — kept as \
         title text"
    )
}

/// Names the value and every spelling that would have worked, because `!` has no
/// escape: there is no way to mean a literal bang-word in a title, so the
/// message has to carry the whole way out rather than assume a retype is obvious.
fn bad_priority(s: &str) -> ApiError {
    ApiError::bad_request(format!(
        "invalid priority: {s:?} (try !h/!high, !m/!medium, !l/!low — or drop the ! to keep it as title text)"
    ))
}

/// `None` is a *rejection*, not a shrug — the caller must turn it into
/// [`bad_priority`]. Returning the token to the title instead would be the
/// quieter failure: the user asked for a priority and would get a renamed task.
///
/// Delegates to `Priority::parse` rather than restating its table. It used to
/// carry its own copy of the vocabulary, which meant `!medium` and
/// `--priority medium` were two independent promises that happened to agree;
/// a spelling added to one and not the other would have been accepted here and
/// rejected by the engine one call later, or the reverse.
fn normalize_prio(s: &str) -> Option<String> {
    Priority::parse(s).map(|p| p.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classic capture form: ONE shell argument carrying the whole title and
    /// its sugar, which the parser re-tokenizes itself.
    fn parse1(raw: &str, flags: AddFlags) -> ParsedAdd {
        parse_add(&[raw.to_string()], flags, ParseContext::Add).expect("parses")
    }

    /// The shell-tokenized form: several argv words, quotes already consumed by
    /// the shell — what `tasqx modify 4 repeat:"every 3 days"` really delivers.
    fn parse_argv(args: &[&str], flags: AddFlags) -> ParsedAdd {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_add(&owned, flags, ParseContext::Add).expect("parses")
    }

    /// The same argv, kept as an error so the rejection itself can be asserted on.
    fn parse_err(args: &[&str], flags: AddFlags) -> ApiError {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        // `.err()` rather than `expect_err`, which would demand Debug on the
        // success type purely to serve a test.
        parse_add(&owned, flags, ParseContext::Add)
            .err()
            .expect("must be refused")
    }

    /// [`parse_argv`]'s `modify` twin — the context where a declined
    /// `key:value`-shaped word is a refusal rather than title text (#156).
    fn parse_modify_argv(args: &[&str], flags: AddFlags) -> Result<ParsedAdd, ApiError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_add(&owned, flags, ParseContext::Modify)
    }

    /// C6: the token must not evaporate. The message names the value and every
    /// spelling that works, since a bare `!` has no escape into title text.
    #[test]
    fn an_unusable_priority_token_is_refused_not_dropped() {
        let e = parse_err(&["urgent thing", "!urgent"], AddFlags::default());
        assert!(
            e.message.contains("urgent"),
            "names the value: {}",
            e.message
        );
        assert!(
            e.message.contains("!high"),
            "lists the way out: {}",
            e.message
        );

        // A lone `!` named no priority either, and vanished just as quietly.
        assert!(parse_err(&["!"], AddFlags::default())
            .message
            .contains("invalid priority"));
    }

    /// Finding #2 (audit-2026-09): `!important thing` cannot be typed at all (`!`
    /// opens the priority sugar and `important` is not a valid priority), and the
    /// only workaround, `\!important thing`, stored the backslash verbatim
    /// instead of yielding the literal title. `\!` and `\+` must escape into
    /// literal `!`/`+` text, mirroring the `\"` the manual already documents.
    #[test]
    fn backslash_escapes_bang_and_plus_into_literal_title_text() {
        let p = parse1("\\!important thing", AddFlags::default());
        assert_eq!(p.title, "!important thing");
        assert_eq!(p.priority, None);

        let p = parse1("\\+notatag thing", AddFlags::default());
        assert_eq!(p.title, "+notatag thing");
        assert!(p.tags.is_empty());
    }

    /// An explicit flag outranks sugar on *value*, never on *validity* — the
    /// alternative excuses a typo whenever a flag happens to be present too.
    #[test]
    fn a_winning_flag_does_not_excuse_an_invalid_sugar_token() {
        let flags = AddFlags {
            priority: Some("H".into()),
            ..AddFlags::default()
        };
        assert!(parse_err(&["Task", "!urgent"], flags)
            .message
            .contains("invalid priority"));
    }

    /// Every accepted spelling, pinned so the error's promised way out is real.
    #[test]
    fn every_documented_priority_spelling_still_parses() {
        for (tok, want) in [
            ("!h", "H"),
            ("!high", "H"),
            ("!m", "M"),
            ("!med", "M"),
            ("!medium", "M"),
            ("!l", "L"),
            ("!low", "L"),
            ("!HIGH", "H"),
        ] {
            let p = parse_argv(&["Task", tok], AddFlags::default());
            assert_eq!(p.priority.as_deref(), Some(want), "{tok} must parse");
            assert_eq!(p.title, "Task", "{tok} must not leak into the title");
        }
    }

    /// The walk found this by typing what the docs show. `modify 4 repeat:"every
    /// 3 days"` arrives as ONE argv element with the quotes already gone; the
    /// old join-then-resplit read `every` as the whole rule. Worse,
    /// `project:"my big project"` silently set project=`my` and RENAMED the task
    /// to `big project` — no error, wrong data.
    #[test]
    fn a_shell_quoted_value_survives_as_one_token() {
        let p = parse_argv(&["repeat:every 3 days"], AddFlags::default());
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
        assert_eq!(p.title, "", "the rule's words must not leak into the title");

        let p = parse_argv(&["project:my big project"], AddFlags::default());
        assert_eq!(p.project.as_deref(), Some("my big project"));
        assert_eq!(p.title, "", "the project's words must not become a title");

        let p = parse_argv(&["due:in 3 days"], AddFlags::default());
        assert_eq!(p.due.as_deref(), Some("in 3 days"));
        assert_eq!(p.title, "");

        let p = parse_argv(&["est:1h 30m"], AddFlags::default());
        assert_eq!(p.estimate.as_deref(), Some("1h 30m"));

        let p = parse_argv(&["every:3 days"], AddFlags::default());
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));

        let p = parse_argv(&["remind:friday 9am"], AddFlags::default());
        assert_eq!(p.remind.as_deref(), Some("friday 9am"));
    }

    /// The shell-tokenized multi-word form: new title words and sugar, mixed.
    #[test]
    fn argv_words_mix_title_and_sugar() {
        let p = parse_argv(
            &["Renamed", "task", "due:friday", "+api", "!high", "est:4h"],
            AddFlags::default(),
        );
        assert_eq!(p.title, "Renamed task");
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.priority.as_deref(), Some("H"));
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.tags, vec!["api".to_string()]);
    }

    /// A value element that kept its literal quotes still works — the quote-aware
    /// tokenizer handles it, and both paths must agree.
    #[test]
    fn quoted_and_shell_stripped_forms_agree() {
        let literal = parse_argv(&[r#"repeat:"every 3 days""#], AddFlags::default());
        let stripped = parse_argv(&["repeat:every 3 days"], AddFlags::default());
        assert_eq!(literal.recurrence, stripped.recurrence);
        assert_eq!(literal.recurrence.as_deref(), Some("every 3 days"));
    }

    /// C2: the same argv-boundary promise D13 made for `key:value`, for `+tag`.
    /// The whole element is the tag, and nothing leaks into the title.
    #[test]
    fn a_shell_quoted_tag_survives_as_one_token() {
        let p = parse_argv(&["painting job", "+needs paint"], AddFlags::default());
        assert_eq!(p.tags, vec!["needs paint".to_string()]);
        assert_eq!(
            p.title, "painting job",
            "the tag's words must not become title words"
        );

        // Literal quotes take the tokenizer path and must land in the same place,
        // which is what lets a tag round-trip through C1's filter quoting.
        let literal = parse_argv(&[r#"+"needs paint""#], AddFlags::default());
        assert_eq!(literal.tags, vec!["needs paint".to_string()]);
        assert_eq!(literal.title, "");
    }

    /// A `+` that names nothing is not a tag, and must not become the tag
    /// `" foo"` just because the element happens to contain a space.
    ///
    /// The title now keeps the `+`. It used to read `foo`: the `+` was claimed
    /// by the tag branch, failed the non-empty check, and was deleted — which is
    /// the same defect as `add "Display + Error"`, just with the space on the
    /// other side. Not naming a tag is a reason to leave a token alone, never a
    /// reason to consume it.
    #[test]
    fn a_bare_plus_names_no_tag() {
        let p = parse_argv(&["+ foo"], AddFlags::default());
        assert_eq!(p.tags, Vec::<String>::new());
        assert_eq!(p.title, "+ foo");
    }

    /// The classic one-argument capture form is untouched: there the user never
    /// drew a boundary, so `+needs` is still one tag and `paint` a title word.
    #[test]
    fn the_single_string_capture_form_still_splits_on_spaces() {
        let p = parse1("painting job +needs paint", AddFlags::default());
        assert_eq!(p.tags, vec!["needs".to_string()]);
        assert_eq!(p.title, "painting job paint");

        // …and quoting inside that one string is how you spell a spaced tag there.
        let q = parse1(r#"painting job +"needs paint""#, AddFlags::default());
        assert_eq!(q.tags, vec!["needs paint".to_string()]);
        assert_eq!(q.title, "painting job");
    }

    /// A value key with no spaces is untouched by the whole-element rule.
    #[test]
    fn unspaced_value_elements_are_unaffected() {
        let p = parse_argv(
            &["due:friday", "remind:-30m", "est:4h"],
            AddFlags::default(),
        );
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.remind.as_deref(), Some("-30m"));
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.title, "");
    }

    #[test]
    fn keeps_existing_tag_project_prio_sugar() {
        let p = parse1("Do the thing +x project:work !high", AddFlags::default());
        assert_eq!(p.title, "Do the thing");
        assert_eq!(p.project.as_deref(), Some("work"));
        assert_eq!(p.priority.as_deref(), Some("H"));
        assert_eq!(p.tags, vec!["x".to_string()]);
    }

    #[test]
    fn parses_due_keyword_and_strips_it_from_title() {
        let p = parse1("Do it due:friday +x project:p", AddFlags::default());
        assert_eq!(p.title, "Do it");
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.project.as_deref(), Some("p"));
        assert_eq!(p.tags, vec!["x".to_string()]);
    }

    #[test]
    fn parses_quoted_due_value_with_spaces() {
        let p = parse1(r#"Pay taxes due:"in 3 days""#, AddFlags::default());
        assert_eq!(p.title, "Pay taxes");
        assert_eq!(p.due.as_deref(), Some("in 3 days"));
    }

    #[test]
    fn parses_scheduled_wait_and_repeat() {
        let p = parse1(
            r#"Ship it scheduled:"tomorrow 9am" wait:friday repeat:"every 3 days""#,
            AddFlags::default(),
        );
        assert_eq!(p.title, "Ship it");
        assert_eq!(p.scheduled.as_deref(), Some("tomorrow 9am"));
        assert_eq!(p.wait.as_deref(), Some("friday"));
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
    }

    #[test]
    fn every_key_prefixes_the_rule() {
        let p = parse1(r#"Water plants every:"3 days""#, AddFlags::default());
        assert_eq!(p.title, "Water plants");
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
    }

    #[test]
    fn explicit_flags_win_over_sugar() {
        let flags = AddFlags {
            due: Some("2026-01-01".into()),
            repeat: Some("every week".into()),
            remind: Some("-2h".into()),
            ..AddFlags::default()
        };
        let p = parse1("Task due:friday repeat:\"every 3 days\" remind:-1h", flags);
        assert_eq!(p.due.as_deref(), Some("2026-01-01"));
        assert_eq!(p.recurrence.as_deref(), Some("every week"));
        assert_eq!(p.remind.as_deref(), Some("-2h"));
    }

    #[test]
    fn parses_remind_offset_and_strips_it_from_title() {
        // The leading `-` must survive tokenizing — it is what marks the value
        // as a due-anchored offset rather than a date (see core `remind`).
        let p = parse1(
            r#"Standup due:"friday 9am" remind:-15m"#,
            AddFlags::default(),
        );
        assert_eq!(p.title, "Standup");
        assert_eq!(p.due.as_deref(), Some("friday 9am"));
        assert_eq!(p.remind.as_deref(), Some("-15m"));
    }

    #[test]
    fn parses_quoted_absolute_remind_value() {
        let p = parse1(r#"Call mum remind:"friday 9am""#, AddFlags::default());
        assert_eq!(p.title, "Call mum");
        assert_eq!(p.remind.as_deref(), Some("friday 9am"));
    }

    /// DESIGN §5's own capture example carries `est:4h`; without this key the
    /// token landed in the title, so the task was literally named "… est:4h".
    #[test]
    fn parses_est_key_and_strips_it_from_title() {
        let p = parse1("Ship the API est:4h +api", AddFlags::default());
        assert_eq!(p.title, "Ship the API");
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.tags, vec!["api".to_string()]);

        let long = parse1(r#"Ship it estimate:"1h 30m""#, AddFlags::default());
        assert_eq!(long.title, "Ship it");
        assert_eq!(long.estimate.as_deref(), Some("1h 30m"));
    }

    #[test]
    fn explicit_estimate_flag_wins_over_sugar() {
        let flags = AddFlags {
            estimate: Some("2h".into()),
            ..AddFlags::default()
        };
        let p = parse1("Task est:4h", flags);
        assert_eq!(p.estimate.as_deref(), Some("2h"));
    }

    #[test]
    fn no_remind_key_means_no_reminder() {
        // Quiet by default (§9): nothing infers a reminder from a due date.
        let p = parse1("Do it due:friday", AddFlags::default());
        assert_eq!(p.remind, None);
    }

    /// C6 again, for `+`: a token that names no tag must reach the title, not be
    /// deleted. `add "Implement Display + std::error::Error"` stored the title
    /// without its `+` and created no tag — a success, a correct-looking task,
    /// and a quietly corrupted title.
    #[test]
    fn a_bare_plus_is_title_text_not_a_dropped_tag() {
        let p = parse1("Display + Error", AddFlags::default());
        assert_eq!(p.title, "Display + Error", "the + must survive verbatim");
        assert_eq!(p.tags, Vec::<String>::new());

        // …and in the shell-tokenized form, where the `+` is its own argv word.
        let q = parse_argv(&["Display", "+", "Error"], AddFlags::default());
        assert_eq!(q.title, "Display + Error");
        assert_eq!(q.tags, Vec::<String>::new());
    }

    /// A Rust path is not sugar: the separator is `::`, and every value key is
    /// also a plausible first module segment. `recur::advance_once` was read as
    /// the recurrence rule `":advance_once"` and the whole command refused.
    #[test]
    fn a_rust_path_is_not_a_sugar_value() {
        let p = parse1("fix recur::advance_once", AddFlags::default());
        assert_eq!(p.title, "fix recur::advance_once");
        assert_eq!(p.recurrence, None);

        // Worse than a hard error: this one was ACCEPTED, set project=":config"
        // and removed the word from the title.
        let q = parse1("see project::config", AddFlags::default());
        assert_eq!(q.title, "see project::config");
        assert_eq!(q.project, None);

        // The shorter alias must not pick up what the longer one just declined:
        // `strip_prefix("proj:")` on `project::config` yields `ect::config`.
        assert_ne!(q.project.as_deref(), Some("ect::config"));

        for tok in [
            "scheduled::at",
            "estimate::of",
            "remind::me",
            "repeat::forever",
            "every::other",
            "sched::at",
            "proj::x",
            "wait::for",
            "due::soon",
            "est::of",
        ] {
            let r = parse_argv(&["Task", tok], AddFlags::default());
            assert_eq!(r.title, format!("Task {tok}"), "{tok} is title text");
        }
    }

    /// One colon is still sugar — the fix must not cost the common spelling.
    #[test]
    fn a_single_colon_is_still_sugar() {
        let p = parse_argv(&["due:friday", "ship", "it"], AddFlags::default());
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.title, "ship it");
    }

    /// A key with nothing after it names no value, so it is a word. It used to
    /// be swallowed by its own branch and dropped — the `+` bug with a colon.
    #[test]
    fn a_valueless_key_stays_in_the_title() {
        let p = parse1("meeting due: soon", AddFlags::default());
        assert_eq!(p.title, "meeting due: soon");
        assert_eq!(p.due, None);

        let q = parse_argv(&["notes", "est:"], AddFlags::default());
        assert_eq!(q.title, "notes est:");
        assert_eq!(q.estimate, None);
    }

    /// #140: a literal `"` in ordinary prose is grammar to the ONE shared
    /// scanner (D30) — it opens a quoted span there and nothing distinguishes
    /// "this delimits a sugar value" from "this is a quote the user typed".
    /// `tasqx add 'He said "hello" to me'` used to store `He said hello to
    /// me` at exit 0, and the escape the resulting "unterminated quote" error
    /// recommended did not even work: `\"` mid-prose opens a NEW quoted span
    /// (the backslash is an ordinary character outside one), which then runs
    /// off the end of input and refuses the whole command — so following the
    /// error's own advice reproduced the error. An element with no `+`, `!`
    /// or [`VALUE_KEYS`] spelling anywhere cannot contain sugar regardless of
    /// how its quotes scan, so it now never reaches the scanner at all.
    #[test]
    fn quote_characters_in_a_title_round_trip_verbatim() {
        let p = parse1(r#"He said "hello" to me"#, AddFlags::default());
        assert_eq!(p.title, r#"He said "hello" to me"#);

        // The same title, already split into argv words by the shell (quotes
        // intact, no whitespace inside any one word).
        let q = parse_argv(
            &["He", "said", "\"hello\"", "to", "me"],
            AddFlags::default(),
        );
        assert_eq!(q.title, r#"He said "hello" to me"#);

        // The documented escape, tried exactly as the error message spells
        // it, in a title with nothing else sugar-shaped in it.
        let r = parse1(r#"He said \"hi\" ok"#, AddFlags::default());
        assert_eq!(r.title, r#"He said \"hi\" ok"#);
    }

    /// #156: `modify` silently overwrote the title with any unrecognised
    /// `key:value`-shaped word — `modify 247 status:done` answered `Modified
    /// #247 / title <- status:done` at exit 0, destroying the real title with
    /// no way back (`undo` refuses `modify`: D54 records only what was SET).
    /// `status:`/`priority:` are exactly this sharp because they are real,
    /// documented FILTER grammar, so the vocabulary itself teaches the
    /// mistake. `add` keeps D45's fall-through — still stored, still exit 0
    /// — and only gains a stderr warning (D108, see
    /// [`add_warns_on_stderr_for_a_declined_key_but_still_stores_and_exits_0`]).
    #[test]
    fn modify_refuses_an_unrecognised_key_value_word_instead_of_destroying_the_title() {
        for tok in ["status:done", "priority:high", "prio:H", "p:H", "urgency:5"] {
            let e = parse_modify_argv(&[tok], AddFlags::default())
                .err()
                .unwrap_or_else(|| panic!("{tok} must be refused, not stored as the title"));
            assert!(e.message.contains(tok), "{tok}: {}", e.message);
        }

        // `add` is unaffected on TITLE and EXIT CODE: the same word is still
        // harmless prose there (D108 adds a warning, not a behaviour change —
        // see the dedicated test).
        let p = parse_argv(&["status:done"], AddFlags::default());
        assert_eq!(p.title, "status:done");

        // A real recognised key still wins on `modify`, exactly as before.
        let m = parse_modify_argv(&["due:friday"], AddFlags::default()).expect("recognised key");
        assert_eq!(m.due.as_deref(), Some("friday"));

        // The `::` path spelling (D45) stays prose on `modify` too — it has
        // two colons, not the one-colon shape this guard targets.
        let n = parse_modify_argv(&["fix", "recur::advance_once"], AddFlags::default())
            .expect("a Rust path is not sugar");
        assert_eq!(n.title, "fix recur::advance_once");

        // A multi-word sentence containing a colon is unambiguously prose,
        // not a mistyped key — only a single suspicious WORD earns scrutiny.
        let s = parse_modify_argv(&["note:", "check", "this"], AddFlags::default())
            .expect("a valueless key plus words is still just a sentence");
        assert_eq!(s.title, "note: check this");
    }

    /// #139/D108: `add "deadline:friday"` used to fold silently into the
    /// title, exit 0, with nothing telling the caller that `deadline` is not
    /// a recognized sugar key — the same class of silence D34 closed for
    /// `status:pendign` and D109 closes for `project:`, on the write side.
    /// Dimitri's call: `add` keeps D45's behaviour (still stored, still exit
    /// 0 — a blanket refusal would break `recur::advance_once`, `note:`,
    /// `C:\path`, ratios) and gains only a stderr warning. This asserts the
    /// warning function directly rather than capturing stderr — the same
    /// pattern `settings.rs`'s `unknown_theme_warning` uses for its own
    /// `eprintln!`-fed message.
    #[test]
    fn add_warns_on_stderr_for_a_declined_key_but_still_stores_and_exits_0() {
        assert_eq!(
            declined_key_warning("deadline:friday", "deadline"),
            "warning: \"deadline:friday\" looks like sugar but \"deadline\" isn't a recognized \
             field — kept as title text"
        );

        // The behaviour itself is untouched: `parse_add` on `Add` still never
        // errors for this shape, and the word still lands in the title.
        let p = parse_argv(&["deadline:friday"], AddFlags::default());
        assert_eq!(p.title, "deadline:friday");

        // Real prose a blanket refusal would have broken stays unwarned,
        // because `declined_key_shape` already excludes it (double colon, an
        // uppercase drive letter, a valueless key, a digit-led "identifier").
        for tok in ["recur::advance_once", "note:", r"C:\path", "3:2"] {
            let p = parse1(tok, AddFlags::default());
            assert_eq!(p.title, tok, "{tok} must round-trip verbatim");
        }
    }

    // ---- tag_arguments (the `tag`/`untag` verbs) ----------------------------

    fn words(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The property the whole helper exists for: `tasqx tag 42 +api` and
    /// `tasqx modify 42 +api` must name the SAME tag. Sending `+api` verbatim
    /// would create a tag literally called `+api` — invisible next to the `api`
    /// the sugar path writes, and unreachable by the `+api` filter token.
    #[test]
    fn a_tag_means_the_same_thing_with_and_without_the_plus() {
        assert_eq!(tag_arguments(&words(&["api"])).unwrap(), ["api"]);
        assert_eq!(tag_arguments(&words(&["+api"])).unwrap(), ["api"]);
        // And the sugar path, which is the surface this must agree with.
        let sugared = parse_add(
            &words(&["x", "+api"]),
            AddFlags::default(),
            ParseContext::Add,
        )
        .unwrap();
        assert_eq!(sugared.tags, tag_arguments(&words(&["+api"])).unwrap());
    }

    /// Duplicates collapse and the typed order survives, exactly as in
    /// [`parse_add`] — two spellings of one tag in one command is a typo-shaped
    /// mistake, not a request for two links.
    #[test]
    fn duplicate_tags_collapse_and_keep_their_order() {
        assert_eq!(
            tag_arguments(&words(&["release", "+api", "api", "release"])).unwrap(),
            ["release", "api"]
        );
    }

    /// D172: the names are the core's stored spelling, so `Perf` and `perf`
    /// are one tag and the echo can mark the tag that was stored.
    #[test]
    fn tag_arguments_are_the_stored_lowercase_spelling() {
        assert_eq!(
            tag_arguments(&words(&["+Perf", "perf", "ÜNÏ"])).unwrap(),
            ["perf", "ünï"]
        );
    }

    /// A bare `+` reaches the TITLE on `add` (C6). There is no title on
    /// `tag`/`untag`, so the only alternatives are a refusal and a silent
    /// deletion — and a silently deleted argument on a write is the failure
    /// this repository keeps paying for. The message has to name the fix.
    #[test]
    fn a_bare_plus_is_refused_rather_than_dropped() {
        let err = tag_arguments(&words(&["+"])).expect_err("a bare `+` names no tag");
        assert!(err.message.contains('+'), "{}", err.message);
        assert!(
            err.message.contains("api"),
            "the refusal must show a working spelling: {}",
            err.message
        );
    }

    /// The empty word, refused here rather than at the core. The core's own
    /// message ("`tags` contains an empty string") is true and says nothing
    /// about WHICH argument the shell handed over empty.
    #[test]
    fn an_empty_tag_word_is_refused_at_the_cli() {
        let err = tag_arguments(&words(&["api", ""])).expect_err("an empty tag names nothing");
        assert!(err.message.contains("empty"), "{}", err.message);
    }
}
