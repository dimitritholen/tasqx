//! Cutting memory into the pieces that are embedded (D196).
//!
//! A static embedding is the mean of its tokens, so a long text's vector is
//! an average of everything it says and near nothing in particular. Memory
//! is therefore embedded a paragraph or a section at a time, and an entry
//! scores as its best chunk.
//!
//! [`chunk_doc`] reads a doc's text as markdown, just enough to find its
//! blocks:
//!
//! - A blank line ends a paragraph. A list or a table without blank lines
//!   inside is one paragraph.
//! - An ATX heading (`#` to `######`, up to three spaces of indent) ends a
//!   paragraph and sets the heading path for what follows; a heading is
//!   never a chunk on its own. Setext headings (a line of `===` or `---`
//!   under text) are read as text.
//! - A fenced code block (three or more backticks or tildes) is one block
//!   and is never split, however long, and a `#` line inside it is code,
//!   not a heading. A fence left open runs to the end of the text.
//! - A paragraph over [`MAX_WORDS`] is cut at sentence ends (`.`, `!`, `?`,
//!   `…` and their CJK forms, followed by whitespace) into pieces of at most
//!   [`MAX_WORDS`], each after the first starting with the sentence that
//!   ended the one before. A single sentence over [`MAX_WORDS`] (a pasted
//!   log line, text without punctuation) is cut every [`MAX_WORDS`] words.
//! - Neighbouring blocks under the same heading path merge while either is
//!   under [`MIN_WORDS`] and the two together are at most [`MAX_WORDS`].
//! - Every chunk starts with the doc's title and its heading path, one per
//!   line, then a blank line, then its text. A first heading that repeats
//!   the title (as a doc's `# Title` line usually does) is the doc itself:
//!   it is not repeated, and text above and below it is one section.
//!
//! Front matter needs no rule here: what is chunked is a doc's
//! `search_body`, where a front matter block has already been flattened
//! into plain `key  value` lines (D135), which read as a paragraph and merge
//! with their neighbours like any other. `\r\n` reads as `\n`.
//!
//! Word counts are whitespace-separated runs, the prefix not included.

/// Bump when any text would be cut differently; it is part of
/// [`super::MODEL_ID`].
pub const VERSION: u32 = 1;

/// A block this short merges with its neighbour when the two fit.
pub const MIN_WORDS: usize = 40;

/// No chunk is longer than this, except a fenced code block.
pub const MAX_WORDS: usize = 150;

