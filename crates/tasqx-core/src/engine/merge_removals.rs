//! D197: `store.import --merge` carries removals as events.
//!
//! D185 keeps tombstones out of the export, so a union alone brought back
//! whatever one machine removed the moment it merged the other's older copy.
//! The event log already records every removal, so a merge reads the UNION
//! of both logs — this store's and the document's — and, per row, compares
//! the latest removal with the latest write that (re)adds or edits it, in
//! [`field_merge::order`]. The removal stands when it is the later one; a
//! re-add after it wins. Both directions of a merge read the same union, so
//! they agree.
//!
//! A row is known by one or more keys, and all of them are compared at
//! once: a doc by its id and its `source` (two stores that each imported one
//! file hold it under two ids, D183), a link by its id and its edge (two
//! stores can hold one edge under two ids, D181). Its removal is the latest
//! removal of any of its keys, and its last write the latest write to any of
//! them.
//!
//! An annotation's removal is final. Its id is never re-added, and D113
//! scrubs its text for being a possible secret: an edit written elsewhere
//! before the removal arrived must not bring the text back.
//!
//! `store_import` calls this only on a merge; a plain restore keeps D185's
//! contract and applies no removal.

use std::collections::HashMap;

use jiff::Timestamp;
use serde_json::Value;

use super::field_merge::{self, LoggedEvent};

type Stamp = (Option<Timestamp>, u8, String);

fn stamp(ev: &LoggedEvent) -> Stamp {
    let (ts, rank, id) = field_merge::order(ev);
    (ts, rank, id.to_string())
}

/// The latest removal and the latest write of every row key over one union
/// log.
#[derive(Default)]
pub(super) struct Ledger {
    /// Key -> the latest removal's place in the order and its `ts`.
    removed: HashMap<String, (Stamp, String)>,
    written: HashMap<String, Stamp>,
}

impl Ledger {
    fn note(&mut self, key: String, ev: &LoggedEvent, removal: bool) {
        let at = stamp(ev);
        if removal {
            let held = self
                .removed
                .entry(key)
                .or_insert_with(|| (at.clone(), ev.ts.clone()));
            if at > held.0 {
                *held = (at, ev.ts.clone());
            }
        } else {
            let held = self.written.entry(key).or_insert_with(|| at.clone());
            if at > *held {
                *held = at;
            }
        }
    }

    /// The `ts` of the removal that stands for the row known by `keys`: the
    /// latest removal of any of them, when no write to any of them is later.
    pub fn removal(&self, keys: &[String]) -> Option<&str> {
        let (at, ts) = keys
            .iter()
            .filter_map(|k| self.removed.get(k))
            .max_by(|a, b| a.0.cmp(&b.0))?;
        let written = keys.iter().filter_map(|k| self.written.get(k)).max();
        match written {
            Some(w) if w >= at => None,
            _ => Some(ts),
        }
    }

    /// What follows `prefix` in every key some removal names: where a store
    /// looks for the rows a merge may have to delete.
    pub fn removed_under<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> {
        self.removed
            .keys()
            .filter_map(move |k| k.strip_prefix(prefix))
    }
}

pub(super) fn tag_key(name: &str) -> String {
    format!("tag:{name}")
}

pub(super) fn check_key(id: &str) -> String {
    format!("check:{id}")
}

pub(super) fn annotation_key(id: &str) -> String {
    format!("annotation:{id}")
}

pub(super) fn dependency_key(id: &str) -> String {
    format!("dependency:{id}")
}

/// The keys a doc is known by: its id, and its `source` when it has one.
pub(super) fn doc_keys(id: &str, source: Option<&str>) -> Vec<String> {
    let mut keys = vec![format!("doc:{id}")];
    if let Some(s) = source.filter(|s| !s.is_empty()) {
        keys.push(format!("source:{s}"));
    }
    keys
}

/// The keys a link is known by: its id, and its edge.
pub(super) fn link_keys(id: &str, from: &str, to: &str, relation: &str) -> Vec<String> {
    vec![format!("link:{id}"), edge_key(from, to, relation)]
}

fn edge_key(from: &str, to: &str, relation: &str) -> String {
    format!("edge:{from} {to} {relation}")
}

/// An edge key's `(from, to, relation)`, as [`Ledger::removed_under`]
/// answers it for the prefix `edge:`.
pub(super) fn split_edge(edge: &str) -> Option<(&str, &str, &str)> {
    let mut parts = edge.splitn(3, ' ');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

fn strings(v: &Value) -> impl Iterator<Item = &str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str)
}

