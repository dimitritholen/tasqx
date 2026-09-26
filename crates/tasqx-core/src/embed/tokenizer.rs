//! A port of the upstream BERT WordPiece tokenizer potion-base-8M ships with.
//!
//! What its `tokenizer.json` says, and so what this does, in order:
//!
//! 1. **Special tokens.** `[PAD]`, `[UNK]`, `[CLS]`, `[SEP]` and `[MASK]`,
//!    written literally in the raw text, are cut out first and become their
//!    own ids (they are not normalised: `[mask]` is ordinary text).
//! 2. **`BertNormalizer`** with `clean_text`, `handle_chinese_chars`,
//!    `lowercase`, and `strip_accents` unset, which upstream means "follow
//!    `lowercase`", so accents are stripped: whitespace becomes a space,
//!    CJK ideographs get a space either side, the text is NFD-decomposed,
//!    controls, format characters, private-use characters and non-spacing
//!    marks are removed, and what is left is lowercased.
//! 3. **`BertPreTokenizer`**: split on whitespace, and every punctuation
//!    character is a word of its own.
//! 4. **`WordPiece`**: greedy longest match from the start of each word,
//!    continuations prefixed `##`; a word with no complete cover, or longer
//!    than 100 characters, is one `[UNK]`.
//!
//! The character classes in steps 2 and 3 are not Rust's or Python's Unicode
//! tables but the upstream tokenizer's own, measured code point by code point
//! into `tables.rs` by `scripts/embedding-model.py`. The golden test in
//! `super::tests` holds the whole pipeline to the upstream ids.

use unicode_normalization::UnicodeNormalization;

use super::model::Model;
use super::tables;

/// The id of `[UNK]`.
pub(super) const UNK: u32 = 1;
/// Ids below this are special tokens (`[PAD]` … `[MASK]`).
pub(super) const FIRST_ORDINARY: u32 = 5;
const SPECIALS: [(&str, u32); 5] = [
    ("[PAD]", 0),
    ("[UNK]", 1),
    ("[CLS]", 2),
    ("[SEP]", 3),
    ("[MASK]", 4),
];
const MAX_WORD_CHARS: usize = 100;

