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
//! output.
//!
//! Every sum here — the rows into a mean, a norm, a dot product — runs as
//! plain f32 additions in index order, with no parallel or SIMD reduction to
//! regroup them, so a text embeds to the same bits and two vectors score the
//! same similarity on every run of the same build. [`round_similarity`] does
//! not provide that and is not needed for it; it only keeps the noise of the
//! last digits out of what a caller compares and shows.
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
pub const TOKENIZER_VERSION: u32 = 2;

/// Bump when [`chunk`] would cut any text differently.
pub const CHUNKER_VERSION: u32 = chunk::VERSION;

/// What every stored vector records as its model. Two vectors are
/// comparable only when their ids are equal.
pub const MODEL_ID: &str =
    "potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23/int8/tok2/chunk3";

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
/// [`quantize`], without dequantising it. 0 for a stored vector that is all
/// zeros. [`QueryVector`] and [`StoredVector`] are the same computation with
/// each side prepared once, which is how search scores many vectors.
pub fn cosine_quantized(query: &[f32; DIMS], stored: &[u8; BLOB_LEN]) -> f32 {
    StoredVector::from_blob(stored).map_or(0.0, |s| s.similarity(&QueryVector::new(query)))
}

/// Largest magnitude a query component takes in fixed point.
const QUERY_STEPS: f32 = 32767.0;

/// A query in i16 fixed point: each value rounded to a step of
/// `max(abs(v)) / 32767`, which moves a similarity by well under 1e-4 (a
/// test holds the golden set to that). Its dot product with a stored
/// vector's int8 values is integer arithmetic, so it is exact: the sum has
/// one value whatever order it is taken in, the compiler may vectorise it,
/// and every CPU gives the same bits.
pub struct QueryVector {
    q: [i16; DIMS],
    scale: f64,
}

impl QueryVector {
    /// `v` in fixed point. An all-zero vector gives a query every stored
    /// vector scores 0 against.
    pub fn new(v: &[f32; DIMS]) -> QueryVector {
        let max = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let mut q = [0i16; DIMS];
        if max == 0.0 || !max.is_finite() {
            return QueryVector { q, scale: 0.0 };
        }
        let step = max / QUERY_STEPS;
        for (o, x) in q.iter_mut().zip(v) {
            *o = (x / step).round().clamp(-QUERY_STEPS, QUERY_STEPS) as i16;
        }
        QueryVector {
            q,
            scale: f64::from(step),
        }
    }
}

/// A stored vector's int8 values and their norm, read once from its blob.
/// The blob's own scale cancels out of a cosine and is not needed.
pub struct StoredVector {
    q: [i8; DIMS],
    norm: f64,
}

impl StoredVector {
    /// `None` unless `blob` is exactly [`BLOB_LEN`] bytes.
    pub fn from_blob(blob: &[u8]) -> Option<StoredVector> {
        if blob.len() != BLOB_LEN {
            return None;
        }
        let mut q = [0i8; DIMS];
        for (o, &b) in q.iter_mut().zip(&blob[..DIMS]) {
            *o = b as i8;
        }
        // At most 256 * 127^2, far inside i32.
        let n: i32 = q.iter().map(|&x| i32::from(x) * i32::from(x)).sum();
        Some(StoredVector {
            q,
            norm: f64::from(n).sqrt(),
        })
    }

    /// The cosine of this vector and `query`. The dot product is exact in
    /// i32 (at most 256 * 32767 * 127, under 2^31); the one division is in
    /// f64 and correctly rounded, so the result is the same on every CPU.
    pub fn similarity(&self, query: &QueryVector) -> f32 {
        if self.norm == 0.0 || query.scale == 0.0 {
            return 0.0;
        }
        let dot: i32 = self
            .q
            .iter()
            .zip(&query.q)
            .map(|(&s, &q)| i32::from(s) * i32::from(q))
            .sum();
        (f64::from(dot) * query.scale / self.norm) as f32
    }
}

/// A similarity as search compares and reports it: three decimals, and
/// never `-0.0`. Determinism does not rest on this (see the module docs);
/// what it removes is display noise, and any last-digit difference a
/// different CPU's float arithmetic could still introduce (D196).
pub fn round_similarity(s: f32) -> f64 {
    // `+ 0.0` turns a rounded `-0.0` into `0.0` and leaves everything else.
    (f64::from(s) * 1000.0).round() / 1000.0 + 0.0
}

/// The attribution and licence texts of the model and vocabulary compiled
/// into this crate: the repository's `NOTICE`, carried in the binary so a
/// build installed without its archive still has it (`tasqx about --notices`).
pub const NOTICE: &str = include_str!("../../../../NOTICE");

#[cfg(test)]
mod tests;
