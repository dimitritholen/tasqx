//! Token-accounting domain methods for Engine
//! (DESIGN.md §10, backlog #11).
//!
//! `token_usage` rows are per-task child records like annotations, with one
//! deliberate difference: recording a measurement does NOT bump the task's
//! `rev`/`modified`. See [`Engine::token_add`].

use std::collections::{HashMap, HashSet};

use rusqlite::Row;

use super::*;
use crate::attribution::{consumed_sample_ids_by_task, recompute_measurement, WindowScan};
use crate::otlp::OtlpSample;
use crate::tokens::{
    require_confidence, require_source, CONFIDENCE_HIGH, CONFIDENCE_LOW, SOURCE_LOG_PARSE,
    SOURCE_OTEL, SOURCE_SELF_REPORT,
};

/// How long a buffered OTLP sample is kept before opportunistic pruning (#18).
/// The buffer is a short-lived staging area between telemetry arriving and a task
/// completing; a task is normally attributed within seconds of `task.done`, so
/// 30 days is generous headroom that still bounds the table for a daemon that
/// runs for months. Pruning runs inside every ingest, so no separate sweeper
/// thread is needed.
const OTLP_RETENTION_SECS: i64 = 30 * 24 * 60 * 60;

/// Upper bound on the `sample_ids` array [`Engine::token_attribute`] accepts.
/// The array lands verbatim in the `tokens.attributed` event payload and is
/// re-parsed on every pending-set build, so an unbounded one becomes a
/// permanent per-tick tax on the daemon. No real transcript window approaches
/// this (the store's biggest banked window held 41 samples); anything past it
/// is a caller error, not data.
const MAX_SAMPLE_IDS: usize = 4096;

/// An `opt_u64` whose value must also fit the INTEGER column it is stored in.
/// Without the bound, a count above `i64::MAX` would fail at the SQL binding
/// and surface as `internal` — a caller mistake reported as a tasqx bug.
pub(super) fn opt_token_count(p: &Value, key: &str) -> Result<Option<i64>, ApiError> {
    match opt_u64(p, key)? {
        None => Ok(None),
        Some(n) => Ok(Some(i64::try_from(n).map_err(|_| {
            ApiError::bad_request(format!(
                "`{key}` must fit a 64-bit signed integer, but {n} was given — send at most {}",
                i64::MAX
            ))
        })?)),
    }
}

/// One validated measurement, ready to insert. The row id and `created`
/// instant are minted at write time, so two callers cannot disagree about
/// either.
pub(super) struct NewTokenUsage {
    pub(super) tool: String,
    pub(super) source: String,
    pub(super) model: Option<String>,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_creation_tokens: i64,
    pub(super) confidence: String,
    /// Opaque bookkeeping the caller's write door attaches — currently only
    /// `token_add`'s `idempotency_key` (#221), JSON-encoded so the column
    /// stays free-form for whatever a future write door needs. `None` for
    /// every other writer. Deliberately not part of the canonical measurement
    /// object [`record_token_usage`] returns: the frozen shape (D56) is
    /// closed, and this is transport bookkeeping, not a fact about the spend.
    pub(super) extra: Option<String>,
}

/// Insert one measurement row inside `tx` and answer the canonical
/// measurement object — the one shape `task.get`, `store.export`, the
/// snapshot loader and the event payloads all speak.
pub(super) fn record_token_usage(
    tx: &Transaction,
    task_id: &str,
    usage: &NewTokenUsage,
) -> Result<Value, ApiError> {
    // Every write door enforces the closed vocabularies (storage.rs schema
    // comment); internal callers pass the `crate::tokens` constants, so this
    // only fires on a genuinely out-of-vocabulary value.
    crate::tokens::require_source(&usage.source)?;
    crate::tokens::require_confidence(&usage.confidence)?;
    let id = Uuid::now_v7().to_string();
    let created = now();
    tx.execute(
        "INSERT INTO token_usage (id, task_id, tool, source, model, input_tokens, \
         output_tokens, cache_read_tokens, cache_creation_tokens, confidence, created, extra) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            id,
            task_id,
            usage.tool,
            usage.source,
            usage.model,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
            usage.confidence,
            created,
            usage.extra,
        ],
    )?;
    Ok(json!({
        "id": id,
        "tool": usage.tool,
        "source": usage.source,
        "model": usage.model,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_creation_tokens": usage.cache_creation_tokens,
        "confidence": usage.confidence,
        "created": created,
    }))
}

/// True when this task's latest completion has already been attributed — the
/// async attribution dedupe record (the `already_reminded` precedent). A task is
/// attributed only when a `tokens.attributed` event exists *after* its most
/// recent `done` (rowid strictly greater), so a reopen + re-complete (which
/// appends a fresh `done` past the old marker, the log being append-only) is
/// re-attributed instead of being suppressed by the stale marker. Takes a
/// `&Connection` so it runs on the open `Transaction` (which derefs to
/// `Connection`), letting [`Engine::token_attribute`] re-check inside its own
/// write lock.
pub(super) fn has_attributed_event(conn: &Connection, task_id: &str) -> Result<bool, ApiError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events \
         WHERE entity_id = ?1 AND op = 'tokens.attributed' AND rowid > COALESCE( \
             (SELECT MAX(rowid) FROM events WHERE entity_id = ?1 AND op = 'done'), 0)",
        params![task_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Map a token_usage row into the canonical measurement object. `base` is the
/// column index the measurement starts at, so the grouped snapshot query
/// (which leads with `task_id`) and the per-task reader share one mapper.
pub(super) fn measurement_from_row(row: &Row, base: usize) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, String>(base)?,
        "tool": row.get::<_, String>(base + 1)?,
        "source": row.get::<_, String>(base + 2)?,
        "model": row.get::<_, Option<String>>(base + 3)?,
        "input_tokens": row.get::<_, i64>(base + 4)?,
        "output_tokens": row.get::<_, i64>(base + 5)?,
        "cache_read_tokens": row.get::<_, i64>(base + 6)?,
        "cache_creation_tokens": row.get::<_, i64>(base + 7)?,
        "confidence": row.get::<_, String>(base + 8)?,
        "created": row.get::<_, String>(base + 9)?,
    }))
}

/// The measurement column list every token_usage SELECT shares, kept in step
/// with [`measurement_from_row`] the same way `TASK_COLS` pairs with
/// `map_task_row`.
pub(super) const TOKEN_COLS: &str = "id, tool, source, model, input_tokens, output_tokens, \
     cache_read_tokens, cache_creation_tokens, confidence, created";

/// One stored log-parse measurement row, as [`Engine::token_recompute`] reads
/// it back for the before/after report and the unchanged check. Carries its
/// own `id` (#220) so the `channel_conflict` and `downgraded` write arms —
/// neither of which mints a fresh measurement row — can still name exactly
/// which row they touched in the audit event, the way the `recomputed` arm's
/// freshly-inserted row always could.
struct StoredLogParse {
    id: String,
    tool: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    confidence: String,
}

