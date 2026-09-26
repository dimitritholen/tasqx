//! Cutting memory into the pieces that are embedded (D196).
//!
//! A static embedding is the mean of its tokens, so a long text's vector is
//! an average of everything it says and near nothing in particular. Memory
//! is therefore embedded a paragraph or a section at a time, and an entry
//! scores as its best chunk.
//!
//! Text is read as markdown, just enough to find its blocks:
//!
//! - `\r\n` reads as `\n`. A blank line ends a paragraph; a list or a table
//!   without blank lines inside is one paragraph.
//! - An ATX heading (`#` to `######`, up to three spaces of indent) ends a
//!   paragraph and sets the heading path for what follows; a heading is
//!   never a chunk on its own. Setext headings (a line of `===` or `---`
//!   under text) are read as text. In a doc, the FIRST heading is the doc
//!   itself when it repeats the title (as a `# Title` line usually does): it
//!   joins no path, so text above and below it is one section. Any later
//!   heading is a section, whatever it says.
//! - A fenced code block (three or more backticks or tildes) is one block
//!   and is never split, however long; a `#` line inside it is code, not a
//!   heading. A fence left open runs to the end of the text.
//! - A block over [`MAX_WORDS`] is cut at sentence ends (`.`, `!`, `?`, `…`
//!   followed by whitespace or the end, and `。`, `！`, `？` anywhere) into
//!   pieces that fit. Each piece after the first starts with the sentence
//!   that ended the one before, always: a piece may exceed [`MAX_WORDS`] by
//!   that one sentence. A single sentence over [`MAX_WORDS`] (a pasted log,
//!   text without punctuation) is cut into the fewest near-equal parts that
//!   fit, which carry no overlap.
//! - Neighbouring blocks under the same heading path merge while either is
//!   under [`MIN_WORDS`] and the two together are at most [`MAX_WORDS`].
//! - Every chunk of a doc starts with the doc's title and its heading path,
//!   one per line, then a blank line, then its text.
//!
//! Front matter needs no rule here: what is chunked is a doc's
//! `search_body`, where a front matter block has already been flattened
//! into plain `key  value` lines (D135), which read as a paragraph and merge
//! with their neighbours like any other.
//!
//! Sizes are counted in units, not bytes or whitespace words, so that text
//! the tokenizer reads as many words is never one giant chunk: a unit is a
//! run of characters between whitespace, except that a CJK ideograph is a
//! unit of its own (the tokenizer reads each as a word) and a run longer
//! than [`MAX_UNIT_CHARS`] counts once per [`MAX_UNIT_CHARS`] characters (a
//! URL, a hash, base64). English text has as many units as words. The
//! prefix is not counted.

use super::tables;
use super::tokenizer::in_class;

/// Bump when any text would be cut differently; it is part of
/// [`super::MODEL_ID`].
pub const VERSION: u32 = 2;

/// A block this short merges with its neighbour when the two fit.
pub const MIN_WORDS: usize = 40;

/// No chunk is longer than this, except a fenced code block and a piece
/// carrying its overlap sentence.
pub const MAX_WORDS: usize = 150;

/// Characters in one unit of an unbroken run.
pub const MAX_UNIT_CHARS: usize = 25;

#[derive(Debug)]
struct Piece {
    path: Vec<String>,
    text: String,
    units: usize,
}

/// The byte ranges of `text`'s units (see the module docs).
fn units(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        if c.is_whitespace() || in_class(tables::WHITE, c) {
            out.extend(run.take().map(|(s, _)| (s, i)));
        } else if in_class(tables::CJK, c) {
            out.extend(run.take().map(|(s, _)| (s, i)));
            out.push((i, end));
        } else {
            match &mut run {
                Some((_, n)) if *n < MAX_UNIT_CHARS => *n += 1,
                Some((s, _)) => {
                    out.push((*s, i));
                    run = Some((i, 1));
                }
                None => run = Some((i, 1)),
            }
        }
    }
    out.extend(run.map(|(s, _)| (s, text.len())));
    out
}

fn unit_count(s: &str) -> usize {
    units(s).len()
}

/// The chunks of a doc titled `title` whose text is `text` (its
/// `search_body`). A doc with no text is one chunk, its title; a doc with
/// neither has none.
pub fn chunk_doc(title: &str, text: &str) -> Vec<String> {
    let title = title.trim();
    let chunks: Vec<String> = sections(text, title)
        .into_iter()
        .map(|p| with_prefix(title, &p.path, &p.text))
        .collect();
    if chunks.is_empty() && !title.is_empty() {
        return vec![title.to_string()];
    }
    chunks
}