/// The rows one task event writes (`false`) or removes (`true`), by key.
fn task_effects(ev: &LoggedEvent, by_id: &HashMap<&str, &LoggedEvent>) -> Vec<(String, bool)> {
    let p = &ev.payload;
    let id = |f: fn(&str) -> String, key: &str| p[key].as_str().map(f).into_iter();
    match ev.op.as_str() {
        "add" | "tag.add" => strings(&p["tags"]).map(|t| (tag_key(t), false)).collect(),
        "tag.remove" => strings(&p["tags"]).map(|t| (tag_key(t), true)).collect(),
        "check.add" | "check.set" => id(check_key, "id").map(|k| (k, false)).collect(),
        "check.remove" => id(check_key, "id").map(|k| (k, true)).collect(),
        // Final: no write to an annotation is ever counted against its
        // removal (module docs).
        "annotation.remove" => id(annotation_key, "id").map(|k| (k, true)).collect(),
        "dependency.add" => id(dependency_key, "depends_on")
            .map(|k| (k, false))
            .collect(),
        "dependency.remove" => id(dependency_key, "depends_on")
            .map(|k| (k, true))
            .collect(),
        // An undone removal puts back what the removal named: a write, at
        // the undo's own place in the order.
        "undo" => match p["reverted_op"].as_str() {
            Some("tag.remove" | "dependency.remove") => p["reverted"]
                .as_str()
                .and_then(|r| by_id.get(r))
                .map(|reverted| task_effects(reverted, by_id))
                .unwrap_or_default()
                .into_iter()
                .map(|(k, _)| (k, false))
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The ledger of one task's child rows — tags, checks, annotations and
/// dependency edges — over the union of both logs for it.
pub(super) fn task_ledger(log: &[LoggedEvent]) -> Ledger {
    let by_id: HashMap<&str, &LoggedEvent> = log.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut ledger = Ledger::default();
    for ev in log {
        for (key, removal) in task_effects(ev, &by_id) {
            ledger.note(key, ev, removal);
        }
    }
    ledger
}

/// The ledger of memory docs over `(entity_id, event)` pairs from both
/// logs. The `memory.add` a `store.import` writes as bookkeeping is no write
/// of the doc's: it is stamped with the import's own instant, so counting it
/// would let any stale copy arriving through a later import outlive a
/// removal made before that import ran.
pub(super) fn doc_ledger(log: &[(String, LoggedEvent)]) -> Ledger {
    let mut ledger = Ledger::default();
    for (entity_id, ev) in log {
        let removal = match ev.op.as_str() {
            "memory.remove" => true,
            "memory.add" if ev.payload["via"] == "store.import" => continue,
            "memory.add" | "memory.update" => false,
            _ => continue,
        };
        for key in doc_keys(entity_id, ev.payload["source"].as_str()) {
            ledger.note(key, ev, removal);
        }
    }
    ledger
}

/// The ledger of links over `(entity_id, event)` pairs from both logs. A
/// `link.remove` written before D197 names no edge, so it is known by its id
/// alone.
pub(super) fn link_ledger(log: &[(String, LoggedEvent)]) -> Ledger {
    let mut ledger = Ledger::default();
    for (entity_id, ev) in log {
        let removal = match ev.op.as_str() {
            "link.remove" => true,
            "link.add" => false,
            _ => continue,
        };
        let p = &ev.payload;
        let edge = match (p["from"].as_str(), p["to"].as_str(), p["relation"].as_str()) {
            (Some(f), Some(t), Some(r)) if !f.is_empty() && !t.is_empty() && !r.is_empty() => {
                link_keys(entity_id, f, t, r)
            }
            _ => vec![format!("link:{entity_id}")],
        };
        for key in edge {
            ledger.note(key, ev, removal);
        }
    }
    ledger
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(id: &str, op: &str, payload: Value, ts: &str) -> LoggedEvent {
        LoggedEvent {
            id: id.to_string(),
            op: op.to_string(),
            payload,
            ts: ts.to_string(),
        }
    }

    #[test]
    fn a_removal_stands_until_a_later_write_to_any_key_of_the_row() {
        let mut log = vec![
            ev("1", "add", json!({ "tags": ["x"] }), "2026-09-01T00:00:00Z"),
            ev(
                "2",
                "tag.remove",
                json!({ "tags": ["x"] }),
                "2026-09-02T00:00:00Z",
            ),
        ];
        assert!(task_ledger(&log).removal(&[tag_key("x")]).is_some());
        log.push(ev(
            "3",
            "tag.add",
            json!({ "tags": ["x"] }),
            "2026-09-03T00:00:00Z",
        ));
        assert!(task_ledger(&log).removal(&[tag_key("x")]).is_none());
    }

    #[test]
    fn an_undone_removal_is_a_write() {
        let log = [
            ev(
                "1",
                "tag.remove",
                json!({ "tags": ["x"] }),
                "2026-09-02T00:00:00Z",
            ),
            ev(
                "2",
                "undo",
                json!({ "reverted": "1", "reverted_op": "tag.remove" }),
                "2026-09-03T00:00:00Z",
            ),
        ];
        assert!(task_ledger(&log).removal(&[tag_key("x")]).is_none());
    }

    #[test]
    fn an_annotation_removal_is_final() {
        let log = [
            ev(
                "1",
                "annotation.remove",
                json!({ "id": "n" }),
                "2026-09-02T00:00:00Z",
            ),
            ev(
                "2",
                "annotation.update",
                json!({ "id": "n" }),
                "2026-09-03T00:00:00Z",
            ),
        ];
        assert!(task_ledger(&log).removal(&[annotation_key("n")]).is_some());
    }

    #[test]
    fn a_doc_re_imported_under_a_new_id_outlives_the_old_ids_removal() {
        let log = [
            (
                "old".to_string(),
                ev(
                    "1",
                    "memory.remove",
                    json!({ "source": "d.md" }),
                    "2026-09-02T00:00:00Z",
                ),
            ),
            (
                "new".to_string(),
                ev(
                    "2",
                    "memory.add",
                    json!({ "source": "d.md" }),
                    "2026-09-03T00:00:00Z",
                ),
            ),
            (
                "new".to_string(),
                ev(
                    "3",
                    "memory.add",
                    json!({ "source": "d.md", "via": "store.import" }),
                    "2026-09-04T00:00:00Z",
                ),
            ),
        ];
        let ledger = doc_ledger(&log);
        assert!(ledger.removal(&doc_keys("old", Some("d.md"))).is_none());
        assert!(ledger.removal(&doc_keys("old", None)).is_some());
    }
}
