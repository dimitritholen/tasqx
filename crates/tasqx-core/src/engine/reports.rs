//! Reports domain methods for Engine.

use super::*;

impl Engine {
    // ---- report.summary ------------------------------------------------------

    /// `report.summary` — counts and totals per group. Params: `group_by` (one
    /// of [`SUMMARY_GROUP_BY`], default the first), `filter`, `metrics` (a
    /// subset of [`SUMMARY_METRICS`], default `count`), `all`.
    ///
    /// Cancelled tasks are excluded — D24's [`Status::counts_in_reports`]
    /// partition, which is why `done` still counts and `cancelled` does not —
    /// unless `all` is set OR the `filter` names a status itself, in which case
    /// the caller's explicit ask wins over the default. An unknown metric is refused
    /// rather than dropped: a table quietly missing a column still looks like a
    /// valid table.
    pub fn report_summary(&self, p: &Value) -> Result<Value, ApiError> {
        // D35: `unwrap_or_else` fires only on a genuinely ABSENT value now, so
        // `group_by: ""` reaches the vocabulary check below instead of silently
        // becoming the default axis — the closed-set rule of D34, which
        // `group_by: "bogus"` already got and `""` did not.
        let group_by = opt_str(p, "group_by")?.unwrap_or_else(|| SUMMARY_GROUP_BY[0].to_string());
        if !SUMMARY_GROUP_BY.contains(&group_by.as_str()) {
            return Err(ApiError::bad_request(format!(
                "group_by must be {} (got {group_by:?})",
                SUMMARY_GROUP_BY.join("|")
            )));
        }
        // Validated against the same constant the MCP schema renders its `enum`
        // from. It used to `filter_map` unknown names away and answer `ok`, so
        // `metrics:["overdeu"]` produced a table with the column missing — the
        // `fields` and sort-key drop again, on the surface that had already
        // published its valid set and simply did not enforce it.
        let metrics: Vec<String> = match p.get("metrics") {
            None => vec![SUMMARY_METRICS[0].to_string()],
            Some(Value::Array(a)) => {
                let mut v = Vec::with_capacity(a.len());
                for m in a {
                    let name = m.as_str().filter(|s| SUMMARY_METRICS.contains(s));
                    let Some(name) = name else {
                        return Err(ApiError::bad_request(format!(
                            "unknown metric {m} (valid metrics: {})",
                            SUMMARY_METRICS.join(", ")
                        )));
                    };
                    v.push(name.to_string());
                }
                v
            }
            Some(_) => {
                return Err(ApiError::bad_request(
                    "`metrics` must be an array of metric names",
                ))
            }
        };

        // Kept as a string as well as a parsed filter: the result echoes it,
        // because a report that names no scope is a total that can be read
        // against the wrong period (D69). The filter DSL carries
        // `completed.after:`/`completed.before:`, so "what did this week cost"
        // is one call — and the answer used to come back with `generated` (the
        // time of the call, easy to misread as the boundary) and nothing else.
        let filter_str = opt_str(p, "filter")?.unwrap_or_default();
        // THE operation's clock: filter binding, every row's wait/schedule
        // release, the overdue comparison and `generated` all resolve against
        // this one instant — it used to be read three separate times here.
        let now_ts = Timestamp::now();
        let filter = Filter::parse(&filter_str, now_ts).map_err(ApiError::bad_request)?;
        validate_filter_projects(self.conn(), &filter)?;

        // D97: `since`/`until` window WHEN THE SPEND HAPPENED — a task tracked
        // time, or logged a token measurement — which is a different axis from
        // `filter`'s `completed.after:`/`completed.before:` entirely. `filter`
        // still decides which tasks the report is ABOUT; `since`/`until` bound
        // which slice of each selected task's history counts toward
        // `tracked_total` and the four token buckets.
        //
        // Audit #190/#224: the only date-scoped report was `completed.*`,
        // which sums a task's WHOLE lifetime tracked/token totals onto
        // whichever month it happened to close in — a task worked in July,
        // closed in September, banked all of July's hours as September's, and
        // a measurement written today attributed to a task closed weeks ago
        // was invisible to "what did we spend this week". Both are the same
        // mistake: attributing a rollup by an unrelated date field instead of
        // by the instant the work or the measurement actually landed.
        let since = opt_when(p, "since", now_ts)?
            .map(|when| {
                parse_ts(&when).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "`since` resolved to an unreadable instant {when:?}"
                    ))
                })
            })
            .transpose()?;
        let until = opt_when(p, "until", now_ts)?
            .map(|when| {
                parse_ts(&when).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "`until` resolved to an unreadable instant {when:?}"
                    ))
                })
            })
            .transpose()?;
        if let (Some(s), Some(u)) = (since, until) {
            if u <= s {
                return Err(ApiError::bad_request(format!(
                    "`until` ({u}) must be after `since` ({s})"
                )));
            }
        }
        let windowed = since.is_some() || until.is_some();
        // Reconstructed only when asked for: every other caller pays nothing
        // for a query this report never makes. See `task_tracked_intervals`
        // for why the event log, not `tasks.tracked_seconds`, is the source.
        let intervals = if windowed {
            self.task_tracked_intervals(now_ts)?
        } else {
            HashMap::new()
        };

        // D24: a report is an *aggregation*, so abandoned work must not inflate
        // any total. tasqx has no hard delete (DESIGN.md §7, "No hidden bulk
        // delete") — cancelling is how you get rid of a task — so without this
        // every throwaway task counted forever. `done` deliberately still
        // counts: completed work is real work and carries nearly all the
        // tracked time.
        //
        // Resolution order (D24): `all` wins; otherwise a caller who already
        // named a status is taken literally, so `status:cancelled` returns
        // cancelled tasks rather than a baffling empty table; otherwise the
        // default applies. The rule lives here, in core, so the CLI, the HTML
        // report and MCP agents all inherit one answer.
        let all = opt_bool(p, "all")?.unwrap_or(false);
        let apply_default = !all && !filter.constrains_status();

        // Accumulator per group key (insertion via BTreeMap => sorted output).
        use std::collections::BTreeMap;
        struct Agg {
            count: i64,
            est_secs: i64,
            tracked_secs: i64,
            overdue: i64,
            // The four token buckets stay separate all the way through (research
            // rule #5: cache tokens cost a fraction, so a blended total would
            // lie). Since D50 they stay separate past emit too: the blended
            // `tokens_total` is no longer a metric at all.
            tokens_in: i64,
            tokens_out: i64,
            tokens_cache_read: i64,
            tokens_cache_creation: i64,
            // D50's trust hierarchy lives in `source`/`confidence` per
            // measurement (#217). The four buckets above blend every
            // measurement's counts together with no way back to which row
            // contributed what, so the group's WORST confidence is tracked
            // alongside them — the one fact a blended total must not drop.
            tokens_confidence: Option<String>,
        }
        let mut groups: BTreeMap<String, Agg> = BTreeMap::new();
        // #234 item 12: D24 excludes cancelled tasks from the default report
        // for the reason its own decision gives — counting abandoned work
        // would inflate every total forever — but that ruling is about task
        // COUNTS, not about spend already incurred, and the default view
        // dropped a cancelled task's token measurements with nothing saying
        // so. This does not change what the default totals ARE (that is a
        // design question for whoever owns D24); it only stops the omission
        // from being silent.
        let mut tokens_excluded_cancelled_tasks: i64 = 0;

        for snapshot in
            self.load_task_snapshots_for(super::task::SnapshotParts::REPORT_SUMMARY, now_ts)?
        {
            let t = snapshot.task;
            if apply_default && !t.status.counts_in_reports() {
                let spent = snapshot.tokens.iter().any(|m| {
                    let n = |key: &str| m.get(key).and_then(Value::as_i64).unwrap_or(0);
                    n("input_tokens") != 0
                        || n("output_tokens") != 0
                        || n("cache_read_tokens") != 0
                        || n("cache_creation_tokens") != 0
                });
                if spent {
                    tokens_excluded_cancelled_tasks += 1;
                }
                continue;
            }
            let ctx = MatchCtx {
                status: t.status,
                priority: t.priority,
                project: t.project.as_deref(),
                tags: &snapshot.tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked: snapshot.blocked,
            };
            if !filter.matches(&ctx) {
                continue;
            }
            let key = match group_by.as_str() {
                "project" => t.project.clone().unwrap_or_else(|| "(none)".to_string()),
                // D28: the group *key* is a read surface, so it goes through the
                // one choke point (`Task::status_text`) that prefers the stored
                // text over the in-memory placeholder. `t.status.as_str()` filed
                // an unrecognized status under `pending` — the placeholder D24's
                // scope check deliberately keeps counting, but which no surface
                // may print as fact — while `task.list` and `store.export` named
                // the same row `Done`. Only the label changes here: `ctx.status`
                // above still carries the placeholder, because that is what keeps
                // the anomalous row inside the default `@working` view.
                //
                // Arbitrary text in this slot is already the norm — `project`
                // feeds user input through it — and both renderers sanitise it
                // (render.rs `san`, html.rs `esc`).
                "status" => t.status_text().to_string(),
                "priority" => t
                    .priority
                    .map(|x| x.as_str().to_string())
                    .unwrap_or_else(|| "(none)".to_string()),
                _ => unreachable!(),
            };
            let agg = groups.entry(key).or_insert(Agg {
                count: 0,
                est_secs: 0,
                tracked_secs: 0,
                overdue: 0,
                tokens_in: 0,
                tokens_out: 0,
                tokens_cache_read: 0,
                tokens_cache_creation: 0,
                tokens_confidence: None,
            });
            agg.count += 1;
            // Saturating: a single estimate is bounded by `duration_secs`, but a
            // roll-up sums arbitrarily many rows. A clamped total is wrong-but-
            // visible; a wrapped one is negative nonsense and a panic in debug.
            if let Some(e) = t.estimate.as_deref().and_then(duration_secs) {
                agg.est_secs = agg.est_secs.saturating_add(e);
            }
            // D97: with a window, `tracked_secs` is the sum of this task's
            // interval OVERLAP with `[since, until)`, not its lifetime total —
            // a task with no interval inside the window contributes 0 even
            // though `t.tracked_seconds` is nonzero, which is the whole fix.
            let tracked_contribution = if windowed {
                intervals
                    .get(&t.id)
                    .map(|ivs| windowed_overlap_secs(ivs, since, until))
                    .unwrap_or(0)
            } else {
                t.tracked_seconds
            };
            agg.tracked_secs = agg.tracked_secs.saturating_add(tracked_contribution);
            // A task carries many measurements (#11); its contribution to the
            // group is the sum of the four buckets across them. Saturating for
            // the same reason as the duration roll-ups above. Scope is already
            // handled: this runs only for tasks that survived the D24 skip and
            // the filter, so cancelled work stays out unless `all:true`.
            for m in &snapshot.tokens {
                // D97: windowed, a measurement counts only if IT was recorded
                // inside `[since, until)` — `created` is the instant the spend
                // was measured, independent of when its task completed (or
                // whether it ever did).
                if windowed {
                    // A variable key, not a literal `.get("created")` chain:
                    // `no_engine_param_is_read_with_a_raw_json_accessor`
                    // (D32) bans that shape store-wide because it cannot tell
                    // "absent" from "wrong type" — the same reason `bucket`
                    // two lines below reads through a closure parameter
                    // rather than four literal `.get("...")` calls.
                    let field = |key: &str| m.get(key).and_then(Value::as_str);
                    let created = field("created").and_then(parse_ts);
                    let in_window = match created {
                        Some(c) => since.is_none_or(|s| c >= s) && until.is_none_or(|u| c < u),
                        None => false,
                    };
                    if !in_window {
                        continue;
                    }
                }
                let bucket = |name: &str| m.get(name).and_then(Value::as_i64).unwrap_or(0);
                agg.tokens_in = agg.tokens_in.saturating_add(bucket("input_tokens"));
                agg.tokens_out = agg.tokens_out.saturating_add(bucket("output_tokens"));
                agg.tokens_cache_read = agg
                    .tokens_cache_read
                    .saturating_add(bucket("cache_read_tokens"));
                agg.tokens_cache_creation = agg
                    .tokens_cache_creation
                    .saturating_add(bucket("cache_creation_tokens"));
                let str_field = |name: &str| m.get(name).and_then(Value::as_str);
                if let Some(c) = str_field("confidence") {
                    let is_worse = match agg.tokens_confidence.as_deref() {
                        Some(existing) => {
                            crate::tokens::confidence_rank(c)
                                < crate::tokens::confidence_rank(existing)
                        }
                        None => true,
                    };
                    if is_worse {
                        agg.tokens_confidence = Some(c.to_string());
                    }
                }
            }
            // Shared with the filter DSL's `due.before:now` (#148) so this
            // count and that query can no longer independently drift.
            if is_overdue(t.status.is_open(), t.due.as_deref(), now_ts) {
                agg.overdue += 1;
            }
        }

        // Confidence is metadata about the token figures specifically, so it
        // travels with them: a caller who asked for none of the four buckets
        // gets no confidence marker either, rather than a field describing a
        // number this response never printed.
        let token_metric_requested = metrics.iter().any(|m| m.starts_with("tokens_"));

        let mut out = Vec::new();
        for (key, agg) in groups {
            let mut obj = Map::new();
            obj.insert(group_by.clone(), Value::String(key));
            obj.insert("count".into(), json!(agg.count));
            for m in &metrics {
                match m.as_str() {
                    "count" => {}
                    "est_total" => {
                        obj.insert("est_total".into(), json!(iso_duration(agg.est_secs)));
                    }
                    "tracked_total" => {
                        obj.insert(
                            "tracked_total".into(),
                            json!(iso_duration(agg.tracked_secs)),
                        );
                    }
                    "overdue" => {
                        obj.insert("overdue".into(), json!(agg.overdue));
                    }
                    "tokens_in" => {
                        obj.insert("tokens_in".into(), json!(agg.tokens_in));
                    }
                    "tokens_out" => {
                        obj.insert("tokens_out".into(), json!(agg.tokens_out));
                    }
                    "tokens_cache_read" => {
                        obj.insert("tokens_cache_read".into(), json!(agg.tokens_cache_read));
                    }
                    "tokens_cache_creation" => {
                        obj.insert(
                            "tokens_cache_creation".into(),
                            json!(agg.tokens_cache_creation),
                        );
                    }
                    _ => {}
                }
            }
            // D50's trust hierarchy (#217): the group's worst confidence,
            // carried past the sum that erases which measurement contributed
            // what. `report.summary`'s clients (the terminal report, the HTML
            // page, `--json dashboard`) render a token figure with no source
            // marker at all today — this is the field that lets them stop.
            if token_metric_requested {
                if let Some(c) = &agg.tokens_confidence {
                    obj.insert("tokens_confidence".into(), json!(c));
                }
            }
            out.push(Value::Object(obj));
        }

        Ok(json!({
            "groups": out,
            // The same instant everything above resolved against — not a
            // fourth clock read (`util::now` is `Timestamp::now().to_string()`,
            // so the wire format is unchanged).
            "generated": now_ts.to_string(),
            "filter": filter_str,
            "all": all,
            // #234 item 12: how many cancelled tasks the D24 default just
            // excluded that had non-zero token spend — always 0 under `all`
            // or a filter that already names a status. Additive to the v1
            // shape (see `tests/conformance.rs`'s `R_REPORT_SUMMARY`).
            "tokens_excluded_cancelled_tasks": tokens_excluded_cancelled_tasks,
            // D97: echoed for the same reason `filter` is (D69) — a report
            // whose `tracked_total`/token buckets are window-scoped must say
            // so, rather than let the reader assume a lifetime total. `null`
            // when omitted, exactly like an unset date field elsewhere.
            "since": since.map(|t| t.to_string()),
            "until": until.map(|t| t.to_string()),
            // See `task_list`'s field of the same name: an empty `groups` on
            // a genuinely empty store reads identically to one where the
            // filter matched nothing, and only this tells them apart (#233).
            "store_empty": self.store_is_empty()?,
        }))
    }

    // ---- report.outcomes -----------------------------------------------------

    /// `report.outcomes` — D137. What the work did, as opposed to what it cost.
    /// Params: `group_by` (one of [`SUMMARY_GROUP_BY`], default the first),
    /// `filter`, `metrics` (a subset of [`OUTCOME_METRICS`], default all of
    /// them), `since`/`until`.
    ///
    /// Writes nothing and adds no table: every figure is derived from rows the
    /// store already held. That is the ruling's own load-bearing claim — it is
    /// what makes the first run of this report **retroactive**, so D136, D138
    /// and D139 have a baseline without a measurement period first.
    ///
    /// The unit is the task, and a task is in scope when its most recent
    /// CLOSING event — a `done`, a `cancel`, or the `modify` that set
    /// `status: cancelled` — falls inside the window. Current status is
    /// deliberately not the scope test: a completion that was reopened is
    /// `pending` again, and scoping on status would drop precisely the rework
    /// this report exists to count.
    ///
    /// Every rate is emitted beside the `n` it was computed over. D137 rules
    /// that a rate over three completions and one over ninety are different
    /// claims that print identically, so no rate here is reachable without its
    /// denominator.
    pub fn report_outcomes(&self, p: &Value) -> Result<Value, ApiError> {
        let group_by = opt_str(p, "group_by")?.unwrap_or_else(|| SUMMARY_GROUP_BY[0].to_string());
        if !SUMMARY_GROUP_BY.contains(&group_by.as_str()) {
            return Err(ApiError::bad_request(format!(
                "group_by must be {} (got {group_by:?})",
                SUMMARY_GROUP_BY.join("|")
            )));
        }
        // Absent means ALL of them, which is where this differs from
        // `report.summary`'s `count`-only default: the metrics are meant to be
        // read together, and a rework rate with no cost beside it invites the
        // wrong fix. An unknown name is refused rather than dropped, the
        // `SUMMARY_METRICS` lesson applied before it can be relearned here.
        let metrics: Vec<String> = match p.get("metrics") {
            None => OUTCOME_METRICS.iter().map(|m| m.to_string()).collect(),
            Some(Value::Array(a)) => {
                let mut v = Vec::with_capacity(a.len());
                for m in a {
                    let name = m.as_str().filter(|s| OUTCOME_METRICS.contains(s));
                    let Some(name) = name else {
                        return Err(ApiError::bad_request(format!(
                            "unknown metric {m} (valid metrics: {})",
                            OUTCOME_METRICS.join(", ")
                        )));
                    };
                    v.push(name.to_string());
                }
                v
            }
            Some(_) => {
                return Err(ApiError::bad_request(
                    "`metrics` must be an array of metric names",
                ))
            }
        };
        let wants = |m: &str| metrics.iter().any(|x| x == m);

        let filter_str = opt_str(p, "filter")?.unwrap_or_default();
        let now_ts = Timestamp::now();
        let filter = Filter::parse(&filter_str, now_ts).map_err(ApiError::bad_request)?;
        validate_filter_projects(self.conn(), &filter)?;

        // D97's window vocabulary, not a second one — but over a different
        // axis than `report.summary` uses it for. There it bounds WHEN SPEND
        // HAPPENED inside a task's life; here it bounds WHEN THE TASK CLOSED,
        // because every metric below is a property of a closing rather than of
        // an interval.
        let since = opt_when(p, "since", now_ts)?
            .map(|when| {
                parse_ts(&when).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "`since` resolved to an unreadable instant {when:?}"
                    ))
                })
            })
            .transpose()?;
        let until = opt_when(p, "until", now_ts)?
            .map(|when| {
                parse_ts(&when).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "`until` resolved to an unreadable instant {when:?}"
                    ))
                })
            })
            .transpose()?;
        if let (Some(s), Some(u)) = (since, until) {
            if u <= s {
                return Err(ApiError::bad_request(format!(
                    "`until` ({u}) must be after `since` ({s})"
                )));
            }
        }

        let history = self.task_closing_history()?;
        let annotated = self.annotated_task_ids()?;

        struct Agg {
            /// Tasks in scope whose most recent close was a completion.
            completions: i64,
            /// Tasks in scope, closed either way — abandonment's denominator.
            closed: i64,
            rework: Vec<i64>,
            silent: Vec<i64>,
            abandoned: Vec<i64>,
            abandoned_secs: i64,
            ratios: Vec<f64>,
            /// Completions that carried a budget — `overrun`'s denominator.
            budgeted: i64,
            overrun: Vec<i64>,
            tokens_in: i64,
            tokens_out: i64,
            tokens_cache_read: i64,
            tokens_cache_creation: i64,
            /// Completions that contributed at least one measurement — the
            /// denominator `cost` is read against, which is NOT `completions`:
            /// a report whose cost covers three of twelve completions must not
            /// look like one that covers all twelve.
            measured: i64,
            /// D50's trust hierarchy survives the sum, exactly as it does in
            /// `report.summary`: the group's WORST grading, carried beside a
            /// total that has erased which measurement contributed what.
            confidence: Option<String>,
        }
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<String, Agg> = BTreeMap::new();

        for snapshot in
            self.load_task_snapshots_for(super::task::SnapshotParts::REPORT_SUMMARY, now_ts)?
        {
            let t = snapshot.task;
            let Some(close) = history.get(&t.id) else {
                // Never closed: open work is not an outcome yet. This is the
                // one exclusion in the report and it is a definition, not a
                // filter — `report.summary` is the read for work in flight.
                continue;
            };
            if let Some(s) = since {
                if close.at < s {
                    continue;
                }
            }
            if let Some(u) = until {
                if close.at >= u {
                    continue;
                }
            }
            let ctx = MatchCtx {
                status: t.status,
                priority: t.priority,
                project: t.project.as_deref(),
                tags: &snapshot.tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked: snapshot.blocked,
            };
            if !filter.matches(&ctx) {
                continue;
            }
            let key = match group_by.as_str() {
                "project" => t.project.clone().unwrap_or_else(|| "(none)".to_string()),
                // D28's choke point, for the same reason `report.summary` uses
                // it: the group key is a read surface and may not print a
                // placeholder as fact.
                "status" => t.status_text().to_string(),
                "priority" => t
                    .priority
                    .map(|x| x.as_str().to_string())
                    .unwrap_or_else(|| "(none)".to_string()),
                _ => unreachable!(),
            };
            let agg = groups.entry(key).or_insert(Agg {
                completions: 0,
                closed: 0,
                rework: Vec::new(),
                silent: Vec::new(),
                abandoned: Vec::new(),
                abandoned_secs: 0,
                ratios: Vec::new(),
                budgeted: 0,
                overrun: Vec::new(),
                tokens_in: 0,
                tokens_out: 0,
                tokens_cache_read: 0,
                tokens_cache_creation: 0,
                measured: 0,
                confidence: None,
            });
            agg.closed += 1;
            if !close.completed {
                // Abandonment is work that was STARTED and then cancelled. A
                // task cancelled without ever being started cost nothing and
                // is not abandoned effort — it is a task that went away, which
                // is what cancelling is for.
                if close.ever_started {
                    agg.abandoned.push(t.short_id);
                    agg.abandoned_secs = agg.abandoned_secs.saturating_add(t.tracked_seconds);
                }
                continue;
            }
            agg.completions += 1;
            // Rework is a completion that came back. `reopen` carries `from`,
            // so a reopen out of `cancelled` — resurrecting abandoned work —
            // is not counted here: nothing was completed to come back.
            if close.reopened_from_done {
                agg.rework.push(t.short_id);
            }
            if !annotated.contains(&t.id) {
                agg.silent.push(t.short_id);
            }
            // Calibration needs both numbers to be a ratio at all; a
            // completion missing either contributes to no `n`, which is why
            // the metric reports its own rather than borrowing `completions`.
            if let Some(est) = t.estimate.as_deref().and_then(duration_secs) {
                if est > 0 && t.tracked_seconds > 0 {
                    agg.ratios.push(t.tracked_seconds as f64 / est as f64);
                }
            }
            // D139: the overrun, and the first evidence this roadmap has had
            // about task size. The gauge is recomputed from the same
            // measurements the cost buckets below read, rather than from a
            // stored verdict: `over` is derived on every read precisely so it
            // cannot go stale against a budget somebody edited afterwards.
            if let Some(budget) = t.budget_tokens {
                agg.budgeted += 1;
                let fresh: i64 = snapshot.tokens.iter().fold(0, |sum, m| {
                    let bucket = |name: &str| m.get(name).and_then(Value::as_i64).unwrap_or(0);
                    sum.saturating_add(bucket("input_tokens"))
                        .saturating_add(bucket("output_tokens"))
                        .saturating_add(bucket("cache_creation_tokens"))
                });
                if fresh > budget {
                    agg.overrun.push(t.short_id);
                }
            }
            let mut contributed = false;
            for m in &snapshot.tokens {
                let bucket = |name: &str| m.get(name).and_then(Value::as_i64).unwrap_or(0);
                let (i, o, cr, cc) = (
                    bucket("input_tokens"),
                    bucket("output_tokens"),
                    bucket("cache_read_tokens"),
                    bucket("cache_creation_tokens"),
                );
                if i == 0 && o == 0 && cr == 0 && cc == 0 {
                    // D65: `tool`/`model` are recorded without a count and
                    // write no measurement — but a zero-count row reaching
                    // here from anywhere else must not inflate `measured`
                    // either, for the same reason: it is not an observation.
                    continue;
                }
                contributed = true;
                agg.tokens_in = agg.tokens_in.saturating_add(i);
                agg.tokens_out = agg.tokens_out.saturating_add(o);
                agg.tokens_cache_read = agg.tokens_cache_read.saturating_add(cr);
                agg.tokens_cache_creation = agg.tokens_cache_creation.saturating_add(cc);
                // A closure parameter rather than a literal key, the shape
                // `report_summary` uses two hundred lines up and for the same
                // reason: D32's guard bans the literal chain store-wide because
                // it cannot tell "absent" from "wrong type".
                let str_field = |name: &str| m.get(name).and_then(Value::as_str);
                if let Some(c) = str_field("confidence") {
                    let is_worse = match agg.confidence.as_deref() {
                        Some(existing) => {
                            crate::tokens::confidence_rank(c)
                                < crate::tokens::confidence_rank(existing)
                        }
                        None => true,
                    };
                    if is_worse {
                        agg.confidence = Some(c.to_string());
                    }
                }
            }
            if contributed {
                agg.measured += 1;
            }
        }

        let mut out = Vec::new();
        for (key, mut agg) in groups {
            let mut obj = Map::new();
            obj.insert(group_by.clone(), Value::String(key));
            obj.insert("completions".into(), json!(agg.completions));
            obj.insert("closed".into(), json!(agg.closed));
            if wants("rework") {
                agg.rework.sort_unstable();
                obj.insert(
                    "rework".into(),
                    json!({
                        "count": agg.rework.len(),
                        "n": agg.completions,
                        "rate": rate(agg.rework.len() as i64, agg.completions),
                        "refs": agg.rework,
                    }),
                );
            }
            if wants("calibration") {
                obj.insert(
                    "calibration".into(),
                    json!({
                        "median_ratio": median(&mut agg.ratios),
                        "n": agg.ratios.len(),
                    }),
                );
            }
            if wants("cost") {
                // The four keys `report.summary` already uses, not the
                // `input_tokens` spelling `task.done` and `task.get` use for
                // the same quantity. Both spellings exist in the store today,
                // so the question is which neighbourhood `cost` belongs to,
                // and it is this one: it is a report aggregate, it is read
                // beside `report.summary`'s groups, and the shared render
                // helpers (`tokens::BUCKETS`, `tokens::dominant_cell`) key off
                // these names. A sibling method spelling the same sum
                // differently is the drift D30 legislates against.
                let mut cost = Map::new();
                cost.insert("tokens_in".into(), json!(agg.tokens_in));
                cost.insert("tokens_out".into(), json!(agg.tokens_out));
                cost.insert("tokens_cache_read".into(), json!(agg.tokens_cache_read));
                cost.insert(
                    "tokens_cache_creation".into(),
                    json!(agg.tokens_cache_creation),
                );
                cost.insert("n".into(), json!(agg.measured));
                // Absent rather than null when nothing was measured: a
                // confidence grading describes figures, and there are none.
                if let Some(c) = &agg.confidence {
                    cost.insert("confidence".into(), json!(c));
                }
                obj.insert("cost".into(), Value::Object(cost));
            }
            if wants("silent") {
                agg.silent.sort_unstable();
                obj.insert(
                    "silent".into(),
                    json!({
                        "count": agg.silent.len(),
                        "n": agg.completions,
                        "rate": rate(agg.silent.len() as i64, agg.completions),
                        "refs": agg.silent,
                    }),
                );
            }
            if wants("overrun") {
                agg.overrun.sort_unstable();
                obj.insert(
                    "overrun".into(),
                    json!({
                        "count": agg.overrun.len(),
                        "n": agg.budgeted,
                        "rate": rate(agg.overrun.len() as i64, agg.budgeted),
                        "refs": agg.overrun,
                    }),
                );
            }
            if wants("abandonment") {
                agg.abandoned.sort_unstable();
                obj.insert(
                    "abandonment".into(),
                    json!({
                        "count": agg.abandoned.len(),
                        "n": agg.closed,
                        "rate": rate(agg.abandoned.len() as i64, agg.closed),
                        "tracked_total": iso_duration(agg.abandoned_secs),
                        "refs": agg.abandoned,
                    }),
                );
            }
            out.push(Value::Object(obj));
        }

        Ok(json!({
            "groups": out,
            "group_by": group_by,
            "metrics": metrics,
            "generated": now_ts.to_string(),
            // Echoed for D69's reason, the same one `report.summary` echoes
            // them for: a rate read against the wrong scope or period is worse
            // than no rate, because it is actionable and wrong.
            "filter": filter_str,
            "since": since.map(|t| t.to_string()),
            "until": until.map(|t| t.to_string()),
            "store_empty": self.store_is_empty()?,
        }))
    }

    /// Every task's closing history, read from the event log in one pass.
    ///
    /// `at`/`completed` describe the task's most recent CLOSE: a `done`, a
    /// `cancel`, or the `modify` that set `status: cancelled` — because
    /// `task.modify` is a second, equally valid path to cancellation (§7,
    /// "Cancellation goes through task.modify status:cancelled") and it writes
    /// a `modify` event rather than a `cancel` one. Reading only `cancel`
    /// would count one path and silently miss the other.
    ///
    /// `modify` is the one high-frequency op here, so the SQL pre-filters it
    /// on the payload text before any of it is parsed: an edit that named no
    /// status is not a close and never reaches this reader. `op` leads
    /// `idx_events_op`, so the other four are index seeks.
    ///
    /// `reopened_from_done` is a property of the TASK rather than of one
    /// close: a task done, reopened and done again came back, and the report
    /// says so on the completion that is in scope.
    fn task_closing_history(&self) -> Result<HashMap<String, TaskClose>, ApiError> {
        let mut stmt = self.conn().prepare(
            "SELECT entity_id, op, ts, payload FROM events \
             WHERE entity = 'task' \
               AND op IN ('done', 'cancel', 'reopen', 'start', 'modify') \
               AND (op <> 'modify' OR payload LIKE '%\"status\"%') \
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;

        let mut out: HashMap<String, TaskClose> = HashMap::new();
        for row in rows {
            let (task_id, op, ts, payload) = row?;
            let entry = out.entry(task_id).or_default();
            if op == "start" {
                entry.ever_started = true;
                continue;
            }
            // A row whose instant will not parse contributes no close rather
            // than aborting the report — `task_tracked_intervals`' rule, for
            // the same reason.
            let Some(at) = parse_ts(&ts) else { continue };
            let from = payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok());
            match op.as_str() {
                "done" => {
                    entry.at = at;
                    entry.completed = true;
                    entry.closed = true;
                }
                "cancel" => {
                    entry.at = at;
                    entry.completed = false;
                    entry.closed = true;
                }
                "modify" => {
                    // The pre-filter above only proves the word `status`
                    // appears in the payload; this is where it has to actually
                    // BE a cancellation. `task.modify` refuses every other
                    // status target, but the payload is the caller's `set`
                    // object and this reader may not assume that.
                    let cancelled = payload_str(from.as_ref(), "status") == Some("cancelled");
                    if cancelled {
                        entry.at = at;
                        entry.completed = false;
                        entry.closed = true;
                    }
                }
                "reopen" => {
                    // D137's rule: rework is a COMPLETION that came back.
                    // `reopen` carries the status it reopened out of, so
                    // resurrecting a cancelled task is visibly not that.
                    let was_done = payload_str(from.as_ref(), "from") == Some("done");
                    if was_done {
                        entry.reopened_from_done = true;
                    }
                }
                _ => {}
            }
        }
        out.retain(|_, v| v.closed);
        Ok(out)
    }

    /// The ids of tasks carrying at least one annotation.
    ///
    /// A set, not [`super::task::SnapshotParts`]'s `annotations` gate: `silent`
    /// asks whether there is any note at all, and loading every note to answer
    /// it is the log-shaped cost `REPORT_SUMMARY` was carved out to avoid.
    fn annotated_task_ids(&self) -> Result<HashSet<String>, ApiError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT DISTINCT task_id FROM annotations")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Reconstruct every task's tracked intervals — `(start, end)` pairs — from
    /// the event log, the only place that knows WHEN each second of
    /// `tracked_seconds` was earned. The `tasks` row itself carries only the
    /// lifetime sum (D97's motivating bug: a report windowed on that sum alone
    /// can only ever attribute it whole, to whichever month the task last
    /// touched the clock).
    ///
    /// `stop` closes the interval its own `start` opened. `done` and `cancel`
    /// close one too, WITHOUT a `stop` event of their own (`task_done`,
    /// `task_cancel` fold the elapsed time into `tracked_seconds` directly) —
    /// so a `start` with no matching `stop` is closed by whichever of
    /// `stop`/`done`/`cancel` comes next for that task, using the CLOSING
    /// event's own `ts`. Neither event's payload is read: `start`'s `ts` and
    /// the closing event's `ts` are the exact two endpoints already, which is
    /// simpler than parsing `stop`'s `tracked` back out of an ISO-8601 string
    /// and sidesteps the very asymmetry noted in audit #190 (`done`/`cancel`
    /// carry no `tracked` field at all).
    ///
    /// A task still active when this runs has an unclosed `start`; its
    /// interval is closed at `now_ts` — the same instant the rest of this
    /// report resolves against — so time still on the clock counts toward a
    /// window that reaches the present, matching the dashboard's "now card".
    fn task_tracked_intervals(
        &self,
        now_ts: Timestamp,
    ) -> Result<HashMap<String, Vec<(Timestamp, Timestamp)>>, ApiError> {
        let mut stmt = self.conn().prepare(
            "SELECT entity_id, op, ts FROM events \
             WHERE entity = 'task' AND op IN ('start', 'stop', 'done', 'cancel') \
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;

        let mut open: HashMap<String, Timestamp> = HashMap::new();
        let mut out: HashMap<String, Vec<(Timestamp, Timestamp)>> = HashMap::new();
        for row in rows {
            let (task_id, op, ts) = row?;
            // A row this reader cannot parse contributes no interval rather
            // than aborting the whole report — the same "degrade, never
            // panic" rule `parse_ts`'s callers already follow throughout this
            // file.
            let Some(at) = parse_ts(&ts) else { continue };
            match op.as_str() {
                "start" => {
                    open.insert(task_id, at);
                }
                _ => {
                    if let Some(started) = open.remove(&task_id) {
                        out.entry(task_id).or_default().push((started, at));
                    }
                }
            }
        }
        for (task_id, started) in open {
            out.entry(task_id).or_default().push((started, now_ts));
        }
        Ok(out)
    }
}

