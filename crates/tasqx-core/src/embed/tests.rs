use super::*;

const GOLDEN: &str = include_str!("golden.json");
const SCRIPT: &str = include_str!("../../../../scripts/embedding-model.py");

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {c}"),
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= u32::from(val(c)) << (18 - 6 * i);
        }
        out.extend(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

fn reference(b64: &str) -> [f32; DIMS] {
    let bytes = base64(b64);
    assert_eq!(bytes.len(), DIMS * 2);
    let mut v = [0.0; DIMS];
    for (o, b) in v.iter_mut().zip(bytes.chunks_exact(2)) {
        *o = f32::from(i16::from_le_bytes([b[0], b[1]])) / 32767.0;
    }
    v
}

fn norm(v: &[f32; DIMS]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

#[test]
fn the_model_id_names_the_revision_and_both_versions() {
    assert_eq!(
        MODEL_ID,
        format!("potion-base-8M@{REVISION}/int8/tok{TOKENIZER_VERSION}/chunk{CHUNKER_VERSION}")
    );
    assert!(
        SCRIPT.contains(&format!("REVISION = \"{REVISION}\"")),
        "scripts/embedding-model.py pins a different revision than REVISION"
    );
    assert!(GOLDEN.contains(REVISION));
    assert!(include_str!("tables.rs").contains(REVISION));
}

#[test]
fn golden_token_ids_match_the_upstream_tokenizer_exactly() {
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let samples = golden["samples"].as_array().unwrap();
    assert!(samples.len() >= 300);
    let mut wrong = Vec::new();
    for s in samples {
        let text = s["text"].as_str().unwrap();
        let want: Vec<u32> = s["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let got = tokenizer::token_ids(model::model(), text);
        if got != want {
            wrong.push(format!("{text:?}\n  want {want:?}\n   got {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {}:\n{}",
        wrong.len(),
        samples.len(),
        wrong.join("\n")
    );
}

#[test]
fn golden_vectors_match_the_f32_model_within_quantisation() {
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let mut checked = 0;
    let mut worst = (1.0f32, String::new());
    for s in golden["samples"].as_array().unwrap() {
        let text = s["text"].as_str().unwrap();
        if s.get("special").is_some() {
            continue;
        }
        let got = embed(text);
        match s["vec"].as_str() {
            None => assert!(got.is_none(), "{text:?} has no known token upstream"),
            Some(b64) => {
                let got = got.unwrap_or_else(|| panic!("{text:?} has no vector"));
                assert!((norm(&got) - 1.0).abs() < 1e-5);
                let want = reference(b64);
                let c = cosine(&got, &want) / norm(&want);
                if c < worst.0 {
                    worst = (c, text.to_string());
                }
                checked += 1;
            }
        }
    }
    assert!(checked >= 250, "{checked}");
    assert!(worst.0 >= 0.995, "cosine {} for {:?}", worst.0, worst.1);
}

#[test]
fn text_with_no_known_token_has_no_vector() {
    assert_eq!(embed(""), None);
    assert_eq!(embed("   \n"), None);
    assert_eq!(embed("🎉🚀"), None);
    assert_eq!(embed("[CLS] [SEP]"), None);
    assert!(embed("release").is_some());
}

#[test]
fn special_tokens_do_not_move_a_vector() {
    assert_eq!(embed("ship the release [SEP]"), embed("ship the release"));
    assert_eq!(embed("ship 🎉 the release"), embed("ship the release"));
}

#[test]
fn embedding_is_deterministic_and_meaning_is_nearer_than_noise() {
    let a = embed("the login fails after the session expires").unwrap();
    assert_eq!(Some(a), embed("the login fails after the session expires"));
    let b = embed("sign-in broken when the token times out").unwrap();
    let c = embed("a recipe for banana bread with walnuts").unwrap();
    assert!(
        cosine(&a, &b) > cosine(&a, &c) + 0.1,
        "{} vs {}",
        cosine(&a, &b),
        cosine(&a, &c)
    );
    assert!((cosine(&a, &a) - 1.0).abs() < 1e-5);
}

#[test]
fn a_quantised_vector_round_trips_within_a_step_and_keeps_its_cosine() {
    let texts = [
        "when do we ship the release",
        "release process: tag, build, publish",
        "the database is locked",
        "café naïve résumé",
        "De vergadering is verplaatst naar donderdag.",
    ];
    for t in texts {
        let v = embed(t).unwrap();
        let blob = quantize(&v);
        let scale = f32::from_le_bytes(blob[DIMS..].try_into().unwrap());
        let back = dequantize(&blob);
        for (x, y) in v.iter().zip(&back) {
            assert!((x - y).abs() <= scale / 2.0 + 1e-7);
        }
        for q in texts {
            let q = embed(q).unwrap();
            let exact = cosine(&q, &v);
            let stored = cosine_quantized(&q, &blob);
            assert!((exact - stored).abs() < 0.005, "{t}: {exact} vs {stored}");
        }
        assert!((cosine_quantized(&v, &blob) - 1.0).abs() < 1e-3);
    }
}

#[test]
fn the_blob_is_int8_values_then_a_little_endian_scale() {
    let mut v = [0.0f32; DIMS];
    v[0] = 0.5;
    v[1] = -0.25;
    v[255] = 0.125;
    let blob = quantize(&v);
    assert_eq!(blob.len(), 260);
    assert_eq!(blob[0] as i8, 127);
    assert_eq!(blob[1] as i8, -64);
    assert_eq!(blob[255] as i8, 32);
    assert_eq!(blob[DIMS..], (0.5f32 / 127.0).to_le_bytes());
    let zero = quantize(&[0.0; DIMS]);
    assert_eq!(zero, [0u8; BLOB_LEN]);
    assert_eq!(dequantize(&zero), [0.0; DIMS]);
    assert_eq!(cosine_quantized(&v, &zero), 0.0);
}

#[test]
fn similarity_rounds_to_three_decimals() {
    assert_eq!(round_similarity(0.300_49), 0.3);
    assert_eq!(round_similarity(0.2996), 0.3);
    assert_eq!(round_similarity(0.1234), 0.123);
    assert_eq!(round_similarity(-0.0004), 0.0);
    assert!(
        round_similarity(-0.0004).is_sign_positive(),
        "no negative zero"
    );
    assert_eq!(round_similarity(1.0), 1.0);
}

/// Throughput, measured rather than asserted in a debug build: run with
/// `cargo test --release -p tasqx-core embed_throughput -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check; run in --release"]
fn embed_throughput() {
    let t = std::time::Instant::now();
    model::model();
    let parse = t.elapsed();
    let chunk = "The release checklist lists every feature flag older than two releases; \
                 a flag is deleted within two releases of reaching one hundred percent, \
                 because forty stale flags made the pricing page untestable and nobody \
                 knew which combinations were live. Ship on Tuesdays unless CI is red.";
    let texts: Vec<String> = (0..1000).map(|i| format!("{chunk} #{i}")).collect();
    let t = std::time::Instant::now();
    let n = texts.iter().filter_map(|s| embed(s)).count();
    let took = t.elapsed();
    assert_eq!(n, 1000);
    eprintln!("parse {parse:?}; 1000 chunks of ~50 words in {took:?}");
}
