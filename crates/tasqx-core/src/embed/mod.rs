//! Text embeddings for memory search (DESIGN.md D196).
//!
//! A static embedding model — MinishLab's potion-base-8M, a Model2Vec
//! distillation of bge-base-en-v1.5, MIT-licensed, see `NOTICE` — compiled
//! into the binary. Inference is a lookup and a mean: the text is tokenized
//! with the model's own WordPiece tokenizer (`tokenizer.rs`, a pure-Rust
//! port), each token's row of the table is summed, and the sum is
//! L2-normalised. That is what model2vec's `StaticModel.encode` does for this
//! model — its `config.json` sets `normalize: true`, and the table carries no
//! separate token weights (the Zipf weighting was baked into the rows when
//! the model was distilled) — with two deliberate differences: nothing is
//! truncated (model2vec cuts at 512 tokens by default; long text is chunked
//! instead, [`chunk`]), and a special token written literally into the text
//! is dropped along with `[UNK]`, where model2vec drops only `[UNK]`.
//!
//! The table is stored as int8 with one f32 scale per row (see `model.rs` for
//! the file format), which moves a vector by well under 1% of cosine; the
//! golden test holds every sample to within 0.995 of the f32 model's own
//! output. Every sum runs in one fixed scalar order, so a text embeds to the
//! same bits on every run of the same build.
//!
//! [`MODEL_ID`] names everything a stored vector depends on: the table, its
//! revision, the quantisation, and the tokenizer's and chunker's versions.
//! Change any of them and the id changes with it, so no vector made the old
//! way is compared with one made the new way.

pub mod chunk;
mod model;
mod tables;
mod tokenizer;

/// Dimensions of every vector.
pub const DIMS: usize = 256;

/// Bytes in a stored vector: [`DIMS`] int8 values, then their f32 scale,
/// little-endian.
pub const BLOB_LEN: usize = DIMS + 4;

/// The upstream revision the committed table was built from. It must match
/// `REVISION` in `scripts/embedding-model.py`, which a test checks.
pub const REVISION: &str = "bf8b056651a2c21b8d2565580b8569da283cab23";

/// Bump when the tokenizer (`tokenizer.rs`) would give any text different ids.
pub const TOKENIZER_VERSION: u32 = 1;

/// Bump when [`chunk`] would cut any text differently.
pub const CHUNKER_VERSION: u32 = chunk::VERSION;

/// What every stored vector records as its model. Two vectors are
/// comparable only when their ids are equal.
pub const MODEL_ID: &str =
    "potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23/int8/tok1/chunk1";

/// The unit vector of `text`: the mean of its tokens' rows, L2-normalised.
///
/// `None` when the text has no token the model knows — empty text,
/// whitespace, or only characters and words outside the vocabulary. Such a
/// text is not near anything, and saying so beats a zero vector that every
/// cosine would score 0.
pub fn embed(text: &str) -> Option<[f32; DIMS]> {
    let model = model::model();
    let mut acc = [0.0f32; DIMS];
    let mut n = 0u32;
    for id in tokenizer::token_ids(model, text) {
        if id >= tokenizer::FIRST_ORDINARY {
            model.add_row(id, &mut acc);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    // The mean, as model2vec takes it; the normalisation below would make
    // a plain sum come out the same up to rounding.
    let n = n as f32;
    for a in &mut acc {
        *a /= n;
    }
    let norm = dot(&acc, &acc).sqrt();
    if norm == 0.0 || !norm.is_finite() {
        return None;
    }
    for a in &mut acc {
        *a /= norm;
    }
    Some(acc)
}

fn dot(a: &[f32; DIMS], b: &[f32; DIMS]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..DIMS {
        s += a[i] * b[i];
    }
    s
}

/// Cosine similarity of two vectors from [`embed`]. They are unit vectors,
/// so this is their dot product, summed in index order.
pub fn cosine(a: &[f32; DIMS], b: &[f32; DIMS]) -> f32 {
    dot(a, b)
}

/// A unit vector as it is stored: each value rounded to an int8 step of
/// `max(abs(v)) / 127`, then that scale as f32. [`dequantize`] reverses it
/// to within half a step per dimension.
pub fn quantize(v: &[f32; DIMS]) -> [u8; BLOB_LEN] {
    let max = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let scale = max / 127.0;
    let mut out = [0u8; BLOB_LEN];
    if scale > 0.0 && scale.is_finite() {
        for (o, x) in out.iter_mut().zip(v) {
            *o = (x / scale).round().clamp(-127.0, 127.0) as i8 as u8;
        }
    }
    let scale = if scale.is_finite() { scale } else { 0.0 };
    out[DIMS..].copy_from_slice(&scale.to_le_bytes());
    out
}

/// A stored vector back as f32. Not renormalised: the values are what the
/// int8 steps say.
pub fn dequantize(blob: &[u8; BLOB_LEN]) -> [f32; DIMS] {
    let scale = blob_scale(blob);
    let mut out = [0.0f32; DIMS];
    for (o, &q) in out.iter_mut().zip(&blob[..DIMS]) {
        *o = f32::from(q as i8) * scale;
    }
    out
}

fn blob_scale(blob: &[u8; BLOB_LEN]) -> f32 {
    f32::from_le_bytes([blob[DIMS], blob[DIMS + 1], blob[DIMS + 2], blob[DIMS + 3]])
}

/// Cosine similarity of a query from [`embed`] and a stored vector from
/// [`quantize`], without dequantising it: the scale cancels, so this is the
/// dot product with the int8 values over their norm, each sum in index
/// order. 0 for a stored vector that is all zeros.
pub fn cosine_quantized(query: &[f32; DIMS], stored: &[u8; BLOB_LEN]) -> f32 {
    let mut d = 0.0f32;
    let mut n = 0.0f32;
    for i in 0..DIMS {
        let q = f32::from(stored[i] as i8);
        d += query[i] * q;
        n += q * q;
    }
    if n == 0.0 {
        0.0
    } else {
        d / n.sqrt()
    }
}

/// A similarity as search compares and reports it: rounded to three
/// decimals, so that last-digit differences between CPU architectures can
/// neither move a hit across the floor nor reorder two hits (D196).
pub fn round_similarity(s: f32) -> f64 {
    (f64::from(s) * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests;