/// Seconds of overlap between `intervals` and the half-open window
/// `[since, until)` — either bound `None` meaning unbounded on that side.
/// Shared by nothing else on purpose: this is `report.summary`'s one
/// consumer, over intervals [`Engine::task_tracked_intervals`] already
/// resolved to instants, so there is no bound left to fail to parse here.
fn windowed_overlap_secs(
    intervals: &[(Timestamp, Timestamp)],
    since: Option<Timestamp>,
    until: Option<Timestamp>,
) -> i64 {
    intervals
        .iter()
        .map(|(start, end)| {
            let lo = since.map_or(*start, |s| s.max(*start));
            let hi = until.map_or(*end, |u| u.min(*end));
            if hi > lo {
                hi.as_second() - lo.as_second()
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D109: `report.summary`'s `filter` is one of the three call sites that
    /// share `validate_filter_projects` (`task.list`, `store.export` are the
    /// other two) — an unknown/wrong-case `project:` must refuse here too,
    /// not just on the read path most people reach for first.
    #[test]
    fn report_summary_refuses_a_project_filter_naming_no_live_project() {
        let e = Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "work" })).unwrap();
        e.task_add(&json!({ "title": "t", "project": "work" }))
            .unwrap();

        let err = e
            .report_summary(&json!({ "filter": "project:Work" }))
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::NotFound);
        assert!(err.message.contains("Work"), "{}", err.message);
    }

    /// #234 item 12: a cancelled task's token spend is dropped from the
    /// default report with nothing saying it was excluded (D24 says nothing
    /// about spend — it excludes cancelled tasks from COUNTS). This test
    /// wants a way for a caller to learn the default view omitted spend, not
    /// (yet) a change to what the default totals contain.
    #[test]
    fn report_names_cancelled_spend_it_left_out_of_the_default_totals() {
        let e = Engine::open_in_memory().unwrap();
        let sid = e.task_add(&json!({ "title": "abandoned" })).unwrap()["short_id"].clone();
        e.token_add(&json!({
            "ref": sid, "tool": "claude-code", "source": "self-report",
            "input_tokens": 500, "confidence": "medium",
        }))
        .unwrap();
        e.task_cancel(&json!({ "ref": sid })).unwrap();

        let default = e.report_summary(&json!({})).unwrap();
        assert_eq!(
            default["tokens_excluded_cancelled_tasks"],
            json!(1),
            "one cancelled task with spend was excluded, and the response must say so: {default}"
        );

        // `--all` includes it, and correspondingly nothing was excluded.
        let all = e.report_summary(&json!({ "all": true })).unwrap();
        assert_eq!(all["tokens_excluded_cancelled_tasks"], json!(0));
    }

    /// The same fixture `increment.rs` uses for the B1 cluster: a row whose
    /// `status` column holds text `Status::parse` rejects. `store.import`
    /// accepted such a value until that cluster closed the hole, so this is a
    /// real store shape an upgrade has to keep readable, not a hypothetical.
    fn store_with_an_unrecognized_status() -> Engine {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "important work" })).unwrap();
        e.conn()
            .execute("UPDATE tasks SET status = 'Done'", [])
            .unwrap();
        e
    }

    /// `report.summary` aggregates tasks, tags, `blocked` and the token
    /// measurements — never an annotation and never the edge list. Dropping
    /// the table is what PROVES the read is gone (the same proof as
    /// `task_list_never_touches_the_annotations_table` in task.rs, and the
    /// same stake: annotations grow with every note ever written while the
    /// report does not, so a summary that scanned them got slower with the
    /// log, forever).
    #[test]
    fn report_summary_never_touches_the_annotations_table() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "work", "tags": ["a"] }))
            .unwrap();
        e.annotation_add(&json!({ "ref": 1, "body": "a note" }))
            .unwrap();
        e.conn().execute_batch("DROP TABLE annotations").unwrap();

        let out = e.report_summary(&json!({})).unwrap();
        assert_eq!(out["groups"][0]["count"], json!(1));
    }

    /// D28: `report.summary --group_by status` is a read surface like any other,
    /// so it must name the stored text, not the placeholder. Grouping on
    /// `t.status` bypassed `Task::status_text` — the single choke point that
    /// exists so no surface can print `Pending` as though it were the fact — and
    /// filed the row under `pending`. That is worse than a cosmetic mislabel:
    /// `tasqx list` and `store.export` both call the same row `Done`, so the
    /// report showed one extra open task whose name matched nothing the user
    /// could find anywhere else.
    #[test]
    fn group_by_status_names_the_stored_text_not_the_placeholder() {
        let e = store_with_an_unrecognized_status();
        let out = e
            .report_summary(&json!({ "group_by": "status" }))
            .expect("the report must survive a status the reader cannot parse");
        let groups = out["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "one row in, one group out: {out}");
        assert_eq!(
            groups[0]["status"],
            json!("Done"),
            "report.summary laundered the anomaly into the placeholder"
        );
    }

    /// The counterpart, kept adjacent so a future "just use status_raw" shortcut
    /// cannot pass by accident: on a well-formed row `status_text` is the
    /// canonical name, and the group key must stay exactly the lowercase word
    /// the filter grammar and the HTML report's CSS classes already use.
    #[test]
    fn group_by_status_still_names_recognized_statuses_canonically() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "a" })).unwrap();
        let out = e.report_summary(&json!({ "group_by": "status" })).unwrap();
        assert_eq!(out["groups"][0]["status"], json!("pending"));
    }

    /// Audit #190, reproduced directly: a task worked a 7.5h interval in July
    /// and a 1h interval in August, then completed in September. Before D97,
    /// the only date-scoped report (`completed.after:`/`completed.before:`)
    /// had no way to ask "how much of this was July's" — it could only bank
    /// the WHOLE `tracked_seconds` (8h30m) onto whichever month completion
    /// fell in, and every other month saw none of it. `since`/`until` must
    /// see each interval in its own month, and the completion month — which
    /// had no actual work — must see none.
    #[test]
    fn since_until_window_tracked_seconds_by_interval_not_by_completion_month() {
        let e = Engine::open_in_memory().unwrap();
        let id = e.task_add(&json!({ "title": "cross-month work" })).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Interval 1: 2026-07-05 10:00 -> 17:30 (PT7H30M).
        e.conn()
            .execute(
                "INSERT INTO events (id, entity, entity_id, op, payload, ts) VALUES \
             ('evt-1-start', 'task', ?1, 'start', \
             '{\"interval_started\":\"2026-07-05T10:00:00Z\"}', '2026-07-05T10:00:00Z')",
                params![id],
            )
            .unwrap();
        e.conn()
            .execute(
                "INSERT INTO events (id, entity, entity_id, op, payload, ts) VALUES \
             ('evt-1-stop', 'task', ?1, 'stop', '{\"tracked\":\"PT7H30M\"}', \
             '2026-07-05T17:30:00Z')",
                params![id],
            )
            .unwrap();
        // Interval 2: 2026-08-20 09:00 -> 10:00 (PT1H).
        e.conn()
            .execute(
                "INSERT INTO events (id, entity, entity_id, op, payload, ts) VALUES \
             ('evt-2-start', 'task', ?1, 'start', \
             '{\"interval_started\":\"2026-08-20T09:00:00Z\"}', '2026-08-20T09:00:00Z')",
                params![id],
            )
            .unwrap();
        e.conn()
            .execute(
                "INSERT INTO events (id, entity, entity_id, op, payload, ts) VALUES \
             ('evt-2-stop', 'task', ?1, 'stop', '{\"tracked\":\"PT1H\"}', \
             '2026-08-20T10:00:00Z')",
                params![id],
            )
            .unwrap();
        e.conn()
            .execute(
                "UPDATE tasks SET tracked_seconds = 30600, status = 'done', \
                 completed = '2026-09-09T12:00:00Z' WHERE id = ?1",
                params![id],
            )
            .unwrap();

        let metrics = json!(["tracked_total"]);

        let july = e
            .report_summary(
                &json!({ "since": "2026-07-01", "until": "2026-08-01", "metrics": metrics }),
            )
            .unwrap();
        assert_eq!(
            july["groups"][0]["tracked_total"],
            json!("PT7H30M"),
            "July's window must see only July's interval: {july}"
        );

        let august = e
            .report_summary(
                &json!({ "since": "2026-08-01", "until": "2026-09-01", "metrics": metrics }),
            )
            .unwrap();
        assert_eq!(
            august["groups"][0]["tracked_total"],
            json!("PT1H"),
            "August's window must see only August's interval: {august}"
        );

        // September is the COMPLETION month, and the bug's exact failure: no
        // actual work happened there, so a correct window must answer PT0S —
        // never the whole PT8H30M lifetime total the old completion-scoped
        // report would have banked here.
        let september = e
            .report_summary(
                &json!({ "since": "2026-09-01", "until": "2026-10-01", "metrics": metrics }),
            )
            .unwrap();
        assert_eq!(
            september["groups"][0]["tracked_total"],
            json!("PT0S"),
            "September had no work at all; the completion month must not inherit the lifetime total: {september}"
        );

        // Unwindowed (no since/until): unchanged, lifetime-total behaviour.
        let lifetime = e.report_summary(&json!({ "metrics": metrics })).unwrap();
        assert_eq!(lifetime["groups"][0]["tracked_total"], json!("PT8H30M"));
    }

    /// Audit #224, reproduced directly: a task completed weeks before the
    /// report's window, but one of its token measurements was RECORDED
    /// (`created`) inside the window — the log-parse-after-the-fact shape the
    /// audit found. Before D97 the only date-scoped axis was `completed.*`,
    /// so a report scoped to "this week" excluded the task entirely and its
    /// whole spend vanished, even though the spend was measured this week.
    #[test]
    fn since_until_windows_token_buckets_by_measurement_date_not_completion_date() {
        let e = Engine::open_in_memory().unwrap();
        let id = e
            .task_add(&json!({ "title": "old task, fresh measurement" }))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        e.conn()
            .execute(
                "UPDATE tasks SET status = 'done', completed = '2026-08-21T10:00:00Z' \
                 WHERE id = ?1",
                params![id],
            )
            .unwrap();
        e.conn()
            .execute(
                "INSERT INTO token_usage (id, task_id, tool, source, model, input_tokens, \
             output_tokens, cache_read_tokens, cache_creation_tokens, confidence, created) \
             VALUES ('tok-1', ?1, 'claude-code', 'log-parse', NULL, 0, 0, 103441261, 0, \
             'medium', '2026-09-09T12:00:00Z')",
                params![id],
            )
            .unwrap();

        // The bug: a completion-scoped "this week" report never sees this
        // task at all, so its 103.4M cache-read tokens are silently absent.
        let by_completion = e
            .report_summary(&json!({
                "filter": "completed.after:2026-09-02",
                "metrics": ["tokens_cache_read"],
            }))
            .unwrap();
        assert!(
            by_completion["groups"].as_array().unwrap().is_empty(),
            "sanity check: completion-scoped filtering excludes this task, \
             which is exactly the defect since/until exists to route around: {by_completion}"
        );

        // The fix: windowing by WHEN THE MEASUREMENT LANDED finds it.
        let by_measurement = e
            .report_summary(&json!({
                "since": "2026-09-02",
                "metrics": ["tokens_cache_read"],
            }))
            .unwrap();
        assert_eq!(
            by_measurement["groups"][0]["tokens_cache_read"],
            json!(103_441_261),
            "a measurement recorded this week must count toward this week's report \
             regardless of when its task completed: {by_measurement}"
        );

        // And a window that EXCLUDES the measurement's instant must see none
        // of it, proving this is real windowing and not just "ignore since".
        let before = e
            .report_summary(&json!({
                "until": "2026-09-09",
                "metrics": ["tokens_cache_read"],
            }))
            .unwrap();
        assert_eq!(
            before["groups"][0]["tokens_cache_read"],
            json!(0),
            "a window ending before the measurement must not count it: {before}"
        );
    }

    /// #217: D50's trust hierarchy lives in `source`/`confidence` on each
    /// measurement, but `report.summary` used to sum the four buckets and drop
    /// both — a group whose entire volume came from a low-confidence discovery
    /// pass read exactly like one built from verified `otel` correlations. The
    /// aggregate must carry the group's WORST confidence forward, so a reader
    /// of the rolled-up number can still tell it is not fully trustworthy.
    #[test]
    fn report_summary_carries_the_groups_worst_confidence() {
        let e = Engine::open_in_memory().unwrap();
        let sid = e.task_add(&json!({ "title": "a" })).unwrap()["short_id"].clone();
        e.token_add(&json!({
            "ref": sid, "tool": "claude-code", "source": "otel",
            "input_tokens": 10, "confidence": "high",
        }))
        .unwrap();
        e.token_add(&json!({
            "ref": sid, "tool": "claude-code", "source": "log-parse",
            "input_tokens": 5, "confidence": "low",
        }))
        .unwrap();

        let out = e
            .report_summary(&json!({ "group_by": "project", "metrics": ["tokens_in"] }))
            .unwrap();
        assert_eq!(
            out["groups"][0]["tokens_confidence"],
            json!("low"),
            "the low measurement must not be laundered away by the high one: {out}"
        );
    }

    /// The field is metadata about the token metrics specifically: a caller
    /// who never asked for a token figure gets no confidence marker either,
    /// so the two always travel together.
    #[test]
    fn report_summary_omits_confidence_when_no_token_metric_was_requested() {
        let e = Engine::open_in_memory().unwrap();
        let sid = e.task_add(&json!({ "title": "a" })).unwrap()["short_id"].clone();
        e.token_add(&json!({
            "ref": sid, "tool": "claude-code", "source": "log-parse",
            "input_tokens": 5, "confidence": "low",
        }))
        .unwrap();

        let out = e
            .report_summary(&json!({ "group_by": "project", "metrics": ["count"] }))
            .unwrap();
        assert!(
            out["groups"][0].get("tokens_confidence").is_none(),
            "confidence leaked without a token metric requested: {out}"
        );
    }
}

