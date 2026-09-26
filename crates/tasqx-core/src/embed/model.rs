//! The embedding table and its vocabulary, compiled in and parsed once.
//!
//! `assets/embed/model.bin` is written by `scripts/embedding-model.py`;
//! little-endian throughout:
//!
//! | field  | size                  | meaning                                  |
//! |--------|-----------------------|------------------------------------------|
//! | magic  | 4 bytes               | `TQXE`                                   |
//! | version| u32                   | `1`                                      |
//! | rows   | u32                   | vocabulary size                          |
//! | dims   | u32                   | [`DIMS`]                                 |
//! | scales | rows × f32            | per row, `max(abs(row)) / 127`           |
//! | data   | rows × dims × i8      | row-major; a value is `data * scale`     |
//!
//! `assets/embed/vocab.txt` holds one WordPiece token per line, line `n`
//! being token id `n`; the table has one row per line.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::DIMS;

const MODEL_BIN: &[u8] = include_bytes!("../../assets/embed/model.bin");
const VOCAB_TXT: &str = include_str!("../../assets/embed/vocab.txt");
const HEADER: usize = 16;

/// The parsed table: a vocabulary map and a borrowed view of the int8 rows.
pub(super) struct Model {
    pub(super) vocab: HashMap<&'static str, u32>,
    scales: Vec<f32>,
    data: &'static [u8],
}

impl Model {
    /// Add row `id`, dequantised, into `acc`, one dimension at a time in
    /// index order (D196: a fixed scalar order, so the sum is reproducible).
    pub(super) fn add_row(&self, id: u32, acc: &mut [f32; DIMS]) {
        let id = id as usize;
        let scale = self.scales[id];
        let row = &self.data[id * DIMS..(id + 1) * DIMS];
        for (a, &q) in acc.iter_mut().zip(row) {
            *a += f32::from(q as i8) * scale;
        }
    }

    /// Number of rows in the table.
    #[cfg(test)]
    pub(super) fn rows(&self) -> usize {
        self.scales.len()
    }
}

/// The model, parsed on first use and shared by every caller after that.
pub(super) fn model() -> &'static Model {
    static MODEL: OnceLock<Model> = OnceLock::new();
    MODEL.get_or_init(|| {
        parse(MODEL_BIN, VOCAB_TXT).expect("the compiled-in embedding model is valid")
    })
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Parse a `model.bin` and its vocabulary. `Err` names what is wrong; the
/// compiled-in pair is checked by a test, so at run time this cannot fail.
pub(super) fn parse(bin: &'static [u8], vocab: &'static str) -> Result<Model, String> {
    if bin.get(..4) != Some(b"TQXE".as_slice()) {
        return Err("bad magic".into());
    }
    let version = u32_at(bin, 4).ok_or("truncated header")?;
    if version != 1 {
        return Err(format!("unknown version {version}"));
    }
    let rows = u32_at(bin, 8).ok_or("truncated header")? as usize;
    let dims = u32_at(bin, 12).ok_or("truncated header")? as usize;
    if dims != DIMS {
        return Err(format!("{dims} dimensions, expected {DIMS}"));
    }
    let scales_end = HEADER + rows * 4;
    if bin.len() != scales_end + rows * DIMS {
        return Err(format!("{} bytes for {rows} rows", bin.len()));
    }
    let scales = bin[HEADER..scales_end]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect();
    let tokens: HashMap<&'static str, u32> = vocab
        .lines()
        .enumerate()
        .map(|(i, t)| (t, i as u32))
        .collect();
    if tokens.len() != rows || vocab.lines().count() != rows {
        return Err(format!(
            "{} vocabulary lines for {rows} rows",
            vocab.lines().count()
        ));
    }
    Ok(Model {
        vocab: tokens,
        scales,
        data: &bin[scales_end..],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compiled_in_model_parses() {
        let m = parse(MODEL_BIN, VOCAB_TXT).unwrap();
        assert_eq!(m.rows(), 29_528);
        assert_eq!(m.vocab["[UNK]"], 1);
        assert_eq!(m.vocab["[MASK]"], 4);
        assert!(m.vocab.contains_key("release"));
    }

    #[test]
    fn a_malformed_model_is_refused() {
        let bad_magic: &'static [u8] = b"XXXX\x01\0\0\0\x01\0\0\0\0\x01\0\0";
        assert_eq!(parse(bad_magic, "a\n").err().unwrap(), "bad magic");
        let short: &'static [u8] = b"TQXE\x01\0\0\0";
        assert_eq!(parse(short, "a\n").err().unwrap(), "truncated header");
        let v2: &'static [u8] = b"TQXE\x02\0\0\0\x01\0\0\0\0\x01\0\0";
        assert!(parse(v2, "a\n").err().unwrap().contains("version 2"));
        let dims: &'static [u8] = b"TQXE\x01\0\0\0\x01\0\0\0\x08\0\0\0";
        assert!(parse(dims, "a\n").err().unwrap().contains("8 dimensions"));
        // Header claims one row but carries no scale or data.
        let rows: &'static [u8] = b"TQXE\x01\0\0\0\x01\0\0\0\0\x01\0\0";
        assert!(parse(rows, "a\n")
            .err()
            .unwrap()
            .contains("bytes for 1 rows"));
    }

    #[test]
    fn a_vocabulary_that_does_not_match_the_rows_is_refused() {
        let mut bin = b"TQXE\x01\0\0\0\x02\0\0\0\0\x01\0\0".to_vec();
        bin.extend_from_slice(&[0; 8]);
        bin.extend_from_slice(&[0; 2 * DIMS]);
        let bin: &'static [u8] = bin.leak();
        assert!(parse(bin, "a\nb\n").is_ok());
        assert!(parse(bin, "a\n")
            .err()
            .unwrap()
            .contains("1 vocabulary lines"));
        // A duplicated token would silently shadow a row.
        assert!(parse(bin, "a\na\n").is_err());
    }

    #[test]
    fn rows_dequantise_in_index_order() {
        let mut bin = b"TQXE\x01\0\0\0\x01\0\0\0\0\x01\0\0".to_vec();
        bin.extend_from_slice(&0.5f32.to_le_bytes());
        bin.extend((0..DIMS).map(|i| (i as u8).wrapping_sub(100)));
        let m = parse(bin.leak(), "x\n").unwrap();
        let mut acc = [1.0; DIMS];
        m.add_row(0, &mut acc);
        assert_eq!(acc[0], 1.0 - 50.0);
        assert_eq!(acc[100], 1.0);
        assert_eq!(acc[127], 1.0 + 13.5);
        assert_eq!(acc[228], 1.0 - 64.0);
    }
}