/// The previous state of every row a `channel_conflict` or `downgraded` write
/// is about to touch — the id, the confidence it carried, and its counts —
/// so the `tokens.attributed` marker that write appends is not the one place
/// in the store that forgets what it changed (#220). The `recomputed` arm
/// needs none of this: it already records the fresh row's own id and totals.
fn removed_measurements(rows: &[StoredLogParse]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            json!({
                "id": r.id,
                "tool": r.tool,
                "confidence": r.confidence,
                "input_tokens": r.input,
                "output_tokens": r.output,
                "cache_read_tokens": r.cache_read,
                "cache_creation_tokens": r.cache_creation,
            })
        })
        .collect()
}

/// Whether one `tokens.attributed` marker payload recorded a MEASUREMENT —
/// the bank instant [`Engine::token_recompute`]'s replay orders on — as
/// opposed to an empty (found=false) `{"samples": 0}` marker or a recompute
/// downgrade note. Every current banking shape carries the measurement row id
/// in `measurement`; older shapes at least carried non-zero `totals`, so both
/// spellings count.
fn marker_banked_measurement(payload: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    if v.get("measurement").is_some_and(|m| !m.is_null()) {
        return true;
    }
    v.get("totals").is_some_and(|t| {
        [
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
        ]
        .iter()
        .any(|k| t.get(k).and_then(Value::as_i64).unwrap_or(0) > 0)
    })
}

/// The `token_usage.extra` value one `token_add` idempotency key encodes to
/// (#221). JSON-wrapped, not the bare key, so the free-form `extra` column
/// stays extensible for whatever a later write door needs beside this one
/// without the two ever colliding on a bare string.
fn idempotency_extra(key: &str) -> String {
    json!({ "idempotency_key": key }).to_string()
}

/// The four-bucket object the recompute report speaks — the same four keys as
/// a measurement row, never a blended total (D48).
pub(super) fn buckets(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_tokens": cache_read,
        "cache_creation_tokens": cache_creation,
    })
}

/// Sum one task's raw measurements (the shape [`measurement_from_row`]
/// returns) into a [`crate::tokens::TokenTotals`] — the roll-up `task.list`'s
/// `tokens` field and `-tokens` sort key both need (#215), and the same shape
/// `report.summary` already sums across many tasks. Saturating, like every
/// other roll-up here.
pub(super) fn measurement_totals(measurements: &[Value]) -> crate::tokens::TokenTotals {
    let mut totals = crate::tokens::TokenTotals::default();
    for m in measurements {
        let get = |k: &str| m.get(k).and_then(Value::as_i64).unwrap_or(0) as u64;
        totals.input = totals.input.saturating_add(get("input_tokens"));
        totals.output = totals.output.saturating_add(get("output_tokens"));
        totals.cache_read = totals.cache_read.saturating_add(get("cache_read_tokens"));
        totals.cache_creation = totals
            .cache_creation
            .saturating_add(get("cache_creation_tokens"));
    }
    totals
}

/// Render a rolled-up [`crate::tokens::TokenTotals`] as the same four-bucket
/// object [`buckets`] renders for the recompute report — never a blended
/// total (D48). This is `task.list`'s `tokens` projection field.
pub(super) fn bucket_json(totals: &crate::tokens::TokenTotals) -> Value {
    buckets(
        totals.input as i64,
        totals.output as i64,
        totals.cache_read as i64,
        totals.cache_creation as i64,
    )
}

impl Engine {
    // ---- token.add -----------------------------------------------------------