/// The chunks of an annotation. It is read as a doc with no title is, so
/// `\r\n` and headings are handled the same way at any length; but up to
/// [`MAX_WORDS`] it is one chunk, its sections joined in order, and only a
/// longer one is cut. Empty or whitespace-only text has none.
pub fn chunk_annotation(body: &str) -> Vec<String> {
    let text = body.replace("\r\n", "\n");
    if unit_count(&text) > MAX_WORDS {
        return sections(&text, "")
            .into_iter()
            .map(|p| with_prefix("", &p.path, &p.text))
            .collect();
    }
    let mut groups: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for b in blocks(&text, "") {
        match groups.last_mut() {
            Some((path, texts)) if *path == b.path => texts.push(b.text),
            _ => groups.push((b.path, vec![b.text])),
        }
    }
    if groups.is_empty() {
        return Vec::new();
    }
    let one: Vec<String> = groups
        .iter()
        .map(|(path, texts)| with_prefix("", path, &texts.join("\n\n")))
        .collect();
    vec![one.join("\n\n")]
}

/// Blocks, split to fit, then merged: the pieces that become chunks.
fn sections(text: &str, title: &str) -> Vec<Piece> {
    let text = text.replace("\r\n", "\n");
    let mut merged: Vec<Piece> = Vec::new();
    for piece in blocks(&text, title).into_iter().flat_map(split_long) {
        match merged.last_mut() {
            Some(last)
                if last.path == piece.path
                    && (last.units < MIN_WORDS || piece.units < MIN_WORDS)
                    && last.units + piece.units <= MAX_WORDS =>
            {
                last.text.push_str("\n\n");
                last.text.push_str(&piece.text);
                last.units += piece.units;
            }
            _ => merged.push(piece),
        }
    }
    merged
}

fn with_prefix(title: &str, path: &[String], text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    if !title.is_empty() {
        lines.push(title);
    }
    lines.extend(path.iter().map(String::as_str));
    if lines.is_empty() {
        return text.to_string();
    }
    format!("{}\n\n{text}", lines.join("\n"))
}

/// The opening fence of a code block: its character and run length.
fn fence_open(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let c = rest.chars().next().filter(|&c| c == '`' || c == '~')?;
    let run = rest.len() - rest.trim_start_matches(c).len();
    // A backtick fence's info string may not contain a backtick.
    if run < 3 || (c == '`' && rest[run..].contains('`')) {
        return None;
    }
    Some((c, run))
}

fn fence_closes(line: &str, (c, len): (char, usize)) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return false;
    }
    let run = t.len() - t.trim_start_matches(c).len();
    run >= len && t[run..].trim().is_empty()
}

/// An ATX heading's level and text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.len() - rest.trim_start_matches('#').len();
    let after = &rest[level..];
    if !(1..=6).contains(&level) || !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    // An optional closing run of `#`s, when a space separates it.
    let mut text = after.trim();
    let bare = text.trim_end_matches('#');
    if bare.is_empty() || bare.ends_with([' ', '\t']) {
        text = bare.trim_end();
    }
    Some((level, text))
}

/// Push the block `lines` make, under `path`, when it has any text.
fn emit(out: &mut Vec<Piece>, path: &[(usize, String)], lines: &mut Vec<&str>) {
    if lines.is_empty() {
        return;
    }
    let text = lines.join("\n").trim().to_string();
    lines.clear();
    let units = unit_count(&text);
    if units > 0 {
        let path = path.iter().map(|(_, h)| h.clone()).collect();
        out.push(Piece { path, text, units });
    }
}

fn blocks(text: &str, title: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut fence: Option<((char, usize), Vec<&str>)> = None;
    let mut first_heading = true;
    for line in text.split('\n') {
        if let Some((open, code)) = &mut fence {
            code.push(line);
            if fence_closes(line, *open) {
                let (_, mut code) = fence.take().expect("inside a fence");
                emit(&mut out, &path, &mut code);
            }
            continue;
        }
        if let Some(open) = fence_open(line) {
            emit(&mut out, &path, &mut para);
            fence = Some((open, vec![line]));
        } else if let Some((level, h)) = heading(line) {
            emit(&mut out, &path, &mut para);
            let is_title = first_heading && !title.is_empty() && h.eq_ignore_ascii_case(title);
            first_heading = false;
            while path.last().is_some_and(|(l, _)| *l >= level) {
                path.pop();
            }
            if !h.is_empty() && !is_title {
                path.push((level, h.to_string()));
            }
        } else if line.trim().is_empty() {
            emit(&mut out, &path, &mut para);
        } else {
            para.push(line);
        }
    }
    emit(&mut out, &path, &mut para);
    if let Some((_, mut code)) = fence {
        emit(&mut out, &path, &mut code);
    }
    out
}

