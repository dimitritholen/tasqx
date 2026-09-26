#!/usr/bin/env python3
"""Build the embedding model tasqx compiles into its binary (DESIGN.md D196).

    python3 -m venv target/embed-venv
    target/embed-venv/bin/pip install numpy safetensors tokenizers==0.23.2 model2vec==0.9.0
    target/embed-venv/bin/python scripts/embedding-model.py [--cache DIR]
    cargo fmt --all

It downloads MinishLab's potion-base-8M at the pinned REVISION below, refuses
any file whose SHA-256 differs from the one written here, and writes four
files, all committed:

- crates/tasqx-core/assets/embed/model.bin   the table, per-row int8 (format below)
- crates/tasqx-core/assets/embed/vocab.txt   the WordPiece vocabulary, one token
                                             per line, line N = token id N
- crates/tasqx-core/src/embed/tables.rs      the character classes the upstream
                                             normalizer and pre-tokenizer use,
                                             measured from `tokenizers` itself
- crates/tasqx-core/src/embed/golden.json    the golden sample: token ids from
                                             `tokenizers`, vectors from
                                             model2vec's StaticModel on the f32
                                             table

model.bin, little-endian throughout:

    magic   4 bytes  b"TQXE"
    version u32      1
    rows    u32      vocabulary size
    dims    u32      256
    scales  rows x f32   one per row: max(|row|) / 127
    data    rows x dims x i8   row-major; value = data * scale

Nothing here is needed to build tasqx; it is how the committed assets were
made, and how to remake them for a new revision. Changing REVISION changes the
model id in crates/tasqx-core/src/embed/mod.rs too, which makes every stored
vector stale (D196).

Why the character classes are measured instead of taken from a Unicode
database: the upstream Rust tokenizer classifies characters with its own
Unicode tables, whose version is not Python's or Rust's. Asking the upstream
normalizer and pre-tokenizer about every code point is the only way the Rust
port agrees with the tokenizer the table was trained with, and the golden
test is what proves it does.
"""

import argparse
import base64
import hashlib
import json
import random
import struct
import sys
import urllib.request
from pathlib import Path

REPO = "minishlab/potion-base-8M"
REVISION = "bf8b056651a2c21b8d2565580b8569da283cab23"
FILES = {
    "config.json": "2a6ac0e9aaa356a68a5688070db78fc3a464fefe85d2f06a1905ce3718687553",
    "model.safetensors": "f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2",
    "tokenizer.json": "e67e803f624fb4d67dea1c730d06e1067e1b14d830e2c2202569e3ef0f70bb50",
}
DIMS = 256
ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "crates/tasqx-core/assets/embed"
SRC = ROOT / "crates/tasqx-core/src/embed"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def fetch(cache):
    """Every pinned file, from `cache` when it holds the right bytes, else downloaded."""
    cache.mkdir(parents=True, exist_ok=True)
    for name, want in FILES.items():
        path = cache / name
        if path.exists() and sha256(path) == want:
            continue
        url = f"https://huggingface.co/{REPO}/resolve/{REVISION}/{name}"
        print(f"downloading {url}", file=sys.stderr)
        tmp = path.with_suffix(path.suffix + ".part")
        urllib.request.urlretrieve(url, tmp)
        got = sha256(tmp)
        if got != want:
            tmp.unlink()
            sys.exit(f"{name}: SHA-256 {got}, expected {want}; refusing it")
        tmp.replace(path)
    return cache