    /// Record one AI token measurement against a task.
    ///
    /// Deliberately does NOT bump the task's `rev` or `modified` — the
    /// `reminder_fire` precedent. A measurement is a fact about tokens already
    /// spent, not an edit to the task, and the writers of the later phases run
    /// *asynchronously after* completion (daemon attribution, OTLP receiver):
    /// a rev bump from one of those would spuriously break a client's
    /// `expected_rev` on a task the client never touched.
    pub fn token_add(&self, p: &Value) -> Result<Value, ApiError> {
        // `ref` first, so an empty call is refused over the same field every
        // other task verb names first.
        let _ = ref_param(p)?;
        let source = req_str(p, "source")?;
        require_source(&source)?;
        let confidence = req_str(p, "confidence")?;
        require_confidence(&confidence)?;
        // #216 / D50: confidence describes verifiability, not preference — an
        // unverified claim does not become more checkable by being preferred,
        // and the trust hierarchy lives in `source`. The done-time self-report
        // path (`task.done`) never exposes `confidence` at all and forces
        // `medium` unconditionally; a self-report through this door must not
        // reach a trust tier that path can never claim. `medium` and `low`
        // stay open — a caller marking its own estimate as LESS trustworthy
        // than the default is not the inversion this refuses.
        if source == SOURCE_SELF_REPORT && confidence == CONFIDENCE_HIGH {
            return Err(ApiError::bad_request(
                "a self-report cannot claim confidence \"high\" — confidence describes \
                 verifiability, not preference, and an unverified claim is not made more \
                 checkable by being preferred (D50); send \"medium\" or \"low\""
                    .to_string(),
            ));
        }
        // #221: an optional caller-supplied idempotency key. Any MCP or HTTP
        // timeout the client retries otherwise doubles the ticket's recorded
        // cost, silently and with no way to remove either row — the
        // self-report twin of the OTLP replay this same finding named on the
        // telemetry side.
        let idempotency_key = opt_str_nonempty(p, "idempotency_key")?;
        let extra = idempotency_key.as_deref().map(idempotency_extra);
        let usage = NewTokenUsage {
            tool: req_str(p, "tool")?,
            source,
            model: opt_str_nonempty(p, "model")?,
            input_tokens: opt_token_count(p, "input_tokens")?.unwrap_or(0),
            output_tokens: opt_token_count(p, "output_tokens")?.unwrap_or(0),
            cache_read_tokens: opt_token_count(p, "cache_read_tokens")?.unwrap_or(0),
            cache_creation_tokens: opt_token_count(p, "cache_creation_tokens")?.unwrap_or(0),
            confidence,
            extra,
        };

        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;

        // #221: a replay of the SAME idempotency key on the SAME task returns
        // the measurement that key already banked, unchanged — ahead of the
        // #208 check below, deliberately: the write this call would have made
        // already happened, so nothing about the store's current state (an
        // attribution that ran since, say) changes what a repeat of a past
        // success should answer.
        if let Some(marker) = &usage.extra {
            if let Some(existing) = tx
                .query_row(
                    &format!(
                        "SELECT {TOKEN_COLS} FROM token_usage WHERE task_id = ?1 AND extra = ?2 \
                         LIMIT 1"
                    ),
                    params![task.id, marker],
                    |r| measurement_from_row(r, 0),
                )
                .optional()?
            {
                return Ok(json!({ "short_id": task.short_id, "measurement": existing }));
            }
        }

        // #208 / D50: "one task never mixes channels." The attribution engine
        // already refuses to lay a SECOND automated measurement over an
        // existing self-report (`token_attribute`'s `self_reported_meanwhile`
        // TOCTOU guard below) — but that protects only that direction. A
        // self-report arriving AFTER the daemon already banked this task's
        // spend automatically hit no such check, and silently doubled the
        // ledger under two `source`s. Gated on `has_attributed_event` (not
        // merely "does a non-self-report row exist"): an empty
        // `tokens.attributed` marker — unknown client, or a window that
        // turned out to hold nothing — records no row, so there is nothing
        // for a self-report to conflict with.
        if usage.source == SOURCE_SELF_REPORT && has_attributed_event(&tx, &task.id)? {
            if let Some((existing_source, input, output, cache_read, cache_creation)) = tx
                .query_row(
                    "SELECT source, input_tokens, output_tokens, cache_read_tokens, \
                     cache_creation_tokens FROM token_usage \
                     WHERE task_id = ?1 AND source != ?2 ORDER BY id LIMIT 1",
                    params![task.id, SOURCE_SELF_REPORT],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?
            {
                return Err(ApiError::conflict(format!(
                    "task already carries a {existing_source} measurement from the automated \
                     attribution pipeline ({input} in / {output} out / {cache_read} cacheR / \
                     {cache_creation} cacheW tokens) — a self-report here would double the \
                     ledger; use `tokens.recompute` or remove the existing measurement first"
                )));
            }
        }

        let measurement = record_token_usage(&tx, &task.id, &usage)?;
        insert_event(&tx, Entity::Task, &task.id, "token.add", &measurement)?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "measurement": measurement }))
    }

    // ---- token.remove (#210: the correction path token.add never had) --------

    /// Delete one measurement by id, echoing what is gone.
    ///
    /// The corrective half of `token.add`, absent until now: a self-report has
    /// no negative-count offset (`input_tokens`/`output_tokens`/… all refuse a
    /// value below zero), `tokens.recompute` explicitly never touches a
    /// self-report or OTLP row (D50) — only `source=log-parse` — and `token.add`
    /// only ever appends. So a mis-scaled count, a retry, or a double-report
    /// was permanent in every roll-up on every surface, forever.
    ///
    /// Bare `measurement_id`, no `ref`: a measurement id is already globally
    /// unique, the same shape `memory.remove` takes over `docs` instead of
    /// `token_usage`. Refuses `not_found` (exit 4) naming the id when it does
    /// not resolve, like every other by-id lookup this engine answers.
    ///
    /// The event carries the FULL removed measurement, not just its id —
    /// compare `memory.remove`, whose event holds neither and so can never be
    /// undone (`engine/undo.rs`'s own `memory.remove` entry). Even so,
    /// `token.remove` is not in `undo::UNDOABLE_OPS`: replaying this payload
    /// back in would mint a NEW row with a NEW id and a NEW `created` stamp —
    /// a fresh `token.add` in every way that matters, not the exact inverse
    /// undo promises everywhere else in that closed set. An operator who
    /// removed the wrong measurement re-adds the right one with `token.add`;
    /// this event's payload is what tells them what that was.
    pub fn token_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let measurement_id = req_str(p, "measurement_id")?;

        let tx = self.begin_mutation()?;
        let found: Option<(Value, String)> = tx
            .query_row(
                &format!("SELECT {TOKEN_COLS}, task_id FROM token_usage WHERE id = ?1"),
                params![measurement_id],
                |r| {
                    let measurement = measurement_from_row(r, 0)?;
                    let task_id: String = r.get(10)?;
                    Ok((measurement, task_id))
                },
            )
            .optional()?;
        let Some((measurement, task_id)) = found else {
            return Err(ApiError::not_found(
                format!("no token measurement with id {measurement_id}"),
                None,
            ));
        };
        tx.execute(
            "DELETE FROM token_usage WHERE id = ?1",
            params![measurement_id],
        )?;
        let short_id: i64 = tx.query_row(
            "SELECT short_id FROM tasks WHERE id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task_id,
            "token.remove",
            &json!({ "removed": measurement }),
        )?;
        tx.commit()?;

        Ok(json!({ "short_id": short_id, "removed": measurement }))
    }

    // ---- token.attribute (async attribution engine, #17) --------------------

    /// Idempotently record the tokens the async attribution engine reconstructed
    /// for one completed task, and mark the task attributed.
    ///
    /// Shaped like `scheduler::fire_one` and the reminder precedent: the dedupe
    /// record is an event (`tokens.attributed`), re-checked INSIDE this IMMEDIATE
    /// transaction so a restart, a racing tick, or a redelivery all converge on
    /// exactly one attribution per task. Like `reminder_fire` / `token_add` it
    /// does NOT bump the task's `rev`/`modified` — attribution runs
    /// asynchronously after completion and must never break a client's
    /// `expected_rev` on a task the client never touched.
    ///
    /// Exactly one event is written per call (the one-event-per-mutation
    /// invariant). A `token_usage` measurement row is inserted ONLY when real
    /// spend was found (total > 0); an unknown-client or empty-window task still
    /// gets the marker so it terminates and never re-enters the pending set. The
    /// heavy transcript parse happens in `crate::attribution`, off this lock and
    /// before this call.
    ///
    /// Returns `true` when this call performed the attribution, `false` when it
    /// was already attributed (the idempotent no-op path).
    pub fn token_attribute(&self, p: &Value) -> Result<bool, ApiError> {
        // `ref` first, so an empty call is refused over the same field every
        // other task verb names first.
        let _ = ref_param(p)?;
        let source = req_str(p, "source")?;
        require_source(&source)?;
        let confidence = req_str(p, "confidence")?;
        require_confidence(&confidence)?;
        let tool = req_str(p, "tool")?;
        // #213: the model every sample this measurement consumed agreed on,
        // when [`crate::attribution::attribute_one`] found one — the only
        // field that can ever turn four counts into money, and previously
        // discarded at this exact write site regardless of what the caller
        // computed.
        let model = opt_str_nonempty(p, "model")?;
        let samples = opt_u64(p, "samples")?.unwrap_or(0);
        // The identities of the samples this measurement consumed, when the
        // parser had any (Claude Code message ids). Persisted in the marker
        // payload so later ticks can refuse a consumed sample by id even after
        // a streamed re-emission moves its re-parsed timestamp across a window
        // edge (banked decisions are final; stamps are not).
        //
        // Read through the typed layer (D32), not `p.get(…).and_then(as_array)`.
        // The raw accessor maps a present-but-wrong-typed value onto the same
        // `None` as an absent one, so `sample_ids: "msg-1"` — or an array with a
        // number in it, which the old `filter_map(as_str)` dropped silently —
        // banked the measurement with no ids at all and answered ok. The whole
        // point of persisting them is refusing a re-emitted sample later; ids
        // that vanish on the way in take that refusal with them, and nothing
        // downstream can tell the difference between "the parser had none" and
        // "the caller sent them wrong".
        let sample_ids = opt_str_array(p, "sample_ids")?;
        if sample_ids.len() > MAX_SAMPLE_IDS {
            return Err(ApiError::bad_request(format!(
                "`sample_ids` holds {} entries — send at most {MAX_SAMPLE_IDS} \
                 (no real transcript window approaches that many samples)",
                sample_ids.len()
            )));
        }
        let input = opt_token_count(p, "input_tokens")?.unwrap_or(0);
        let output = opt_token_count(p, "output_tokens")?.unwrap_or(0);
        let cache_read = opt_token_count(p, "cache_read_tokens")?.unwrap_or(0);
        let cache_creation = opt_token_count(p, "cache_creation_tokens")?.unwrap_or(0);
        let total = input
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_creation);

        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;

        // Idempotency: a prior tick may already have attributed this task
        // (catch-up scans re-see everything). Re-check inside the write lock so
        // two racing daemons cannot both write a marker.
        if has_attributed_event(&tx, &task.id)? {
            return Ok(false);
        }

        // TOCTOU guard for "one task never mixes channels" (D50): the pending
        // set captured `self_reported` under an earlier lock, and the tick then
        // parsed the transcript UNLOCKED — a self-report landing via
        // `token.add` in that gap would otherwise be joined by a log-parse row
        // for the identical spend. Re-check inside THIS transaction, exactly
        // like `has_attributed_event` above: the self-report is authoritative,
        // so suppress the usage-row insert but still write the terminating
        // marker — the task IS measured, by the caller.
        let self_reported_meanwhile = source == SOURCE_LOG_PARSE && {
            let n: i64 = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM token_usage \
                 WHERE task_id = ?1 AND source = ?2 LIMIT 1)",
                params![task.id, SOURCE_SELF_REPORT],
                |r| r.get(0),
            )?;
            n > 0
        };

        // A measurement row only when there is real spend to record; otherwise
        // just the marker. Either way, exactly one event.
        let payload = if total > 0 && !self_reported_meanwhile {
            let usage = NewTokenUsage {
                tool: tool.clone(),
                source: source.clone(),
                model: model.clone(),
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
                confidence: confidence.clone(),
                extra: None,
            };
            let measurement = record_token_usage(&tx, &task.id, &usage)?;
            let mut payload = json!({
                "source": source,
                "tool": tool,
                "confidence": confidence,
                "samples": samples,
                "totals": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_tokens": cache_read,
                    "cache_creation_tokens": cache_creation,
                },
                "measurement": measurement.get("id").cloned().unwrap_or(Value::Null),
            });
            // Only when banking a measurement, and only when there are ids to
            // record: an empty array would say "consumed nothing" as loudly as
            // omission, and every payload byte lives in the event log forever.
            if !sample_ids.is_empty() {
                payload["sample_ids"] = json!(sample_ids);
            }
            payload
        } else {
            json!({ "samples": 0 })
        };
        insert_event(&tx, Entity::Task, &task.id, "tokens.attributed", &payload)?;
        tx.commit()?;

        Ok(true)
    }

    // ---- otlp buffer (local OTLP receiver, #18) ------------------------------

    /// Buffer raw per-request OTLP samples received over the opt-in telemetry
    /// channel (#18). These are NOT attributed to any task yet — they are matched
    /// to a task later by `session_id` + time window — so unlike every other
    /// mutation here there is deliberately **no** task event and **no** `rev`
    /// bump: nothing about a task changed, only the staging buffer grew. The
    /// IMMEDIATE transaction is still taken to serialize the write and to fold the
    /// opportunistic retention prune into the same commit; there is no entity to
    /// read-back because a raw append correlates to no task.
    ///
    /// Returns the number of rows actually inserted — which, since #221, can
    /// be fewer than `samples.len()` when the batch replays a record already
    /// buffered.
    pub fn otlp_ingest(&self, samples: &[OtlpSample]) -> Result<usize, ApiError> {
        if samples.is_empty() {
            return Ok(0);
        }
        let created = now();
        let tx = self.begin_mutation()?;
        let mut inserted = 0usize;
        for s in samples {
            // Client-supplied counts can exceed i64 in theory; clamp rather than
            // fail the whole export on one absurd row (the export is best-effort).
            let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
            let input = clamp(s.sample.input_tokens);
            let output = clamp(s.sample.output_tokens);
            let cache_read = clamp(s.sample.cache_read_tokens);
            let cache_creation = clamp(s.sample.cache_creation_tokens);
            // #221: the row's id is derived from the record's own natural
            // identity (timestamp + session + tool + the four counts) rather
            // than minted fresh, so a replayed export — which the module's
            // own header notes is a KNOWN condition ("/v1/metrics answers 200
            // so an exporter configured for both does not retry-storm") —
            // computes the SAME id and `INSERT OR IGNORE` makes the replay
            // free instead of a second row. Two samples that genuinely differ
            // in even one count are different spends and get different ids;
            // model is deliberately excluded from the key, matching the
            // suggested natural key, since it never varies for one physical
            // request the way a retried export's counts do not either.
            let natural_key = format!(
                "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
                s.session_id.as_deref().unwrap_or(""),
                s.tool,
                s.sample.ts,
                input,
                output,
                cache_read,
                cache_creation,
            );
            let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, natural_key.as_bytes()).to_string();
            let rows = tx.execute(
                "INSERT OR IGNORE INTO otlp_samples (id, session_id, tool, ts, model, \
                 input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, created) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    id,
                    s.session_id,
                    s.tool,
                    s.sample.ts,
                    s.sample.model,
                    input,
                    output,
                    cache_read,
                    cache_creation,
                    created,
                ],
            )?;
            inserted += rows;
        }
        // Opportunistic retention prune, in the same transaction. An unresolvable
        // cutoff (clock underflow) yields "" and deletes nothing — never a panic.
        let cutoff = crate::clock::now()
            .checked_sub(jiff::SignedDuration::from_secs(OTLP_RETENTION_SECS))
            .map(|t| t.to_string())
            .unwrap_or_default();
        tx.execute(
            "DELETE FROM otlp_samples WHERE created < ?1",
            params![cutoff],
        )?;
        tx.commit()?;
        Ok(inserted)
    }

    /// Buffered OTLP samples for one session, oldest first, with the tool that
    /// emitted the first of them. Read during the attribution pending-set build
    /// (a cheap indexed query, safe under the short engine lock) so the compute
    /// step can prefer telemetry over log-parsing without any file I/O. An empty
    /// or missing session id never matches, so it returns nothing.
    pub(crate) fn otlp_samples_for_session(
        &self,
        session_id: &str,
    ) -> Result<(Vec<crate::tokens::UsageSample>, Option<String>), ApiError> {
        if session_id.is_empty() {
            return Ok((Vec::new(), None));
        }
        let mut stmt = self.conn.prepare(
            "SELECT tool, ts, model, input_tokens, output_tokens, cache_read_tokens, \
             cache_creation_tokens FROM otlp_samples WHERE session_id = ?1 ORDER BY ts, id",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                crate::tokens::UsageSample {
                    id: None,
                    ts: r.get::<_, String>(1)?,
                    model: r.get::<_, Option<String>>(2)?,
                    input_tokens: r.get::<_, i64>(3)?.max(0) as u64,
                    output_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                    cache_read_tokens: r.get::<_, i64>(5)?.max(0) as u64,
                    cache_creation_tokens: r.get::<_, i64>(6)?.max(0) as u64,
                },
            ))
        })?;
        let mut samples = Vec::new();
        let mut tool = None;
        for r in rows {
            let (t, s) = r?;
            if tool.is_none() {
                tool = Some(t);
            }
            samples.push(s);
        }
        Ok((samples, tool))
    }

    /// `otlp.status` (#222): the only surface that answers "is telemetry
    /// reaching tasqx" — before this, the answer required opening the SQLite
    /// file directly. `received` is every buffered row still inside the
    /// `OTLP_RETENTION_SECS` window; `attributed` is how many of those turned
    /// into a `source=otel` measurement; `orphaned` is the rest — rows nothing
    /// will ever attribute (no session id, a contested window, or a task that
    /// never completed) and that the retention prune is the only thing that
    /// will eventually clear. `last_seen` is the newest sample's timestamp, or
    /// `None` on an empty buffer, so a misconfigured exporter that stopped
    /// sending is visible as a stale clock rather than silence indistinguishable
    /// from "never configured".
    ///
    /// `orphaned` is `received.saturating_sub(attributed)` rather than a second
    /// query some other way to reconcile the two counts: `otlp_samples` and
    /// `token_usage` are never joined (a sample carries no `task_id`, see the
    /// schema comment in storage.rs), so there is no query that could count
    /// "this specific sample was attributed" — only how many of each exist.
    pub fn otlp_status(&self) -> Result<Value, ApiError> {
        let received: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM otlp_samples", [], |r| r.get(0))?;
        let attributed: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM token_usage WHERE source = ?1",
            params![SOURCE_OTEL],
            |r| r.get(0),
        )?;
        let last_seen: Option<String> =
            self.conn
                .query_row("SELECT MAX(ts) FROM otlp_samples", [], |r| r.get(0))?;
        Ok(json!({
            "received": received,
            "attributed": attributed,
            "orphaned": received.saturating_sub(attributed),
            "last_seen": last_seen,
        }))
    }

    // ---- tokens.recompute (D50 Decision 3: one-shot history repair) ----------

    /// Re-run log-parse attribution over the stored windows under the D50
    /// refusal rule, repairing history the pre-refusal ticks double-counted.
    ///
    /// Scope: EVERY task holding at least one `source=log-parse` measurement
    /// row — not only overlap-contested ones — processed in the order live
    /// ticks BANKED measurements: ascending rowid of the earliest
    /// `tokens.attributed` marker that RECORDED A MEASUREMENT for the task
    /// (empty found=false markers do not count — a task's first marker can be
    /// an empty one from before a reopen, and ordering on it would replay a
    /// reopened thief before the task that actually banked first), REBUILDING
    /// the identity-claim set as it goes. The full ordered pass is
    /// load-bearing: markers banked before sample ids were persisted carry no
    /// claims, so a moved-stamp theft against a pre-upgrade bank is precisely
    /// NOT window-contested — replaying history in bank order closes that
    /// upgrade window, and backfills `sample_ids` on surviving measurements'
    /// (new) markers as a side effect. A task with only empty markers keeps
    /// its earliest marker of any kind as the fallback; a row that never had
    /// a marker at all (hand-recorded via `token.add`) sorts last, by task
    /// id, so it can never displace a banked claim.
    ///
    /// Per task, one of four actions, reported as
    /// `{ "task": short_id, "action", "before": {four buckets},
    ///    "after": {four buckets}|null }`:
    /// - `"recomputed"` — the transcript is readable and the re-derived
    ///   measurement differs FOR A CONTEST REASON (or only its shape differs:
    ///   duplicate-row collapse, sample-id backfill, confidence re-earn): the
    ///   task's log-parse rows are deleted and the recomputed row inserted
    ///   (none when the recomputed total is 0, which is how a fully-contested
    ///   window ends with `after` all zeros). ONLY CONTEST OR A TASK'S OWN
    ///   REPEATED CLAIM REMOVES TOKENS: a shrink or deletion happens on this
    ///   arm exclusively, and only when at least one of the task's samples
    ///   was contested away by ANOTHER task, or the task itself holds more
    ///   than one stored row (#81 — reopen + re-complete banked the same
    ///   window's spend twice on this one task; `WindowScan` folds every
    ///   `start`/`done` cycle into a single union window, so the recomputed
    ///   row is already the correctly-deduped total and always replaces the
    ///   duplicates, never leaves a second copy standing).
    /// - `"channel_conflict"` — the task ALSO carries a self-report row
    ///   (pre-TOCTOU-fix history; Decision 1 says one task never mixes
    ///   channels): its log-parse rows are removed outright and `after` is
    ///   `null` — the task's real spend is the self-report, which this verb
    ///   never restates.
    /// - `"downgraded"` — the transcript is missing or unreadable, OR it is
    ///   readable but re-reads to different uncontested totals (evidence
    ///   drift: a moved stamp, a truncated file, a re-emission past the
    ///   window edge — mixed drift included, so a row never silently
    ///   shrinks): the counts are kept (`after` == `before`) with
    ///   `confidence` stripped to `low`, never deleted blind.
    /// - `"unchanged"` — readable, identical, already claimed: no writes.
    ///
    /// Writes go per task in ONE IMMEDIATE transaction through this module's
    /// own doors (`Engine::recompute_replace` / a confidence UPDATE) —
    /// deliberately NOT [`Engine::token_attribute`], whose
    /// `has_attributed_event` guard no-ops on every already-attributed task,
    /// which is every task this migration exists to repair. Old markers stay
    /// in the append-only log as provenance.
    ///
    /// `dry_run` (default **true** — the safe direction for the one verb in
    /// the API built to delete measurement rows) computes the identical report
    /// and writes nothing. The result also carries
    /// `{ "totals": { "before": n, "after": n } }`, the blended grand total of
    /// the scoped log-parse spend — a migration delta, not a report surface.
    pub fn token_recompute(&self, p: &Value) -> Result<Value, ApiError> {
        let dry_run = match p.get("dry_run") {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(ApiError::bad_request(format!(
                    "`dry_run` must be a boolean, but {other} was given — omit it for the safe \
                     default (report the delta, write nothing) or send false to apply"
                )))
            }
        };

        // ONE snapshot for the whole read phase (the `store_export` rule):
        // every statement below — including the reads inside
        // `WindowScan::build` and `consumed_sample_ids_by_task`, which run on
        // this same connection — otherwise takes its own, and the apply phase
        // deletes ALL of a task's log-parse rows, so a row committed between
        // two reads could be deleted without ever having been scanned.
        // DEFERRED, never IMMEDIATE: this is a read, and it must not hold the
        // write lock for the length of a full transcript scan.
        //
        // The window this closes is the torn READ. A row that lands after
        // `read_tx.commit()` and before this run's per-task write is still
        // deleted unscanned — the price of per-task IMMEDIATE transactions,
        // which rule out holding one snapshot across the writes. The daemon
        // serializes its own attribution ticks against dispatched calls, so
        // the residue is a direct-store writer racing a recompute on purpose.
        let read_tx = self.conn.unchecked_transaction()?;

        // Every task's stored log-parse rows, oldest first per task — by `id`,
        // the UUIDv7 that is creation order, never by the `created` text (D142).
        let mut stored: HashMap<String, Vec<StoredLogParse>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT task_id, id, tool, input_tokens, output_tokens, cache_read_tokens, \
                 cache_creation_tokens, confidence FROM token_usage \
                 WHERE source = ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![SOURCE_LOG_PARSE], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    StoredLogParse {
                        id: r.get(1)?,
                        tool: r.get(2)?,
                        input: r.get(3)?,
                        output: r.get(4)?,
                        cache_read: r.get(5)?,
                        cache_creation: r.get(6)?,
                        confidence: r.get(7)?,
                    },
                ))
            })?;
            for r in rows {
                let (task_id, row) = r?;
                stored.entry(task_id).or_default().push(row);
            }
        }

        // The order live ticks banked these measurements in: the earliest
        // marker that RECORDED A MEASUREMENT per task, so the claim rebuild
        // replays history instead of inventing a new one. NOT the earliest
        // marker of any kind — an empty (found=false) marker from before a
        // reopen predates the bank without being one, and ordering on it
        // would replay a reopened thief before the live owner. Tasks with
        // only empty markers fall back to their earliest marker, then task
        // id. Decided in Rust, not by a payload LIKE: a substring guard over
        // JSON is unsound.
        let mut first_bank: HashMap<String, i64> = HashMap::new();
        let mut first_marker: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT entity_id, rowid, payload FROM events \
                 WHERE op = 'tokens.attributed' ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?;
            for r in rows {
                let (task_id, rowid, payload) = r?;
                first_marker.entry(task_id.clone()).or_insert(rowid);
                if payload.as_deref().is_some_and(marker_banked_measurement) {
                    first_bank.entry(task_id).or_insert(rowid);
                }
            }
        }
        let bank_order = |task: &String| {
            (
                first_bank
                    .get(task)
                    .or_else(|| first_marker.get(task))
                    .copied()
                    .unwrap_or(i64::MAX),
                task.clone(),
            )
        };
        let mut order: Vec<String> = stored.keys().cloned().collect();
        order.sort_by_key(bank_order);

        let mut short_ids: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = self.conn.prepare("SELECT id, short_id FROM tasks")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for r in rows {
                let (id, short_id) = r?;
                if stored.contains_key(&id) {
                    short_ids.insert(id, short_id);
                }
            }
        }

        // Tasks that also self-reported: Decision 1's channel-conflict set.
        let self_reported: HashSet<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT task_id FROM token_usage WHERE source = ?1")?;
            let rows = stmt.query_map(params![SOURCE_SELF_REPORT], |r| r.get::<_, String>(0))?;
            let mut set = HashSet::new();
            for r in rows {
                set.insert(r?);
            }
            set
        };

        let scan = WindowScan::build(self)?;
        let banked = consumed_sample_ids_by_task(self)?;
        // The rebuilt claim set starts from every task OUTSIDE the recompute
        // scope (their banks are not re-derived here, so their claims stand);
        // in-scope tasks re-earn theirs in bank order below.
        let mut claims: HashSet<String> = banked
            .iter()
            .filter(|(task_id, _)| !stored.contains_key(*task_id))
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect();

        // The read phase is over; the per-task writes below open their own
        // IMMEDIATE transactions on this connection, so the snapshot must be
        // gone first (a commit of zero writes, i.e. a clean release).
        read_tx.commit()?;

        let mut report = Vec::new();
        let (mut total_before, mut total_after) = (0i64, 0i64);
        for task_id in &order {
            let rows = &stored[task_id];
            // A scoped row without a task row is impossible (FK), but a
            // migration must tolerate a strange store rather than die halfway.
            let Some(short_id) = short_ids.get(task_id).copied() else {
                continue;
            };
            let c = classify_task(&scan, task_id, &claims, &banked, &self_reported, rows);
            let action = c.action.as_str();
            // The ids this task holds onto contest every later task in this
            // pass, exactly as its live-tick bank would have; WHICH ids those
            // are is the classification's answer (re-earned sample ids, or the
            // banked claims a conflict/downgrade keeps consumed).
            claims.extend(c.claim_ids);
            if !dry_run {
                match c.action {
                    RecomputeAction::ChannelConflict => {
                        self.recompute_replace(
                            task_id,
                            "channel_conflict",
                            None,
                            0,
                            &[],
                            &removed_measurements(rows),
                        )?;
                    }
                    RecomputeAction::Recomputed {
                        usage,
                        samples,
                        sample_ids,
                    } => {
                        self.recompute_replace(
                            task_id,
                            "recomputed",
                            usage,
                            samples,
                            &sample_ids,
                            &[],
                        )?;
                    }
                    RecomputeAction::Unchanged => {}
                    RecomputeAction::Downgraded => {
                        self.recompute_downgrade(task_id, &removed_measurements(rows))?;
                    }
                }
            }

            total_before = total_before.saturating_add(c.before_total);
            total_after = total_after.saturating_add(c.after_total);
            report.push(json!({
                "task": short_id,
                "action": action,
                "before": c.before,
                "after": c.after,
            }));
        }

        Ok(json!({
            "dry_run": dry_run,
            "tasks": report,
            "totals": { "before": total_before, "after": total_after },
        }))
    }

    /// The recompute's row-replacing write door: inside ONE IMMEDIATE
    /// transaction, delete the task's log-parse rows, insert the recomputed
    /// survivor (when there is one), and append the recompute's
    /// `tokens.attributed` marker — the mutation's one event, carrying the
    /// recomputed `sample_ids` so pre-upgrade claims are backfilled. The old
    /// marker stays in the append-only log as provenance. Deliberately NOT
    /// [`Engine::token_attribute`]: that door's idempotency guard no-ops on
    /// every already-attributed task, and its one-event shape belongs to the
    /// live tick.
    ///
    /// `removed` (#220) is the pre-image of the rows this call is about to
    /// delete with no replacement — the `channel_conflict` arm's own case,
    /// since a fresh `usage` already answers the same question on the
    /// `recomputed` arm. Empty there, so its payload is unchanged.
    fn recompute_replace(
        &self,
        task_id: &str,
        action: &str,
        usage: Option<NewTokenUsage>,
        samples: usize,
        sample_ids: &[String],
        removed: &[Value],
    ) -> Result<(), ApiError> {
        let tx = self.begin_mutation()?;
        tx.execute(
            "DELETE FROM token_usage WHERE task_id = ?1 AND source = ?2",
            params![task_id, SOURCE_LOG_PARSE],
        )?;
        let mut payload = json!({ "recompute": true, "action": action, "samples": samples });
        if let Some(usage) = &usage {
            let measurement = record_token_usage(&tx, task_id, usage)?;
            payload["source"] = json!(usage.source);
            payload["tool"] = json!(usage.tool);
            payload["confidence"] = json!(usage.confidence);
            payload["totals"] = json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_read_tokens": usage.cache_read_tokens,
                "cache_creation_tokens": usage.cache_creation_tokens,
            });
            payload["measurement"] = measurement.get("id").cloned().unwrap_or(Value::Null);
        }
        if !sample_ids.is_empty() {
            payload["sample_ids"] = json!(sample_ids);
        }
        if !removed.is_empty() {
            payload["measurements"] = json!(removed);
        }
        insert_event(&tx, Entity::Task, task_id, "tokens.attributed", &payload)?;
        tx.commit()
    }

    /// The recompute's keep-but-distrust write door: strip the task's
    /// log-parse rows to `confidence=low` (counts untouched) and append the
    /// downgrade marker, in one transaction. No `sample_ids` here — an
    /// unreadable transcript is exactly the case where they cannot be
    /// re-derived; the task's OLD markers keep whatever claims they held.
    ///
    /// `measurements` (#220) is [`removed_measurements`] over the rows this
    /// call strips — the id and the confidence EACH ONE carried before this
    /// write, so the event names what changed instead of just that something
    /// did. Nothing is deleted here (the rows survive, only their confidence
    /// moves), but `--apply` is still a one-way door: `undo` refuses
    /// `tokens.attributed`, so an operator who disagrees with a downgrade
    /// needs this payload's ids to act on it at all — with them in hand,
    /// `token.remove` retracts the row outright, or a fresh `token.add`
    /// re-banks it at the confidence they believe is right.
    fn recompute_downgrade(&self, task_id: &str, measurements: &[Value]) -> Result<(), ApiError> {
        let tx = self.begin_mutation()?;
        tx.execute(
            "UPDATE token_usage SET confidence = ?1 WHERE task_id = ?2 AND source = ?3",
            params![CONFIDENCE_LOW, task_id, SOURCE_LOG_PARSE],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            task_id,
            "tokens.attributed",
            &json!({
                "recompute": true,
                "action": "downgraded",
                "confidence": CONFIDENCE_LOW,
                "measurements": measurements,
            }),
        )?;
        tx.commit()
    }

    /// Measurements of a task as canonical objects, oldest first (the
    /// `annotations_of` shape, for `task.get`).
    ///
    /// Oldest first *by `id`*, which is UUIDv7 and therefore creation order.
    /// Not by `created`: that TEXT column carries a variable-length fractional
    /// second and sorts an older row above a newer one under BINARY collation
    /// (D142, and `storage::event_id_floor` for the whole story).
    pub(super) fn tokens_of(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TOKEN_COLS} FROM token_usage WHERE task_id = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![task_id], |r| measurement_from_row(r, 0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }
}

