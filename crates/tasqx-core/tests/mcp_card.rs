//! Tests for `view: "card"` on `tasqx_get_task` and `tasqx_brief_task` (D146).
//!
//! The argument is transport-only, like `include_json` beside it: the store is
//! never asked anything different, and what changes is the spelling of the one
//! rendered block. So what is asserted here is mostly about the ENVELOPE — how
//! many blocks, which one carries what, what survives the byte budget — and
//! `tests/markdown_card.rs` remains the golden test for the card itself.
//!
//! The one thing a golden string cannot carry across this seam is `now`: it is
//! stamped inside `present`, so a card built in a test can only be compared to
//! the one the server drew under `TimeFormat::Iso`, where no instant is written
//! relative to anything. Every test that compares bytes pins the format for
//! that reason; the ones that do not check the geometry instead.

use serde_json::{json, Value};
use tasqx_core::markdown::{self, Borders, CardOpts, DetailOpts, TimeFormat, CARD_WIDTH};
use tasqx_core::{dispatch, Engine, McpServer, Scope};
use unicode_width::UnicodeWidthStr;

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

/// A server whose instants are absolute, so a card it draws is a pure function
/// of the result and can be compared byte for byte (see the module note).
fn iso_server(engine: &Engine) -> McpServer<'_> {
    McpServer::new(engine, Scope::Read).with_time_format(TimeFormat::Iso)
}

fn call(server: &McpServer, id: i64, name: &str, arguments: Value) -> Value {
    server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .expect("tools/call is a request and yields a response")
}

fn blocks(result: &Value) -> Vec<String> {
    result["result"]["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .map(|b| b["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn is_error(result: &Value) -> bool {
    result["result"]["isError"].as_bool().unwrap_or(false)
}

fn add(e: &Engine, title: &str, extra: Value) -> i64 {
    let mut params = json!({ "title": title });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            params[k] = v.clone();
        }
    }
    dispatch(e, "task.add", &params).expect("task.add")["short_id"]
        .as_i64()
        .expect("add returns short_id")
}

fn annotate(e: &Engine, r: i64, body: &str) {
    dispatch(e, "annotation.add", &json!({ "ref": r, "body": body })).expect("annotation.add");
}

/// One task with enough on it that the card has rows to draw.
fn one_task(e: &Engine) -> i64 {
    let a = add(
        e,
        "Paste a task in front of a person",
        json!({ "priority": "H", "tags": ["render"] }),
    );
    annotate(
        e,
        a,
        "The card is a document: fixed geometry, no escape codes.",
    );
    a
}

/// The card the server should have drawn, built from the same dispatch it
/// dispatched. `annotations_limit` is named so the comparison does not depend
/// on the transport's page size, and two annotations fit inside any page it
/// might choose.
fn expected_card(e: &Engine, r: i64) -> String {
    let result =
        dispatch(e, "task.get", &json!({ "ref": r, "annotations_limit": 20 })).expect("task.get");
    markdown::task_card(
        &result,
        &CardOpts {
            detail: DetailOpts {
                time: TimeFormat::Iso,
                now: "2026-09-15T18:00:00Z".parse().expect("a fixed instant"),
            },
            borders: Borders::Unicode,
        },
    )
}

/// The fence's own promise: the lines between the markers are the card, and
/// there is exactly one newline after the closing marker. Returns the card.
#[track_caller]
fn unfence(text: &str) -> String {
    let body = text
        .strip_prefix("```text\n")
        .unwrap_or_else(|| panic!("a card block opens with a text fence:\n{text}"));
    let body = body
        .strip_suffix("\n```\n")
        .unwrap_or_else(|| panic!("a card block closes its fence and stops:\n{text}"));
    assert!(
        !body.ends_with('\n'),
        "a blank line inside the fence:\n{text:?}"
    );
    format!("{body}\n")
}

/// Every line of a card is a box side, and a box is only a box while they are
/// all the same width. Asserted here as well as in `markdown_card.rs` because
/// this is the path where a card could be reflowed on its way out.
#[track_caller]
fn assert_is_a_box(card: &str) {
    assert!(card.starts_with('┌'), "not a card:\n{card}");
    for line in card.lines() {
        assert_eq!(
            UnicodeWidthStr::width(line),
            CARD_WIDTH,
            "line is not {CARD_WIDTH} cells:\n{line}"
        );
    }
}