/// One task's closing history, as [`Engine::task_closing_history`] reads it.
///
/// `closed` rather than an `Option<Timestamp>` because `ever_started` is
/// collected for tasks that never closed at all (a `start` alone creates the
/// entry), and those are dropped at the end rather than being representable
/// half-way through.
#[derive(Default)]
struct TaskClose {
    /// The instant of the most recent close. Meaningless unless `closed`.
    at: Timestamp,
    /// Whether that most recent close was a completion rather than a
    /// cancellation.
    completed: bool,
    closed: bool,
    /// Whether this task has ever been reopened out of `done`.
    reopened_from_done: bool,
    /// Whether this task has ever had its clock started. Abandonment counts
    /// work that was picked up and dropped, not a task that was struck off.
    ever_started: bool,
}

/// `numerator / denominator` as a JSON number, or `null` when there is nothing
/// to divide by.
///
/// Null rather than 0.0 because the two say different things and a reader
/// cannot tell them apart once printed: 0.0 is "none of them", null is "there
/// were none". D137 asks every rate to carry its denominator; this is the case
/// where the denominator is what there is to say.
fn rate(numerator: i64, denominator: i64) -> Value {
    if denominator <= 0 {
        return Value::Null;
    }
    json!(numerator as f64 / denominator as f64)
}

/// The median of `samples`, or `null` when there are none. Sorts in place.
///
/// A median and not a mean, because one task that ran ten times its estimate
/// would otherwise become the project's calibration figure. At even `n` it is
/// the mean of the two middle samples, the ordinary convention.
fn median(samples: &mut [f64]) -> Value {
    if samples.is_empty() {
        return Value::Null;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = samples.len() / 2;
    let m = if samples.len().is_multiple_of(2) {
        (samples[mid - 1] + samples[mid]) / 2.0
    } else {
        samples[mid]
    };
    json!(m)
}

/// One string field out of an event payload, or `None`.
///
/// A function taking the key as a parameter rather than four literal
/// `.get("...")` chains: D32's guard bans that shape across the engine because
/// it cannot tell an absent value from a wrong-typed one, and `report_summary`
/// already reads its measurement fields through the same closure form. An
/// event payload is not a caller param, so `util`'s typed layer — which
/// answers with an `ApiError` naming the offending param — is the wrong tool
/// here: a malformed payload row degrades to "no close" rather than failing
/// the whole report.
fn payload_str<'a>(payload: Option<&'a Value>, key: &str) -> Option<&'a str> {
    payload.and_then(|v| v.get(key)).and_then(Value::as_str)
}
