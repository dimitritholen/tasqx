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

        let filter = Filter::parse(&opt_str(p, "filter")?.unwrap_or_default(), Timestamp::now())
            .map_err(ApiError::bad_request)?;
        let now_ts = parse_ts(&now());

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
        }
        let mut groups: BTreeMap<String, Agg> = BTreeMap::new();

        for snapshot in self.load_task_snapshots()? {
            let t = snapshot.task;
            if apply_default && !t.status.counts_in_reports() {
                continue;
            }
            let ctx = MatchCtx {
                status: t.status,
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
            });
            agg.count += 1;
            // Saturating: a single estimate is bounded by `duration_secs`, but a
            // roll-up sums arbitrarily many rows. A clamped total is wrong-but-
            // visible; a wrapped one is negative nonsense and a panic in debug.
            if let Some(e) = t.estimate.as_deref().and_then(duration_secs) {
                agg.est_secs = agg.est_secs.saturating_add(e);
            }
            agg.tracked_secs = agg.tracked_secs.saturating_add(t.tracked_seconds);
            // A task carries many measurements (#11); its contribution to the
            // group is the sum of the four buckets across them. Saturating for
            // the same reason as the duration roll-ups above. Scope is already
            // handled: this runs only for tasks that survived the D24 skip and
            // the filter, so cancelled work stays out unless `all:true`.
            for m in &snapshot.tokens {
                let bucket = |name: &str| m.get(name).and_then(Value::as_i64).unwrap_or(0);
                agg.tokens_in = agg.tokens_in.saturating_add(bucket("input_tokens"));
                agg.tokens_out = agg.tokens_out.saturating_add(bucket("output_tokens"));
                agg.tokens_cache_read = agg
                    .tokens_cache_read
                    .saturating_add(bucket("cache_read_tokens"));
                agg.tokens_cache_creation = agg
                    .tokens_cache_creation
                    .saturating_add(bucket("cache_creation_tokens"));
            }
            if t.status.is_open() {
                if let (Some(due), Some(n)) = (t.due.as_deref().and_then(parse_ts), now_ts) {
                    if due < n {
                        agg.overdue += 1;
                    }
                }
            }
        }

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
            out.push(Value::Object(obj));
        }

        Ok(json!({ "groups": out, "generated": now() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