/// The recompute's verdict for one task: the decision, separated from its
/// side effects. `token_recompute` used to express this as ~140 lines of
/// nested if/else with tuple-assignments, interleaving the classification
/// with the claim extension and the writes — the hardest code in the module
/// to verify by reading. The caller applies the write `action` names and
/// extends the in-pass claim set with `claim_ids`; nothing here touches the
/// store.
struct Classified {
    action: RecomputeAction,
    /// The sample ids this task keeps consumed for the rest of the pass:
    /// re-earned ids on the recompute arm, the banked claims on the
    /// conflict/downgrade arms (a dissolved source must not silently release
    /// the samples it consumed) — exactly what the old inline arms extended
    /// the claim set with.
    claim_ids: Vec<String>,
    before: Value,
    before_total: i64,
    after: Value,
    after_total: i64,
}

/// Which write `token_recompute` owes one task — the four actions its report
/// names, as data instead of control flow.
enum RecomputeAction {
    /// Decision 1: the task also self-reported; its log-parse rows move aside.
    ChannelConflict,
    /// Honestly re-derived — a contest, or a clean per-row match: replace the
    /// rows with `usage` (None when the recomputed total is 0).
    Recomputed {
        usage: Option<NewTokenUsage>,
        samples: usize,
        sample_ids: Vec<String>,
    },
    /// Readable, identical, already claimed (or already all-low): no writes.
    Unchanged,
    /// Evidence missing or drifted uncontested: keep the counts, strip the
    /// confidence to low, never delete blind.
    Downgraded,
}