def load(cache):
    import numpy as np
    from safetensors import safe_open

    config = json.loads((cache / "config.json").read_text())
    # What model2vec does beyond a lookup and a mean is decided by these.
    # A new revision that changes them needs the Rust side changed too.
    assert config.get("normalize") is True, config
    with safe_open(cache / "model.safetensors", framework="numpy") as f:
        keys = set(f.keys())
        assert keys == {"embeddings"}, f"weights or a token mapping appeared: {keys}"
        table = f.get_tensor("embeddings").astype(np.float32)
    tok = json.loads((cache / "tokenizer.json").read_text())
    assert tok["normalizer"] == {
        "type": "BertNormalizer",
        "clean_text": True,
        "handle_chinese_chars": True,
        "strip_accents": None,  # None follows `lowercase`, so accents are stripped
        "lowercase": True,
    }, tok["normalizer"]
    assert tok["pre_tokenizer"] == {"type": "BertPreTokenizer"}, tok["pre_tokenizer"]
    model = tok["model"]
    assert model["type"] == "WordPiece", model["type"]
    assert model["unk_token"] == "[UNK]"
    assert model["continuing_subword_prefix"] == "##"
    assert model["max_input_chars_per_word"] == 100
    specials = [(a["id"], a["content"]) for a in tok["added_tokens"]]
    assert specials == [(0, "[PAD]"), (1, "[UNK]"), (2, "[CLS]"), (3, "[SEP]"), (4, "[MASK]")], specials
    assert all(not a["normalized"] and not a["lstrip"] and not a["rstrip"] and not a["single_word"] for a in tok["added_tokens"])
    vocab = sorted(model["vocab"].items(), key=lambda kv: kv[1])
    assert [i for _, i in vocab] == list(range(len(vocab)))
    assert table.shape == (len(vocab), DIMS), table.shape
    return table, [t for t, _ in vocab]


def write_model(table, tokens):
    import numpy as np

    scale = np.abs(table).max(axis=1) / 127.0
    safe = np.where(scale == 0, 1.0, scale)
    q = np.clip(np.rint(table / safe[:, None]), -127, 127).astype(np.int8)
    ASSETS.mkdir(parents=True, exist_ok=True)
    with open(ASSETS / "model.bin", "wb") as f:
        f.write(b"TQXE")
        f.write(struct.pack("<III", 1, table.shape[0], DIMS))
        f.write(scale.astype("<f4").tobytes())
        f.write(q.tobytes())
    for t in tokens:
        assert "\n" not in t and "\r" not in t and t == t.strip(), repr(t)
    (ASSETS / "vocab.txt").write_text("".join(t + "\n" for t in tokens), encoding="utf-8")


def ranges(points):
    out = []
    for c in sorted(points):
        if out and out[-1][1] == c - 1:
            out[-1][1] = c
        else:
            out.append([c, c])
    return out