#[derive(Debug)]
struct Piece {
    path: Vec<String>,
    text: String,
    words: usize,
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The chunks of a doc titled `title` whose text is `text` (its
/// `search_body`). A doc with no text is one chunk, its title; a doc with
/// neither has none.
pub fn chunk_doc(title: &str, text: &str) -> Vec<String> {
    let title = title.trim();
    let chunks = chunks(text, title);
    if chunks.is_empty() && !title.is_empty() {
        return vec![title.to_string()];
    }
    chunks
}

/// The chunks of an annotation: the whole body as one chunk, unless it is
/// over [`MAX_WORDS`], when it is cut as a doc with no title would be.
/// Empty or whitespace-only text has none.
pub fn chunk_annotation(body: &str) -> Vec<String> {
    let body = body.trim();
    match word_count(body) {
        0 => Vec::new(),
        n if n <= MAX_WORDS => vec![body.to_string()],
        _ => chunks(body, ""),
    }
}

fn chunks(text: &str, title: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n");
    let mut merged: Vec<Piece> = Vec::new();
    for mut piece in blocks(&text).into_iter().flat_map(split_long) {
        // A first heading that repeats the title is the doc itself, not a
        // section of it: what sits above it and below it is one section.
        if piece
            .path
            .first()
            .is_some_and(|h| h.eq_ignore_ascii_case(title))
        {
            piece.path.remove(0);
        }
        if let Some(last) = merged.last_mut() {
            if last.path == piece.path
                && (last.words < MIN_WORDS || piece.words < MIN_WORDS)
                && last.words + piece.words <= MAX_WORDS
            {
                last.text.push_str("\n\n");
                last.text.push_str(&piece.text);
                last.words += piece.words;
                continue;
            }
        }
        merged.push(piece);
    }
    merged
        .into_iter()
        .map(|p| with_prefix(title, &p.path, &p.text))
        .collect()
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

fn blocks(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut fence: Option<((char, usize), Vec<&str>)> = None;
    let names = |path: &[(usize, String)]| path.iter().map(|(_, h)| h.clone()).collect();
    let emit = |out: &mut Vec<Piece>, path: Vec<String>, lines: &mut Vec<&str>| {
        let text = lines.join("\n").trim_matches('\n').to_string();
        lines.clear();
        let words = word_count(&text);
        if words > 0 {
            out.push(Piece { path, text, words });
        }
    };
    for line in text.split('\n') {
        if let Some((open, code)) = &mut fence {
            code.push(line);
            if fence_closes(line, *open) {
                let (_, mut code) = fence.take().expect("inside a fence");
                emit(&mut out, names(&path), &mut code);
            }
            continue;
        }
        if let Some(open) = fence_open(line) {
            emit(&mut out, names(&path), &mut para);
            fence = Some((open, vec![line]));
        } else if let Some((level, h)) = heading(line) {
            emit(&mut out, names(&path), &mut para);
            while path.last().is_some_and(|(l, _)| *l >= level) {
                path.pop();
            }
            if !h.is_empty() {
                path.push((level, h.to_string()));
            }
        } else if line.trim().is_empty() {
            emit(&mut out, names(&path), &mut para);
        } else {
            para.push(line);
        }
    }
    emit(&mut out, names(&path), &mut para);
    if let Some((_, mut code)) = fence {
        emit(&mut out, names(&path), &mut code);
    }
    out
}

fn is_code(text: &str) -> bool {
    fence_open(text.lines().next().unwrap_or("")).is_some()
}

/// The sentences of a paragraph, each at most [`MAX_WORDS`].
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends = matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？');
        if ends && chars.peek().is_none_or(|&(_, n)| n.is_whitespace()) {
            let end = i + c.len_utf8();
            out.push(&text[start..end]);
            start = end;
        }
    }
    out.push(&text[start..]);
    let mut result = Vec::new();
    for s in out {
        let words: Vec<&str> = s.split_whitespace().collect();
        for part in words.chunks(MAX_WORDS) {
            result.push(part.join(" "));
        }
    }
    result
}

/// A block over [`MAX_WORDS`] as pieces that fit, with one sentence of
/// overlap between neighbours. Code and blocks that fit pass through.
fn split_long(piece: Piece) -> Vec<Piece> {
    if piece.words <= MAX_WORDS || is_code(&piece.text) {
        return vec![piece];
    }
    let mut out = Vec::new();
    let mut cur: Vec<(String, usize)> = Vec::new();
    let mut cur_words = 0;
    let flush = |cur: &[(String, usize)], words: usize, out: &mut Vec<Piece>| {
        let text = cur
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Piece {
            path: piece.path.clone(),
            text,
            words,
        });
    };
    for s in sentences(&piece.text) {
        let n = word_count(&s);
        if !cur.is_empty() && cur_words + n > MAX_WORDS {
            flush(&cur, cur_words, &mut out);
            let last = cur.pop().expect("not empty");
            cur.clear();
            cur_words = 0;
            if last.1 + n <= MAX_WORDS {
                cur_words = last.1;
                cur.push(last);
            }
        }
        cur_words += n;
        cur.push((s, n));
    }
    if !cur.is_empty() {
        flush(&cur, cur_words, &mut out);
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
            return word_count(chunk);
        }
        word_count(chunk.split_once("\n\n").map_or(chunk, |(_, b)| b))
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
    fn a_long_paragraph_without_punctuation_is_cut_every_max_words() {
        let c = chunk_annotation(&words(400, "x"));
        let counts: Vec<usize> = c.iter().map(|s| word_count(s)).collect();
        assert_eq!(counts, [150, 150, 100]);
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
            sentences("See v1.2.3 now. Then go! Why? Done…"),
            ["See v1.2.3 now.", "Then go!", "Why?", "Done…"]
        );
    }
}