fn in_class(class: &[(u32, u32)], c: char) -> bool {
    let c = c as u32;
    class
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Step 2: the normalised text of one segment.
pub(super) fn normalize(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len());
    for c in text.chars() {
        if in_class(tables::SPACE, c) {
            spaced.push(' ');
        } else if in_class(tables::CJK, c) {
            spaced.push(' ');
            spaced.push(c);
            spaced.push(' ');
        } else {
            spaced.push(c);
        }
    }
    spaced
        .nfd()
        .filter(|&c| !in_class(tables::DROP, c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Step 3: the words of normalised text.
pub(super) fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let punct = in_class(tables::PUNCT, c);
        if c.is_whitespace() || punct {
            if let Some(s) = start.take() {
                out.push(&text[s..i]);
            }
            if punct {
                out.push(&text[i..i + c.len_utf8()]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// Step 4: one word's ids, appended to `out`.
fn wordpiece(model: &Model, word: &str, buf: &mut String, out: &mut Vec<u32>) {
    if word.chars().count() > MAX_WORD_CHARS {
        out.push(UNK);
        return;
    }
    let mark = out.len();
    let mut start = 0;
    while start < word.len() {
        let mut end = word.len();
        let found = loop {
            buf.clear();
            if start > 0 {
                buf.push_str("##");
            }
            buf.push_str(&word[start..end]);
            if let Some(&id) = model.vocab.get(buf.as_str()) {
                break Some(id);
            }
            // Back off one character, not one byte.
            match word[start..end].char_indices().next_back() {
                Some((0, _)) | None => break None,
                Some((i, _)) => end = start + i,
            }
        };
        let Some(id) = found else {
            out.truncate(mark);
            out.push(UNK);
            return;
        };
        out.push(id);
        start = end;
    }
}

/// The token ids of `text`, exactly as the upstream tokenizer's
/// `encode(text, add_special_tokens=False).ids`: `[UNK]` and any literal
/// special token included. [`super::embed`] is what drops them.
pub(super) fn token_ids(model: &Model, text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    loop {
        let next = rest.match_indices('[').find_map(|(i, _)| {
            SPECIALS
                .iter()
                .find(|(s, _)| rest[i..].starts_with(s))
                .map(|&(s, id)| (i, s.len(), id))
        });
        let (segment, special) = match next {
            Some((i, len, id)) => (&rest[..i], Some((len, id, i))),
            None => (rest, None),
        };
        let normal = normalize(segment);
        for word in words(&normal) {
            wordpiece(model, word, &mut buf, &mut out);
        }
        match special {
            Some((len, id, i)) => {
                out.push(id);
                rest = &rest[i + len..];
            }
            None => return out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::model;
    use super::*;

    fn ids(text: &str) -> Vec<u32> {
        token_ids(model(), text)
    }

    fn id(token: &str) -> u32 {
        model().vocab[token]
    }

    #[test]
    fn the_tables_are_sorted_and_disjoint() {
        for class in [tables::SPACE, tables::CJK, tables::DROP, tables::PUNCT] {
            assert!(class.iter().all(|&(lo, hi)| lo <= hi));
            assert!(class.windows(2).all(|w| w[0].1 < w[1].0));
        }
    }

    #[test]
    fn normalising_lowercases_strips_accents_and_spaces_cjk() {
        assert_eq!(normalize("Café NAÏVE"), "cafe naive");
        assert_eq!(normalize("a\u{0}b\u{7}c\u{ad}d\u{200b}e"), "abcde");
        assert_eq!(normalize("tab\there\u{a0}nbsp"), "tab here nbsp");
        assert_eq!(normalize("中文"), " 中  文 ");
        assert_eq!(normalize("İ"), "i");
        assert_eq!(normalize("e\u{301}"), "e");
    }

    #[test]
    fn words_split_on_whitespace_and_every_punctuation_character() {
        assert_eq!(words("a,b  c"), ["a", ",", "b", "c"]);
        assert_eq!(words("x+y=z"), ["x", "+", "y", "=", "z"]);
        assert_eq!(words("don't"), ["don", "'", "t"]);
        assert_eq!(words("¿qué?"), ["¿", "qué", "?"]);
        assert_eq!(words("  "), Vec::<&str>::new());
        assert_eq!(words("…—"), ["…", "—"]);
    }

    #[test]
    fn wordpiece_takes_the_longest_prefix_then_continuations() {
        assert_eq!(ids("hello"), [id("hello")]);
        let parts = ids("embeddings");
        assert!(parts.len() > 1, "{parts:?}");
        assert_eq!(parts[0], id("em"));
        assert!(parts[1..].iter().all(|&p| p != UNK));
    }

    #[test]
    fn a_word_with_no_cover_or_over_a_hundred_chars_is_one_unk() {
        // An emoji has no row and no continuation, so the WHOLE word goes,
        // not just the emoji: upstream behaviour, pinned.
        assert_eq!(ids("ab🎉"), [UNK]);
        assert!(!ids(&"a".repeat(100)).contains(&UNK));
        assert_eq!(ids(&"a".repeat(101)), [UNK]);
    }

    #[test]
    fn literal_special_tokens_are_their_own_ids_and_only_when_exact() {
        assert_eq!(ids("[CLS]"), [2]);
        assert_eq!(ids("a[MASK]b"), [id("a"), 4, id("b")]);
        assert_eq!(ids("[mask]"), [id("["), id("mask"), id("]")]);
        assert_eq!(ids("[[SEP]]"), [id("["), 3, id("]")]);
        assert_eq!(ids("[UNK"), [id("["), id("un"), id("##k")]);
    }

    #[test]
    fn empty_and_whitespace_text_has_no_ids() {
        assert!(ids("").is_empty());
        assert!(ids(" \t\n ").is_empty());
        assert!(ids("\u{0}\u{200b}").is_empty());
    }
}
