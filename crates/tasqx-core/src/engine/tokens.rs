//! Token-accounting domain methods for Engine
//! (docs/research/token-accounting.md, backlog #11).
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
    require_confidence, require_source, CONFIDENCE_LOW, SOURCE_LOG_PARSE, SOURCE_SELF_REPORT,
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
         output_tokens, cache_read_tokens, cache_creation_tokens, confidence, created) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
/// it back for the before/after report and the unchanged check.
struct StoredLogParse {
    tool: String,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    confidence: String,
}

/// The four-bucket object the recompute report speaks — the same four keys as
/// a measurement row, never a blended total (D48).
fn buckets(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_tokens": cache_read,
        "cache_creation_tokens": cache_creation,
    })
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
        let usage = NewTokenUsage {
            tool: req_str(p, "tool")?,
            source,
            model: opt_str_nonempty(p, "model")?,
            input_tokens: opt_token_count(p, "input_tokens")?.unwrap_or(0),
            output_tokens: opt_token_count(p, "output_tokens")?.unwrap_or(0),
            cache_read_tokens: opt_token_count(p, "cache_read_tokens")?.unwrap_or(0),
            cache_creation_tokens: opt_token_count(p, "cache_creation_tokens")?.unwrap_or(0),
            confidence,
        };

        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        let measurement = record_token_usage(&tx, &task.id, &usage)?;
        insert_event(&tx, Entity::Task, &task.id, "token.add", &measurement)?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "measurement": measurement }))
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
        let samples = opt_u64(p, "samples")?.unwrap_or(0);
        // The identities of the samples this measurement consumed, when the
        // parser had any (Claude Code message ids). Persisted in the marker
        // payload so later ticks can refuse a consumed sample by id even after
        // a streamed re-emission moves its re-parsed timestamp across a window
        // edge (banked decisions are final; stamps are not).
        let sample_ids: Vec<String> = match p.get("sample_ids").and_then(Value::as_array) {
            Some(a) if a.len() > MAX_SAMPLE_IDS => {
                return Err(ApiError::bad_request(format!(
                    "`sample_ids` holds {} entries — send at most {MAX_SAMPLE_IDS} \
                     (no real transcript window approaches that many samples)",
                    a.len()
                )));
            }
            Some(a) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            None => Vec::new(),
        };
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
                model: None,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
                confidence: confidence.clone(),
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
    /// Returns the number of rows inserted.
    pub fn otlp_ingest(&self, samples: &[OtlpSample]) -> Result<usize, ApiError> {
        if samples.is_empty() {
            return Ok(0);
        }
        let created = now();
        let tx = self.begin_mutation()?;
        for s in samples {
            // Client-supplied counts can exceed i64 in theory; clamp rather than
            // fail the whole export on one absurd row (the export is best-effort).
            let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO otlp_samples (id, session_id, tool, ts, model, input_tokens, \
                 output_tokens, cache_read_tokens, cache_creation_tokens, created) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    Uuid::now_v7().to_string(),
                    s.session_id,
                    s.tool,
                    s.sample.ts,
                    s.sample.model,
                    clamp(s.sample.input_tokens),
                    clamp(s.sample.output_tokens),
                    clamp(s.sample.cache_read_tokens),
                    clamp(s.sample.cache_creation_tokens),
                    created,
                ],
            )?;
        }
        // Opportunistic retention prune, in the same transaction. An unresolvable
        // cutoff (clock underflow) yields "" and deletes nothing — never a panic.
        let cutoff = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(OTLP_RETENTION_SECS))
            .map(|t| t.to_string())
            .unwrap_or_default();
        tx.execute(
            "DELETE FROM otlp_samples WHERE created < ?1",
            params![cutoff],
        )?;
        tx.commit()?;
        Ok(samples.len())
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

    // ---- tokens.recompute (D50 Decision 3: one-shot history repair) ----------

    /// Re-run log-parse attribution over the stored windows under the D50
    /// refusal rule, repairing history the pre-refusal ticks double-counted.
    ///
    /// Scope: EVERY task holding at least one `source=log-parse` measurement
    /// row — not only overlap-contested ones — processed in ascending order of
    /// its original (first) `tokens.attributed` marker rowid, REBUILDING the
    /// identity-claim set as it goes. The full ordered pass is load-bearing:
    /// markers banked before sample ids were persisted carry no claims, so a
    /// moved-stamp theft against a pre-upgrade bank is precisely NOT
    /// window-contested — replaying history in bank order closes that upgrade
    /// window, and backfills `sample_ids` on surviving measurements' (new)
    /// markers as a side effect. A row that never had a marker (hand-recorded
    /// via `token.add`) sorts last, by task id, so it can never displace a
    /// banked claim.
    ///
    /// Per task, one of four actions, reported as
    /// `{ "task": short_id, "action", "before": {four buckets},
    ///    "after": {four buckets}|null }`:
    /// - `"recomputed"` — the transcript is readable and the re-derived
    ///   measurement differs: the task's log-parse rows are deleted and the
    ///   recomputed row inserted (none when the recomputed total is 0, which
    ///   is how a fully-contested window ends with `after` all zeros).
    /// - `"channel_conflict"` — the task ALSO carries a self-report row
    ///   (pre-TOCTOU-fix history; Decision 1 says one task never mixes
    ///   channels): its log-parse rows are removed outright and `after` is
    ///   `null` — the task's real spend is the self-report, which this verb
    ///   never restates.
    /// - `"downgraded"` — the transcript is missing or unreadable, so the
    ///   counts cannot be re-derived: they are kept (`after` == `before`) with
    ///   `confidence` stripped to `low`, never deleted blind.
    /// - `"unchanged"` — readable, identical, already claimed: no writes.
    ///
    /// Writes go per task in ONE IMMEDIATE transaction through this module's
    /// own doors ([`Engine::recompute_replace`] / a confidence UPDATE) —
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

        // Every task's stored log-parse rows, oldest first per task.
        let mut stored: HashMap<String, Vec<StoredLogParse>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT task_id, tool, input_tokens, output_tokens, cache_read_tokens, \
                 cache_creation_tokens, confidence FROM token_usage \
                 WHERE source = ?1 ORDER BY created, id",
            )?;
            let rows = stmt.query_map(params![SOURCE_LOG_PARSE], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    StoredLogParse {
                        tool: r.get(1)?,
                        input: r.get(2)?,
                        output: r.get(3)?,
                        cache_read: r.get(4)?,
                        cache_creation: r.get(5)?,
                        confidence: r.get(6)?,
                    },
                ))
            })?;
            for r in rows {
                let (task_id, row) = r?;
                stored.entry(task_id).or_default().push(row);
            }
        }

        // The order live ticks banked these measurements in: first marker
        // rowid per task, so the claim rebuild replays history instead of
        // inventing a new one.
        let mut first_marker: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT entity_id, MIN(rowid) FROM events \
                 WHERE op = 'tokens.attributed' GROUP BY entity_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for r in rows {
                let (task_id, rowid) = r?;
                first_marker.insert(task_id, rowid);
            }
        }
        let mut order: Vec<String> = stored.keys().cloned().collect();
        order.sort_by(|a, b| {
            (first_marker.get(a).copied().unwrap_or(i64::MAX), a)
                .cmp(&(first_marker.get(b).copied().unwrap_or(i64::MAX), b))
        });

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

        let mut report = Vec::new();
        let (mut total_before, mut total_after) = (0i64, 0i64);
        for task_id in &order {
            let rows = &stored[task_id];
            // A scoped row without a task row is impossible (FK), but a
            // migration must tolerate a strange store rather than die halfway.
            let Some(short_id) = short_ids.get(task_id).copied() else {
                continue;
            };
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

            let (action, after, after_total);
            if self_reported.contains(task_id) {
                if !dry_run {
                    self.recompute_replace(task_id, "channel_conflict", None, 0, &[])?;
                }
                (action, after, after_total) = ("channel_conflict", Value::Null, 0);
            } else if let Some(rc) = recompute_measurement(&scan, task_id, &claims) {
                let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
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
                if unchanged {
                    action = "unchanged";
                } else {
                    if !dry_run {
                        let usage = found.then(|| NewTokenUsage {
                            tool,
                            source: SOURCE_LOG_PARSE.to_string(),
                            model: None,
                            input_tokens: clamp(rc.totals.input),
                            output_tokens: clamp(rc.totals.output),
                            cache_read_tokens: clamp(rc.totals.cache_read),
                            cache_creation_tokens: clamp(rc.totals.cache_creation),
                            confidence: rc.confidence.to_string(),
                        });
                        self.recompute_replace(
                            task_id,
                            "recomputed",
                            usage,
                            rc.samples,
                            &rc.sample_ids,
                        )?;
                    }
                    action = "recomputed";
                }
                after = buckets(
                    clamp(rc.totals.input),
                    clamp(rc.totals.output),
                    clamp(rc.totals.cache_read),
                    clamp(rc.totals.cache_creation),
                );
                after_total = clamp(rc.totals.total());
                // This task's re-earned claims contest every later task in
                // this pass, exactly as its live-tick bank would have.
                claims.extend(rc.sample_ids.iter().cloned());
            } else {
                // Missing/unreadable transcript (or no explicit one): the
                // counts cannot be re-derived, so they are kept — and so are
                // the task's banked claims, which is what keeps a dissolved
                // source from silently releasing the samples it consumed.
                if let Some(ids) = banked.get(task_id) {
                    claims.extend(ids.iter().cloned());
                }
                if rows.iter().all(|row| row.confidence == CONFIDENCE_LOW) {
                    action = "unchanged";
                } else {
                    if !dry_run {
                        self.recompute_downgrade(task_id)?;
                    }
                    action = "downgraded";
                }
                (after, after_total) = (before.clone(), before_total);
            }

            total_before = total_before.saturating_add(before_total);
            total_after = total_after.saturating_add(after_total);
            report.push(json!({
                "task": short_id,
                "action": action,
                "before": before,
                "after": after,
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
    fn recompute_replace(
        &self,
        task_id: &str,
        action: &str,
        usage: Option<NewTokenUsage>,
        samples: usize,
        sample_ids: &[String],
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
        insert_event(&tx, Entity::Task, task_id, "tokens.attributed", &payload)?;
        tx.commit()
    }

    /// The recompute's keep-but-distrust write door: strip the task's
    /// log-parse rows to `confidence=low` (counts untouched) and append the
    /// downgrade marker, in one transaction. No `sample_ids` here — an
    /// unreadable transcript is exactly the case where they cannot be
    /// re-derived; the task's OLD markers keep whatever claims they held.
    fn recompute_downgrade(&self, task_id: &str) -> Result<(), ApiError> {
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
            &json!({ "recompute": true, "action": "downgraded" }),
        )?;
        tx.commit()
    }

    /// Measurements of a task as canonical objects, oldest first (the
    /// `annotations_of` shape, for `task.get`).
    pub(super) fn tokens_of(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TOKEN_COLS} FROM token_usage WHERE task_id = ?1 ORDER BY created, id"
        ))?;
        let rows = stmt.query_map(params![task_id], |r| measurement_from_row(r, 0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }
}