// ---- tasqx_get_task ---------------------------------------------------------

/// The card arrives fenced, and the machine block beside it is the one the
/// default call sends: `view` chooses a spelling for the rendered half and
/// nothing else (D146).
#[test]
fn a_card_is_the_fenced_box_and_the_json_block_is_untouched() {
    let e = engine();
    let r = one_task(&e);
    let server = iso_server(&e);

    let carded = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": r, "view": "card" }),
    );
    assert!(!is_error(&carded));
    let carded = blocks(&carded);
    assert_eq!(carded.len(), 2, "the JSON block is still on by default");

    assert!(carded[0].starts_with("```text\n┌"), "{}", carded[0]);
    assert!(carded[0].ends_with("┘\n```\n"), "{:?}", carded[0]);
    let card = unfence(&carded[0]);
    assert_is_a_box(&card);
    assert!(card.contains(&format!("Task #{r}")), "{card}");
    assert!(card.contains("Paste a task in front of a person"), "{card}");
    assert_eq!(card, expected_card(&e, r), "not the card markdown.rs draws");

    let default = blocks(&call(&server, 2, "tasqx_get_task", json!({ "ref": r })));
    assert_eq!(
        carded[1], default[1],
        "the machine block is the same result either way"
    );
    assert!(
        default[0].starts_with("## #"),
        "and the default view is still the table:\n{}",
        default[0]
    );
}

/// `include_json: false` under a card is the one block the caller asked for,
/// and no notice: the omission was chosen, not forced by the budget.
#[test]
fn a_card_alone_is_one_block_when_the_json_is_declined() {
    let e = engine();
    let r = one_task(&e);
    let server = iso_server(&e);

    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": r, "view": "card", "include_json": false }),
    );
    assert!(!is_error(&out));
    let blocks = blocks(&out);
    assert_eq!(blocks.len(), 1, "one block: {blocks:?}");
    assert_is_a_box(&unfence(&blocks[0]));
    assert!(
        !blocks[0].contains("response budget"),
        "a chosen omission is not an over-budget one:\n{}",
        blocks[0]
    );
}

/// The default is not merely documented as `markdown`, it IS the call that
/// names nothing — byte for byte, both blocks, both tools.
#[test]
fn naming_markdown_is_the_same_response_as_naming_nothing() {
    let e = engine();
    let r = one_task(&e);
    let server = iso_server(&e);

    for tool in ["tasqx_get_task", "tasqx_brief_task"] {
        let absent = call(&server, 1, tool, json!({ "ref": r }));
        let named = call(&server, 2, tool, json!({ "ref": r, "view": "markdown" }));
        assert_eq!(
            absent["result"], named["result"],
            "{tool}: `view: \"markdown\"` must be the default's own bytes"
        );
        // And a null is absent, the D32 reading `client` and `actor` already
        // get: a client that serializes unset optionals must not lose the
        // default by saying nothing in JSON.
        let null = call(&server, 3, tool, json!({ "ref": r, "view": null }));
        assert_eq!(
            absent["result"], null["result"],
            "{tool}: a JSON null is not a value, it is an omission"
        );
    }
}

/// An unreadable `view` is refused, and the refusal names both values it
/// accepts. Defaulting it would answer a request for a document with the view
/// meant for a model, and the agent would find out when a person read it.
#[test]
fn an_unknown_view_is_refused_and_names_what_it_takes() {
    let e = engine();
    let r = one_task(&e);
    let server = iso_server(&e);

    for tool in ["tasqx_get_task", "tasqx_brief_task"] {
        for bad in [json!("table"), json!("Card"), json!(true)] {
            let out = call(&server, 1, tool, json!({ "ref": r, "view": bad.clone() }));
            assert!(is_error(&out), "{tool} accepted `view: {bad}`: {out}");
            let text = blocks(&out).remove(0);
            assert!(
                text.contains("bad_request"),
                "{tool}: a misread argument is the caller's to fix: {text}"
            );
            for value in ["markdown", "card"] {
                assert!(
                    text.contains(value),
                    "{tool}: the refusal must name `{value}`: {text}"
                );
            }
        }
    }
}