impl RecomputeAction {
    fn as_str(&self) -> &'static str {
        match self {
            RecomputeAction::ChannelConflict => "channel_conflict",
            RecomputeAction::Recomputed { .. } => "recomputed",
            RecomputeAction::Unchanged => "unchanged",
            RecomputeAction::Downgraded => "downgraded",
        }
    }
}

/// The four-way decision for one task, pure over its inputs. `usage` is built
/// on the recompute arm whether or not this is a dry run, which is what makes
/// the dry-run report and the applying run's report identical by construction
/// rather than by parallel arms agreeing.
fn classify_task(
    scan: &WindowScan,
    task_id: &str,
    claims: &HashSet<String>,
    banked: &HashMap<String, Vec<String>>,
    self_reported: &HashSet<String>,
    rows: &[StoredLogParse],
) -> Classified {
    let sums = rows.iter().fold((0i64, 0i64, 0i64, 0i64), |acc, row| {
        (
            acc.0.saturating_add(row.input),
            acc.1.saturating_add(row.output),
            acc.2.saturating_add(row.cache_read),
            acc.3.saturating_add(row.cache_creation),
        )
    });
    let before = buckets(sums.0, sums.1, sums.2, sums.3);
    let before_total = sums
        .0
        .saturating_add(sums.1)
        .saturating_add(sums.2)
        .saturating_add(sums.3);

    let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    let banked_ids = || banked.get(task_id).cloned().unwrap_or_default();

    if self_reported.contains(task_id) {
        // The rows move aside for the self-report, but the samples the bank
        // CONSUMED stay consumed — the banked ids enter the in-pass claim set
        // exactly like the downgrade arm's, so a later task cannot re-earn
        // the same spend and reopen the double-count across channels.
        // Conservative on purpose: cross-channel claims still contest.
        return Classified {
            action: RecomputeAction::ChannelConflict,
            claim_ids: banked_ids(),
            before,
            before_total,
            after: Value::Null,
            after_total: 0,
        };
    }

    // The sample ids this task's OWN markers banked (empty for a pre-upgrade
    // bank): they scope the out-of-window contest check inside
    // `recompute_measurement` to the task's actual evidence.
    let own_claims: HashSet<String> = banked_ids().into_iter().collect();
    if let Some(rc) = recompute_measurement(scan, task_id, claims, &own_claims).filter(|rc| {
        // ONLY CONTEST REMOVES TOKENS (D50 Decision 3 as amended): with
        // nothing contested, a re-read that no longer matches the banked
        // evidence is drift — a moved stamp, a truncated file, a re-emission
        // past the window edge — and falls through to the keep-and-downgrade
        // arm below, exactly like an unreadable transcript. Matching is per
        // stored row, so a reopen duplicate whose every row equals the full
        // re-derived measurement still collapses, and the equal-totals
        // sample-id backfill still rewrites.
        //
        // (#81) `rows.len() > 1` also always passes the filter: a second
        // stored row can only exist because reopen + re-complete produced a
        // second claim event for this SAME task, and `WindowScan` folds every
        // cycle's `start`/`done` into one union window, so `rc` is already
        // the correctly-deduped single measurement — each sample id counted
        // once, however many times this task individually banked it. Per-row
        // equality cannot express that (the union total generally differs
        // from any one row, e.g. the first cycle's row holds only its own
        // slice), so it would misfile the collapse as "drift" and downgrade
        // instead of fixing it — leaving the double-bank exactly as filed.
        // This is not a second CONTEST exception: no other task's claim is
        // being overridden, only this task's own repeated claims are being
        // folded into the one they always should have been.
        rc.contested > 0
            || rows.len() > 1
            || rows.iter().all(|row| {
                row.input == clamp(rc.totals.input)
                    && row.output == clamp(rc.totals.output)
                    && row.cache_read == clamp(rc.totals.cache_read)
                    && row.cache_creation == clamp(rc.totals.cache_creation)
            })
    }) {
        let found = rc.totals.total() > 0;
        let tool = rc.tool.clone().unwrap_or_else(|| rows[0].tool.clone());
        let unchanged = found
            && rows.len() == 1
            && rows[0].input == clamp(rc.totals.input)
            && rows[0].output == clamp(rc.totals.output)
            && rows[0].cache_read == clamp(rc.totals.cache_read)
            && rows[0].cache_creation == clamp(rc.totals.cache_creation)
            && rows[0].confidence == rc.confidence
            && rows[0].tool == tool
            && rc
                .sample_ids
                .iter()
                .all(|id| banked.get(task_id).is_some_and(|ids| ids.contains(id)));
        let after = buckets(
            clamp(rc.totals.input),
            clamp(rc.totals.output),
            clamp(rc.totals.cache_read),
            clamp(rc.totals.cache_creation),
        );
        let after_total = clamp(rc.totals.total());
        let action = if unchanged {
            RecomputeAction::Unchanged
        } else {
            let usage = found.then(|| NewTokenUsage {
                tool,
                source: SOURCE_LOG_PARSE.to_string(),
                model: None,
                input_tokens: clamp(rc.totals.input),
                output_tokens: clamp(rc.totals.output),
                cache_read_tokens: clamp(rc.totals.cache_read),
                cache_creation_tokens: clamp(rc.totals.cache_creation),
                confidence: rc.confidence.to_string(),
                extra: None,
            });
            RecomputeAction::Recomputed {
                usage,
                samples: rc.samples,
                sample_ids: rc.sample_ids.clone(),
            }
        };
        return Classified {
            action,
            claim_ids: rc.sample_ids,
            before,
            before_total,
            after,
            after_total,
        };
    }

    // Missing/unreadable transcript (or no explicit one), or uncontested
    // evidence drift: the banked counts cannot be honestly re-derived, so
    // they are kept — and so are the task's banked claims, which is what
    // keeps a dissolved source from silently releasing the samples it
    // consumed.
    let action = if rows.iter().all(|row| row.confidence == CONFIDENCE_LOW) {
        RecomputeAction::Unchanged
    } else {
        RecomputeAction::Downgraded
    };
    Classified {
        action,
        claim_ids: banked_ids(),
        after: before.clone(),
        after_total: before_total,
        before,
        before_total,
    }
}