fn is_code(text: &str) -> bool {
    fence_open(text.lines().next().unwrap_or("")).is_some()
}

/// One sentence of a long block, or one part of a sentence too long to fit.
struct Sentence {
    text: String,
    units: usize,
    part: bool,
}

/// The sentences of a block; one over [`MAX_WORDS`] comes back as the
/// fewest near-equal parts that fit, cut between units.
fn sentences(text: &str) -> Vec<Sentence> {
    let mut spans = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let western = matches!(c, '.' | '!' | '?' | '…')
            && chars.peek().is_none_or(|&(_, n)| n.is_whitespace());
        if western || matches!(c, '。' | '！' | '？') {
            let end = i + c.len_utf8();
            spans.push(&text[start..end]);
            start = end;
        }
    }
    spans.push(&text[start..]);
    let mut out = Vec::new();
    for span in spans {
        let us = units(span);
        let n = us.len();
        if n == 0 {
            continue;
        }
        let parts = n.div_ceil(MAX_WORDS);
        for k in 0..parts {
            let (a, b) = (k * n / parts, (k + 1) * n / parts);
            out.push(Sentence {
                text: span[us[a].0..us[b - 1].1].to_string(),
                units: b - a,
                part: parts > 1,
            });
        }
    }
    out
}

/// A block over [`MAX_WORDS`] as pieces that fit, each after the first
/// opening with the sentence that closed the one before. Code and blocks
/// that fit pass through.
fn split_long(piece: Piece) -> Vec<Piece> {
    if piece.units <= MAX_WORDS || is_code(&piece.text) {
        return vec![piece];
    }
    let mut out = Vec::new();
    let flush = |cur: &[Sentence], out: &mut Vec<Piece>| {
        let text = cur
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let units = cur.iter().map(|s| s.units).sum();
        out.push(Piece {
            path: piece.path.clone(),
            text,
            units,
        });
    };
    let mut cur: Vec<Sentence> = Vec::new();
    // How many sentences in `cur` are new, not the carried-over overlap.
    let mut fresh = 0;
    for s in sentences(&piece.text) {
        let used: usize = cur.iter().map(|s| s.units).sum();
        if fresh > 0 && used + s.units > MAX_WORDS {
            flush(&cur, &mut out);
            let last = cur.pop().expect("not empty");
            cur.clear();
            if !last.part && !s.part {
                cur.push(last);
            }
            fresh = 0;
        }
        cur.push(s);
        fresh += 1;
    }
    if fresh > 0 {
        flush(&cur, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize, w: &str) -> String {
        vec![w; n].join(" ")
    }

    fn body_words(chunk: &str, title_lines: usize) -> usize {
        if title_lines == 0 {
            return unit_count(chunk);
        }
        unit_count(chunk.split_once("\n\n").map_or(chunk, |(_, b)| b))
    }

    #[test]
    fn nothing_to_embed_is_no_chunks() {
        assert!(chunk_doc("", "").is_empty());
        assert!(chunk_doc("  ", " \n\t\n ").is_empty());
        assert!(chunk_annotation("").is_empty());
        assert!(chunk_annotation(" \n\r\n\t").is_empty());
    }

    #[test]
    fn a_doc_with_only_a_title_is_its_title() {
        assert_eq!(chunk_doc(" Release process ", ""), ["Release process"]);
        assert_eq!(
            chunk_doc("Release process", "# Only\n\n## Headings"),
            ["Release process"]
        );
    }

    #[test]
    fn a_short_doc_is_one_chunk_under_its_title() {
        assert_eq!(
            chunk_doc("Feature flags", "A flag is deleted.\n\nWhy: stale flags."),
            ["Feature flags\n\nA flag is deleted.\n\nWhy: stale flags."]
        );
    }

    #[test]
    fn every_chunk_carries_its_heading_path_and_a_title_heading_is_not_repeated() {
        let text = format!(
            "# Feature flags\n\n{}\n\n## Removal\n\n{}\n\n### Exceptions\n\n{}\n\n## Naming\n\n{}",
            words(50, "intro"),
            words(50, "removal"),
            words(50, "except"),
            words(50, "naming"),
        );
        let c = chunk_doc("Feature flags", &text);
        assert_eq!(c.len(), 4, "{c:#?}");
        assert!(c[0].starts_with("Feature flags\n\nintro"));
        assert!(c[1].starts_with("Feature flags\nRemoval\n\nremoval"));
        assert!(c[2].starts_with("Feature flags\nRemoval\nExceptions\n\nexcept"));
        assert!(c[3].starts_with("Feature flags\nNaming\n\nnaming"));
    }

    #[test]
    fn short_neighbours_merge_but_never_across_a_heading() {
        let text = "one two three\n\nfour five\n\n# Other\n\nsix seven";
        assert_eq!(
            chunk_doc("T", text),
            ["T\n\none two three\n\nfour five", "T\nOther\n\nsix seven"]
        );
    }

    #[test]
    fn merging_stops_at_the_maximum() {
        let text = format!(
            "{}\n\n{}\n\n{}",
            words(100, "a"),
            words(30, "b"),
            words(30, "c")
        );
        let c = chunk_doc("T", &text);
        assert_eq!(c.len(), 2, "{c:#?}");
        assert_eq!(body_words(&c[0], 1), 130);
        assert_eq!(body_words(&c[1], 1), 30);
        // Two blocks both at or over the minimum stay apart.
        let text = format!("{}\n\n{}", words(40, "a"), words(40, "b"));
        assert_eq!(chunk_doc("T", &text).len(), 2);
    }

    #[test]
    fn a_long_paragraph_splits_at_sentence_ends_with_one_sentence_of_overlap() {
        let sentence = |i: usize| format!("{} end{i}.", words(29, "w"));
        let para: Vec<String> = (0..12).map(sentence).collect();
        let c = chunk_doc("T", &para.join(" "));
        assert!(c.len() >= 3, "{c:#?}");
        for (a, b) in c.iter().zip(&c[1..]) {
            let last = a.rsplit(' ').next().unwrap();
            assert!(
                b.contains(&format!("{last} ")),
                "{b} does not start with {last}"
            );
        }
        assert!(c.iter().all(|ch| body_words(ch, 1) <= MAX_WORDS));
        assert!(c.last().unwrap().ends_with("end11."));
    }

    #[test]
    fn a_run_without_sentence_ends_is_cut_into_even_parts() {
        let counts = |n: usize| -> Vec<usize> {
            chunk_annotation(&words(n, "x"))
                .iter()
                .map(|s| unit_count(s))
                .collect()
        };
        assert_eq!(counts(400), [133, 133, 134]);
        assert_eq!(counts(151), [75, 76]);
        assert_eq!(counts(300), [150, 150]);
    }

    #[test]
    fn the_overlap_sentence_is_kept_even_past_the_maximum() {
        let s: Vec<String> = (0..4)
            .map(|i| format!("{} end{i}.", words(99, "w")))
            .collect();
        let c = chunk_annotation(&s.join(" "));
        assert_eq!(c.len(), 4, "{c:#?}");
        for (i, ch) in c.iter().enumerate().skip(1) {
            assert!(ch.starts_with(&s[i - 1]), "chunk {i} lost its overlap");
            assert!(ch.ends_with(&format!("end{i}.")));
        }
    }

    #[test]
    fn only_the_first_heading_can_be_the_doc_itself() {
        let text = "# T\n\nintro\n\n## A\n\nfirst\n\n# T\n\nlater";
        assert_eq!(
            chunk_doc("T", text),
            ["T\n\nintro", "T\nA\n\nfirst", "T\nT\n\nlater"]
        );
        let text = "# Other\n\nx\n\n# T\n\ny";
        assert_eq!(chunk_doc("T", text), ["T\nOther\n\nx", "T\nT\n\ny"]);
    }

    #[test]
    fn cjk_text_is_counted_by_character_and_split() {
        let han: String = "文字".repeat(1400);
        let c = chunk_annotation(&han);
        assert!(c.len() >= 18, "{} chunks", c.len());
        assert!(c.iter().all(|ch| unit_count(ch) <= MAX_WORDS));
        assert_eq!(c.concat(), han);
        let sentences = "这是一个关于发布流程的句子。".repeat(200);
        let c = chunk_annotation(&sentences);
        assert!(c.len() > 1);
        assert!(c[0].ends_with('。'));
    }

    #[test]
    fn an_unbroken_string_is_still_bounded() {
        let blob = "QUJD".repeat(12_500);
        let c = chunk_annotation(&blob);
        assert!(c.len() > 1);
        assert!(
            c.iter().all(|ch| ch.len() <= MAX_WORDS * MAX_UNIT_CHARS),
            "{}",
            c[0].len()
        );
        assert_eq!(c.concat(), blob);
    }

    #[test]
    fn units_are_words_cjk_characters_and_slices_of_long_runs() {
        assert_eq!(unit_count("the release, again."), 3);
        assert_eq!(unit_count("中文 and 日本"), 5);
        assert_eq!(unit_count(&"a".repeat(MAX_UNIT_CHARS * 2 + 1)), 3);
        assert_eq!(unit_count(" \n\t"), 0);
    }

    #[test]
    fn a_short_annotation_still_reads_crlf_and_lifts_its_headings() {
        assert_eq!(
            chunk_annotation("# Decision\r\n\r\nWe ship on Tuesday.\r\nAlways."),
            ["Decision\n\nWe ship on Tuesday.\nAlways."]
        );
        assert_eq!(
            chunk_annotation("Intro\n\n## Why\n\nBecause."),
            ["Intro\n\nWhy\n\nBecause."]
        );
    }

    #[test]
    fn a_fenced_code_block_is_never_split_and_holds_no_headings() {
        let code = format!("```sh\n# not a heading\n\n{}\n```", words(300, "echo"));
        let text = format!("Intro.\n\n{code}\n\nAfter.");
        let c = chunk_doc("T", &text);
        assert_eq!(c.len(), 3, "{c:#?}");
        assert_eq!(c[1], format!("T\n\n{code}"));
        assert!(c[2].starts_with("T\n\nAfter."));
    }

    #[test]
    fn a_short_code_block_merges_with_its_prose() {
        let c = chunk_doc("T", "Run this:\n\n~~~\nmake\n\nmake install\n~~~\n\nDone.");
        assert_eq!(
            c,
            ["T\n\nRun this:\n\n~~~\nmake\n\nmake install\n~~~\n\nDone."]
        );
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let c = chunk_doc("T", "```\ncode\n# still code");
        assert_eq!(c, ["T\n\n```\ncode\n# still code"]);
    }

    #[test]
    fn a_shorter_or_different_fence_does_not_close_a_block() {
        let c = chunk_doc("T", "````\n```\n~~~\n# code\n````\n# Real");
        assert_eq!(c, ["T\n\n````\n```\n~~~\n# code\n````"]);
    }

    #[test]
    fn crlf_reads_as_lf() {
        let lf = "# A\n\none\n\ntwo\n```\nx\n```";
        assert_eq!(
            chunk_doc("T", lf),
            chunk_doc("T", &lf.replace('\n', "\r\n"))
        );
    }

    #[test]
    fn a_list_is_one_paragraph() {
        let c = chunk_doc("T", "Steps:\n- tag\n- build\n- publish");
        assert_eq!(c, ["T\n\nSteps:\n- tag\n- build\n- publish"]);
    }

    #[test]
    fn flattened_front_matter_reads_as_the_first_paragraph() {
        let search_body =
            "description  When a feature flag has to be removed\n\n# Feature flags\n\nRuling: delete it.";
        assert_eq!(
            chunk_doc("Feature flags", search_body),
            ["Feature flags\n\ndescription  When a feature flag has to be removed\n\nRuling: delete it."]
        );
    }

    #[test]
    fn heading_syntax_is_read_strictly() {
        assert_eq!(heading("## Two ##"), Some((2, "Two")));
        assert_eq!(heading("# C#"), Some((1, "C#")));
        assert_eq!(heading("#"), Some((1, "")));
        assert_eq!(heading("#hashtag"), None);
        assert_eq!(heading("####### seven"), None);
        assert_eq!(heading("    # indented code"), None);
    }

    #[test]
    fn an_annotation_is_one_chunk_up_to_the_maximum() {
        let a = words(150, "note");
        assert_eq!(chunk_annotation(&format!("  {a}\n")), [a]);
        let c = chunk_annotation(&format!(
            "{}\n\n# Detail\n\n{}",
            words(100, "a"),
            words(100, "b")
        ));
        assert_eq!(c.len(), 2);
        assert!(c[1].starts_with("Detail\n\nb"));
    }

    #[test]
    fn sentences_end_only_before_whitespace() {
        assert_eq!(
            sentences("See v1.2.3 now. Then go! Why? Done…")
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            ["See v1.2.3 now.", "Then go!", "Why?", "Done…"]
        );
    }
}
