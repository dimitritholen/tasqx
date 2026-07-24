//! Token-accounting domain methods for Engine
//! (docs/research/token-accounting.md, backlog #11).
//!
//! `token_usage` rows are per-task child records like annotations, with one
//! deliberate difference: recording a measurement does NOT bump the task's
//! `rev`/`modified`. See [`Engine::token_add`].

use rusqlite::Row;

use super::*;
use crate::tokens::{require_confidence, require_source};

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