/// The refusal happens before dispatch, so `view` never reaches the params
/// gate — which would refuse it as an unknown key and say something about
/// `task.get` instead, on a valid value.
#[test]
fn view_never_reaches_the_params_gate() {
    let e = engine();
    let r = one_task(&e);
    let server = iso_server(&e);

    for args in [
        json!({ "ref": r, "view": "card" }),
        json!({ "ref": r, "view": "markdown", "include_json": false }),
        json!({ "ref": r, "view": "card", "annotations_limit": 1 }),
    ] {
        let out = call(&server, 1, "tasqx_get_task", args.clone());
        assert!(
            !is_error(&out),
            "`view` reached the engine for {args}: {out}"
        );
    }
}

// ---- tasqx_brief_task -------------------------------------------------------

/// A brief's card is the card plus the brief's own tail — the tail OUTSIDE the
/// fence, because a code block around the prerequisites' conclusions is a code
/// block around sentences.
#[test]
fn a_brief_card_fences_the_task_half_and_keeps_the_tail_as_markdown() {
    let e = engine();
    let upstream = add(&e, "Freeze the envelope", json!({}));
    annotate(&e, upstream, "froze it: the floor derives from PARAMS");
    dispatch(&e, "task.done", &json!({ "ref": upstream })).expect("task.done");
    let downstream = add(&e, "Write the conformance suite", json!({}));
    dispatch(
        &e,
        "dependency.add",
        &json!({ "ref": downstream, "depends_on": upstream }),
    )
    .expect("dependency.add");
    let server = iso_server(&e);

    let out = call(
        &server,
        1,
        "tasqx_brief_task",
        json!({ "ref": downstream, "view": "card" }),
    );
    assert!(!is_error(&out));
    let view = blocks(&out).remove(0);
    assert!(view.starts_with("```text\n┌"), "{view}");

    let (card, tail) = view.split_once("\n```\n").expect("a closed fence");
    assert_is_a_box(&format!("{}\n", card.trim_start_matches("```text\n")));
    assert!(
        tail.contains("### Depends on"),
        "the tail survives the card:\n{tail}"
    );
    assert!(
        tail.contains("froze it: the floor derives from PARAMS"),
        "including what the prerequisite concluded:\n{tail}"
    );
    assert!(
        !tail.contains("```"),
        "and it is markdown, not more fence:\n{tail}"
    );

    let brief = dispatch(&e, "task.brief", &json!({ "ref": downstream })).expect("task.brief");
    assert!(
        view.starts_with(&format!(
            "```text\n{}",
            markdown::task_card(
                &brief,
                &CardOpts {
                    detail: DetailOpts {
                        time: TimeFormat::Iso,
                        now: "2026-09-15T18:00:00Z".parse().expect("a fixed instant"),
                    },
                    borders: Borders::Unicode,
                }
            )
            .trim_end()
        )),
        "the card half is `task_card` of the brief, unaltered:\n{view}"
    );
}

// ---- the budget (D66) -------------------------------------------------------

/// D66's steps run over whichever view was asked for: the duplicate JSON block
/// is spent before any history, and what survives is still a card.
#[test]
fn an_oversized_card_response_drops_the_json_before_the_history() {
    let e = engine();
    let r = add(&e, "eleven very long notes", json!({}));
    // ~6 KB each, the shape of the field report D66 was written for: a row
    // count cannot bound this, so the budget has to measure bytes.
    let body = "detail ".repeat(900);
    for i in 0..11 {
        annotate(&e, r, &format!("## Note {i}\n\n{body}\n"));
    }
    let server = iso_server(&e);

    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": r, "view": "card" }),
    );
    assert!(!is_error(&out));
    let blocks = blocks(&out);
    assert_eq!(
        blocks.len(),
        1,
        "the JSON block is the first thing the budget spends"
    );

    // The notice belongs to the envelope, not to the card: it is appended
    // after the closing fence, where it cannot widen a line of the box.
    let (card, notice) = blocks[0].split_once("\n```\n").expect("a closed fence");
    assert_is_a_box(&format!("{}\n", card.trim_start_matches("```text\n")));
    assert!(
        notice.contains("Machine-readable JSON omitted"),
        "a response one block short must say so:\n{notice}"
    );
    let bytes: usize = blocks.iter().map(String::len).sum();
    assert!(
        bytes < 24_576,
        "the response is {bytes} bytes, over the budget this path exists to keep"
    );
}