def hangul(c):
    """The canonical decomposition of a precomposed Hangul syllable (Unicode ch. 3.12)."""
    s = c - 0xAC00
    t = 0x11A7 + s % 28
    return chr(0x1100 + s // 588) + chr(0x1161 + (s % 588) // 28) + (chr(t) if t != 0x11A7 else "")


def rust_str(s):
    out = []
    for ch in s:
        if ch in '"\\' or not (0x20 <= ord(ch) < 0x7F):
            out.append(f"\\u{{{ord(ch):x}}}")
        else:
            out.append(ch)
    return '"' + "".join(out) + '"'


FNV_OFFSET, FNV_PRIME = 0xCBF29CE484222325, 0x100000001B3


def fnv(h, data):
    for b in data:
        h = ((h ^ b) * FNV_PRIME) & 0xFFFFFFFFFFFFFFFF
    return h


def write_tables(tokenizer):
    """Measure the upstream normalizer and pre-tokenizer on every code point.

    The normalizer works one character at a time (clean, CJK spacing, NFD,
    mark removal, lowercase), so what it makes of each code point on its own
    is the whole of it: a code point maps to nothing, a space, itself between
    spaces, itself, or some other string. Everything that is not itself is
    written down here, so the Rust side needs no Unicode data of its own.
    Precomposed Hangul, 11,172 syllables, is the one class computed instead
    of listed, and only because every syllable was checked to decompose by
    the standard formula. Two checksums over all code points let a Rust test
    prove the port agrees everywhere, not just on the golden sample.
    """
    norm, pre = tokenizer.normalizer, tokenizer.pre_tokenizer
    space, cjk, drop, punct, white, mapped = set(), set(), set(), set(), set(), {}
    norm_sum, pre_sum = FNV_OFFSET, FNV_OFFSET
    for c in range(0x110000):
        if 0xD800 <= c <= 0xDFFF:
            continue
        ch = chr(c)
        out = norm.normalize_str(ch)
        norm_sum = fnv(norm_sum, out.encode() + b"\xff")
        if out == "":
            drop.add(c)
        elif out == " " and ch != " ":
            space.add(c)
        elif out == " " + ch + " ":
            cjk.add(c)
        elif out == ch:
            pass
        elif 0xAC00 <= c <= 0xD7A3:
            assert out == hangul(c), hex(c)
        else:
            mapped[c] = out
        words = [w for w, _ in pre.pre_tokenize_str("a" + ch + "a")]
        if words == ["a", "a"]:
            white.add(c)
        elif len(words) == 3:
            punct.add(c)
        pre_sum = fnv(pre_sum, [1 if c in white else 2 if c in punct else 0])
    doc = {
        "SPACE": "Characters the normalizer turns into a plain space.",
        "CJK": "Characters the normalizer surrounds with spaces, so each is a word of its own.",
        "DROP": "Characters the normalizer removes: controls, format characters, private use,\n/// and non-spacing marks.",
        "WHITE": "Characters the pre-tokenizer splits on and drops.",
        "PUNCT": "Characters the pre-tokenizer splits off as words of their own.",
    }
    lines = [
        "// @generated by scripts/embedding-model.py from `tokenizers` "
        f"for {REPO}@{REVISION}. Do not edit.",
        "",
        "//! The upstream tokenizer's character handling, measured one code point at a",
        "//! time. The range classes are sorted, disjoint, inclusive `(first, last)`.",
        "",
    ]
    for name, points in [("SPACE", space), ("CJK", cjk), ("DROP", drop), ("WHITE", white), ("PUNCT", punct)]:
        lines.append(f"/// {doc[name]}")
        lines.append(f"pub(super) const {name}: &[(u32, u32)] = &[")
        lines.extend(f"    (0x{a:X}, 0x{b:X})," for a, b in ranges(points))
        lines.append("];")
        lines.append("")
    lines.append("/// Every other code point the normalizer changes, and what it becomes: an")
    lines.append("/// accent taken off, a letter lowercased, a compatibility ideograph spaced.")
    lines.append("/// Sorted by code point. Precomposed Hangul is not here; see `tokenizer.rs`.")
    lines.append("pub(super) const MAP: &[(u32, &str)] = &[")
    lines.extend(f"    (0x{c:X}, {rust_str(mapped[c])})," for c in sorted(mapped))
    lines.append("];")
    lines.append("")
    lines.append("/// FNV-1a 64 over the normalizer's output for every code point, each followed")
    lines.append("/// by a 0xFF byte, surrogates skipped.")
    lines.append("#[cfg(test)]")
    lines.append(f"pub(super) const NORMALIZE_FNV: u64 = 0x{norm_sum:016X};")
    lines.append("")
    lines.append("/// FNV-1a 64 over one byte per code point: 1 whitespace, 2 punctuation, 0 other.")
    lines.append("#[cfg(test)]")
    lines.append(f"pub(super) const PRETOKENIZE_FNV: u64 = 0x{pre_sum:016X};")
    lines.append("")
    (SRC / "tables.rs").write_text("\n".join(lines), encoding="utf-8")


HANDWRITTEN = [
    "",
    "   ",
    "\t\n\r",
    "hello",
    "Hello, World!",
    "Login fails after the session expires",
    "sign-in broken when the token times out",
    "When do we ship the release?",
    "Release process: tag, build, publish the formula.",
    "café naïve résumé coöperation",
    "Ångström Øresund Straße façade",
    "Ärger über Öl und Übermut",
    "ÉCOLE ÉLÉMENTAIRE",
    "İstanbul ıi",
    "ﬁne ﬂow ligatures",
    "中文分词测试",
    "日本語のテキストです",
    "한국어 문장입니다",
    "mixed 中文 and English",
    "emoji 🎉 party 🚀 launch 👍🏽",
    "🙂🙃😀",
    "family 👨‍👩‍👧 zwj",
    "flags 🇳🇱 🇬🇧",
    "don't won't it's",
    "\"quoted\" 'single' «guillemets» „Dutch quotes“",
    "em—dash en–dash hyphen-minus ‐ ‑",
    "a...b…c",
    "100% of $5 costs €4.50 or £3",
    "x+y=z; a<b>c; ^caret `tick` |pipe| ~tilde",
    "snake_case_identifier and camelCaseIdentifier and SCREAMING_CASE",
    "fn main() { println!(\"hi\"); }",
    "crates/tasqx-core/src/engine/memory.rs:1343",
    "memory.search(query, mode=hybrid)",
    "https://github.com/tholen/tasqx/pull/836?tab=files#diff-1",
    "mailto:someone@example.com",
    "D41 D196 #835 v0.12.3 2026-09-26T10:00:00Z",
    "0x7fff_ffff 1e-9 3.14159",
    "supercalifragilisticexpialidocious",
    "a" * 99,
    "b" * 100,
    "c" * 101,
    "pneumonoultramicroscopicsilicovolcanoconiosis" * 3,
    "De vergadering is verplaatst naar donderdag.",
    "Wij releasen elke dinsdag, tenzij de pipeline rood is.",
    "Het inloggen mislukt na het verlopen van de sessie.",
    "zeeën, ruïne, geïnteresseerd, coördinatie",
    "Ĳsselmeer ĳs",
    "[CLS] start [SEP] end",
    "a [MASK] b [UNK] c [PAD]",
    "[mask] lower-case is not special",
    "[[CLS]]",
    "zero​width‌joiner‍non",
    "soft­hyphen",
    "bidi ‮rtl‬ mark",
    "nbsp here and em space",
    "line separator para",
    "tab\there\nnewline\r\ncrlf",
    "control\u0007bell\u0000nul\u001bescape",
    "replacement � char",
    "private  use",
    "combining é à ö",
    "Devanagari हिन्दी पाठ",
    "Thai ภาษาไทย",
    "Arabic العربية النص",
    "Hebrew עברית",
    "Greek Ελληνικά κείμενο ΑΒΓ",
    "Cyrillic Русский текст",
    "math ∑ ∫ √ ≤ ≥ ≠ ∞",
    "arrows → ← ↑ ↓ ⇒",
    "box ┌─┐ │ └─┘",
    "fullwidth ＡＢＣ １２３ ！？",
    "half ｶﾀｶﾅ",
    "superscript x² H₂O ½",
    "roman Ⅻ ⅷ",
    "circled ① ② ⓐ",
    "¿Qué? ¡Hola!",
    "Ænima œuvre",
    "ß ẞ",
    "ǅ ǈ titlecase",
    "The quick brown fox jumps over the lazy dog.",
    "THE QUICK BROWN FOX",
    "- [ ] todo item\n- [x] done item",
    "# Heading\n\nParagraph under it.",
    "```rust\nlet x = 1;\n```",
    "| a | b |\n|---|---|\n| 1 | 2 |",
    "> quoted line",
    "**bold** _italic_ ~~strike~~",
    "tasqx memory search --mode semantic \"release process\"",
    "TASQX_DB=/tmp/x.db tasqx --no-daemon list +bug",
    "ERROR: database is locked (SQLITE_BUSY)",
    "thread 'main' panicked at src/main.rs:10:5",
    "Mr. Smith went to Washington. He arrived at 5 p.m.",
    "e.g. i.e. etc. vs.",
    "",
]

POOL = [
    "release", "deploy", "login", "session", "token", "database", "migration",
    "search", "memory", "annotation", "embedding", "vector", "cosine", "chunk",
    "café", "naïve", "façade", "über", "zeeën", "geïnteresseerd", "déjà",
    "中文", "日本", "한국", "🎉", "🚀", "👍", "✅", "❌",
    "fn", "impl", "struct", "Vec<String>", "Option<&str>", "HashMap::new()",
    "snake_case", "camelCase", "kebab-case", "PascalCase", "UPPER_CASE",
    "https://example.com/a/b?c=d", "user@example.org", "v1.2.3", "#836", "D196",
    "don't", "it's", "—", "…", "(", ")", "[", "]", "{", "}", "!", "?", ".", ",",
    "vergadering", "donderdag", "uitrol", "beslissing", "waarom", "inloggen",
    "the", "a", "of", "and", "to", "in", "is", "that", "for", "on",
    "antidisestablishmentarianism", "xylophone", "quizzically", "zzzzzz",
    "3.14", "42", "1e10", "0xFF", "100%", "$5", "€", "²",
]


def samples():
    rng = random.Random(836)
    out = list(HANDWRITTEN)
    while len(out) < 300:
        n = rng.randint(1, 18)
        words = [rng.choice(POOL) for _ in range(n)]
        seps = [rng.choice([" ", " ", " ", "", "\n", ", ", "-", "/"]) for _ in range(n)]
        text = "".join(w + s for w, s in zip(words, seps))
        if rng.random() < 0.3:
            text = text.upper()
        elif rng.random() < 0.3:
            text = text.title()
        out.append(text)
    return out


def i16_b64(vec):
    import numpy as np

    return base64.b64encode(np.rint(vec * 32767).astype("<i2").tobytes()).decode()


def write_golden(cache, tokenizer):
    import numpy as np
    from model2vec import StaticModel

    model = StaticModel.from_pretrained(str(cache))
    rows = []
    for text in samples():
        ids = tokenizer.encode(text, add_special_tokens=False).ids
        # model2vec drops [UNK] but keeps a special token written literally
        # into the text; tasqx drops those too (D196), so the two vectors
        # only agree when there is none.
        special = any(i in (0, 2, 3, 4) for i in ids)
        # max_length=None: model2vec truncates at 512 tokens by default, tasqx never.
        vec = model.encode([text], max_length=None)[0]
        row = {"text": text, "ids": ids}
        if special:
            row["special"] = True
        elif float(np.linalg.norm(vec)) == 0.0:
            row["vec"] = None
        else:
            row["vec"] = i16_b64(vec)
        rows.append(row)
    head = {
        "model": f"{REPO}@{REVISION}",
        "note": "vec: the f32 model's unit vector, i16 little-endian (x * 32767), base64",
    }
    # One sample per line, so a regenerated fixture diffs by sample.
    body = ",\n".join(json.dumps(r, ensure_ascii=False) for r in rows)
    text = json.dumps(head, ensure_ascii=False)[:-1] + ', "samples": [\n' + body + "\n]}\n"
    json.loads(text)
    (SRC / "golden.json").write_text(text, encoding="utf-8")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--cache", type=Path, default=ROOT / "target/embed-model", help="where the downloaded files are kept")
    args = ap.parse_args()
    from tokenizers import Tokenizer

    cache = fetch(args.cache)
    table, tokens = load(cache)
    tokenizer = Tokenizer.from_file(str(cache / "tokenizer.json"))
    write_model(table, tokens)
    write_tables(tokenizer)
    write_golden(cache, tokenizer)
    print(f"wrote {ASSETS}/model.bin, vocab.txt and {SRC}/tables.rs, golden.json", file=sys.stderr)


if __name__ == "__main__":
    main()