#[cfg(test)]
mod tests {
    /// `token_recompute` must take all of its database reads from ONE
    /// snapshot. Each statement otherwise reads its own (WAL), and the apply
    /// phase's `recompute_replace` deletes ALL of a task's log-parse rows —
    /// so a row committed between two of the scan's reads (a daemon
    /// attribution tick, a `token.add`) could be deleted without ever having
    /// been scanned. Structural, for the same reason as `store_export`'s twin
    /// guard in transfer.rs: the interleaving point is inside SQLite, and
    /// rusqlite's `hooks` feature — the only way to drive a write from
    /// between two of our reads — is not compiled in.
    ///
    /// The snapshot must also CLOSE before the write loop: the per-task
    /// writes open their own IMMEDIATE transactions on the same connection,
    /// and a still-open read transaction there is a nested-transaction error
    /// at the first task.
    #[test]
    fn token_recompute_snapshots_its_read_phase() {
        let source = include_str!("tokens.rs");
        // Assembled, never written out literally: `dispatch`'s accepted-key
        // guard splits this same source at every `fn NAME(`, so a marker
        // spelled in full would register HERE as a second definition of the
        // handler and overwrite the real one.
        let marker = format!("pub fn {}(", "token_recompute");
        let marker = marker.as_str();
        let start = source.find(marker).expect("token_recompute exists");
        let rest = &source[start..];
        let end = rest[marker.len()..]
            .find("\n    fn ")
            .map(|offset| marker.len() + offset)
            .unwrap_or(rest.len());
        // Comments out: prose in the body may name the constructs this test
        // scans for without being them.
        let body: String = rest[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.as_str();

        let guard = body
            .find("unchecked_transaction()")
            .expect("token_recompute must open a snapshot for its read phase");
        let first_read = body
            .find("self.conn.prepare")
            .expect("token_recompute reads the stored rows");
        assert!(
            guard < first_read,
            "the snapshot pins at the first read, so the transaction must be opened before it"
        );
        assert!(
            !body.contains("let _ = self.conn.unchecked_transaction"),
            "a `_` binding drops the transaction on the spot, making the guard a no-op"
        );
        let closed = body
            .find(".commit()")
            .expect("the read snapshot must close before the apply phase");
        let write_loop = body
            .find("for task_id in &order")
            .expect("the apply phase iterates the banked order");
        assert!(
            closed < write_loop,
            "the per-task IMMEDIATE writes need the read transaction gone first"
        );
    }

    // ---- otlp.status (#222) ---------------------------------------------

    #[test]
    fn otlp_status_reports_received_attributed_orphaned_and_last_seen() {
        use crate::engine::Engine;
        use crate::otlp::OtlpSample;
        use crate::tokens::{UsageSample, SOURCE_OTEL};
        use serde_json::json;

        let e = Engine::open_in_memory().unwrap();

        // Two buffered OTLP samples, one attributable, one destined to be an
        // orphan (its session never completes a task).
        e.otlp_ingest(&[
            OtlpSample {
                tool: "claude-code".to_string(),
                session_id: Some("sess-a".to_string()),
                sample: UsageSample {
                    id: None,
                    ts: "2026-07-24T10:00:00Z".to_string(),
                    model: Some("claude-opus-4-8".to_string()),
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
            OtlpSample {
                tool: "claude-code".to_string(),
                session_id: Some("sess-orphan".to_string()),
                sample: UsageSample {
                    id: None,
                    ts: "2026-07-24T11:00:00Z".to_string(),
                    model: None,
                    input_tokens: 20,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
        ])
        .expect("ingest");

        // Empty buffer answers zeroes and a null clock, not an error.
        let empty = Engine::open_in_memory().unwrap().otlp_status().unwrap();
        assert_eq!(empty["received"], 0);
        assert_eq!(empty["attributed"], 0);
        assert_eq!(empty["orphaned"], 0);
        assert!(empty["last_seen"].is_null());

        let before = e.otlp_status().expect("otlp.status");
        assert_eq!(before["received"], 2);
        assert_eq!(before["attributed"], 0);
        assert_eq!(before["orphaned"], 2);
        assert_eq!(before["last_seen"], "2026-07-24T11:00:00Z");

        // Recording one `source=otel` measurement (what attribution does when
        // it banks a claim) moves exactly one sample from orphaned to
        // attributed, without changing how many were received.
        e.token_add(&json!({
            "ref": e.task_add(&json!({"title": "t"})).unwrap()["short_id"],
            "tool": "claude-code",
            "source": SOURCE_OTEL,
            "input_tokens": 100,
            "output_tokens": 50,
            "confidence": "high",
        }))
        .expect("token.add");

        let after = e.otlp_status().expect("otlp.status");
        assert_eq!(after["received"], 2);
        assert_eq!(after["attributed"], 1);
        assert_eq!(after["orphaned"], 1);
    }
}
