//! The D160 knowledge graph: the node reference every graph surface resolves,
//! and the explicit `link.*` family that persists edges between nodes.
//!
//! # Why a node reference is not a task ref
//!
//! Every other method here names ONE kind of thing, so `resolve_ref` can answer
//! "which task" from a bare integer or a uuid and be done. A graph edge spans
//! four kinds — a task, a memory doc, an annotation, a project — so a bare uuid
//! is ambiguous and a bare integer is not. [`Engine::parse_node_ref_on`] is the
//! one place that ambiguity is settled, and it is settled once: `graph.query`'s
//! `root` and both ends of a link go through it, so a reference that works on
//! one cannot be refused by the other.

use super::*;

/// The four kinds of node the graph holds (D160).
///
/// `Memory` and not `Doc`: [`Entity::Doc`] is the event log's spelling for the
/// same row, and the two are deliberately different words because they answer
/// different questions — `event.list {entity: "doc"}` is about the audit trail
/// and `memory:<uuid>` is a position in the graph. The wire spelling here is
/// what a node id carries, and it matches the vocabulary D160 states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeType {
    /// A task row, named by short id or uuid.
    Task,
    /// A memory document (the `docs` table).
    Memory,
    /// One note on a task. Still a node after `annotation.remove`, which
    /// tombstones the row rather than deleting it (D113).
    Annotation,
    /// A project row, named by uuid or by its name.
    Project,
}

impl NodeType {
    /// Every variant, in the order a bare uuid is probed against the store.
    /// Hand-written for [`Entity::ALL`]'s reason, and pinned by the same kind of
    /// round-trip test.
    pub const ALL: [NodeType; 4] = [
        NodeType::Task,
        NodeType::Memory,
        NodeType::Annotation,
        NodeType::Project,
    ];

    /// The wire spelling — the prefix of a node id, and the word a
    /// `node_types` filter names.
    pub fn as_str(self) -> &'static str {
        match self {
            NodeType::Task => "task",
            NodeType::Memory => "memory",
            NodeType::Annotation => "annotation",
            NodeType::Project => "project",
        }
    }

    /// The inverse of [`NodeType::as_str`], exact match only — what lets an
    /// unknown prefix be a `bad_request` naming the four instead of a lookup
    /// that finds nothing.
    pub fn parse(s: &str) -> Option<NodeType> {
        NodeType::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// The accepted set as a message fragment, built from [`NodeType::ALL`] so
    /// a refusal cannot fall behind the enum.
    fn accepted() -> String {
        NodeType::ALL
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A resolved node: its kind and the uuid of the row it named.
///
/// Resolved, not merely parsed — every value of this type came back from a
/// store lookup, which is what makes the `<type>:<uuid>` id it renders stable
/// and makes a missing endpoint a `not_found` at the door rather than a
/// dangling row discovered later.
type Node = (NodeType, String);

/// The id a node is known by everywhere outside the store: `task:<uuid>`.
fn node_id((ty, id): &Node) -> String {
    format!("{}:{}", ty.as_str(), id)
}

/// The relations `link.add` accepts (D160).
///
/// A closed vocabulary refused by name on a typo (D34), and the source of truth
/// for the engine's validation and its rejection message — the shape
/// [`CHECK_STATES`] established. Fixed for now and persisted as a STRING, so
/// the day a sixth relation is agreed it costs a line here and no migration:
/// the schema deliberately puts no CHECK on the column.
pub const LINK_RELATIONS: [&str; 5] = [
    "references",
    "supersedes",
    "implements_decision",
    "derived_from",
    "contradicts",
];

/// `link.list`'s page when the caller names no `limit`, and the ceiling on one
/// they do — `task.list`'s pair of numbers, for its reasons.
const LINK_LIST_LIMIT: u64 = 100;
const LINK_LIST_MAX: u64 = 1000;

/// The columns every link row is read with, so the two readers cannot disagree
/// about what a link IS (`task_tags`' rule, one table over).
///
/// `created_by` is stored and deliberately not read: it exists so a later
/// promotion of an inferred edge (D160) can record that a machine proposed one,
/// and publishing a column whose only value is `'user'` would freeze a field
/// that says nothing yet.
const LINK_COLS: &str = "id, from_type, from_id, to_type, to_id, relation, metadata, created";

impl Engine {
    // ---- node references -----------------------------------------------------

    /// Resolve one node reference against `conn`'s view of the store.
    ///
    /// Takes `&Connection` so a mutation can pass its IMMEDIATE transaction and
    /// have the endpoint check share the write-locked snapshot of its own
    /// INSERT — `require_live_project`'s trick, and the reason a link cannot be
    /// written against a doc a concurrent `memory.remove` is deleting.
    ///
    /// The grammar, in the order it is tried:
    ///
    ///  * an integer, or a string that parses as one — a task short id;
    ///  * `<type>:<rest>`, where `<type>` is a [`NodeType`] and `<rest>` is a
    ///    uuid (or a short id for `task:`, or a name for `project:`);
    ///  * a bare uuid, probed against each kind in [`NodeType::ALL`] order.
    ///
    /// An unrecognized prefix or a string that is none of these is a
    /// `bad_request`; a well-formed reference naming nothing is a `not_found`
    /// quoting it back. That split is `require_uuid_shape`'s (engine/memory.rs)
    /// and exists for the same reason: exit 2 and exit 4 let a script branch
    /// without parsing JSON, so "you typed it wrong" and "it is not here" must
    /// not share one.
    fn parse_node_ref_on(&self, conn: &Connection, r: &Value) -> Result<Node, ApiError> {
        if let Some(n) = r.as_i64() {
            return Ok((NodeType::Task, self.task_by_short_on(conn, n)?.id));
        }
        let Some(s) = r.as_str() else {
            return Err(ApiError::bad_request(format!(
                "a node reference must be an integer or a string, but {} was given ({r}) — \
                 send a short id like 42, a uuid, or `<{}>:<id>`",
                crate::util::type_of(r),
                NodeType::accepted()
            )));
        };
        if let Ok(n) = s.parse::<i64>() {
            return Ok((NodeType::Task, self.task_by_short_on(conn, n)?.id));
        }
        if let Some((prefix, rest)) = s.split_once(':') {
            let Some(ty) = NodeType::parse(prefix) else {
                return Err(ApiError::bad_request(format!(
                    "`{prefix}` is not a node type in {s:?} — expected one of {}",
                    NodeType::accepted()
                )));
            };
            return self.resolve_node_of(conn, ty, rest, s);
        }
        if Uuid::parse_str(s).is_ok() {
            // A bare uuid says nothing about which table it came from, so the
            // store is asked. The order is NodeType::ALL's, and it is not a
            // tie-break rule dressed up as a lookup: a v4/v7 collision across
            // two of these tables is not a case anybody has to arbitrate.
            for ty in NodeType::ALL {
                if node_exists(conn, ty, s)? {
                    return Ok((ty, s.to_string()));
                }
            }
            return Err(ApiError::not_found(
                format!(
                    "no task, memory doc, annotation or project with id {s} — prefix the \
                     reference (`memory:{s}`) to say which was meant"
                ),
                Some(json!({ "ref": s })),
            ));
        }
        Err(ApiError::bad_request(format!(
            "`{s}` is not a node reference — expected a task short id like 42, a uuid, or \
             `<{}>:<id>`",
            NodeType::accepted()
        )))
    }

    /// The `<type>:<rest>` half of [`Engine::parse_node_ref_on`], split out so
    /// the grammar above reads as the three shapes it is.
    fn resolve_node_of(
        &self,
        conn: &Connection,
        ty: NodeType,
        rest: &str,
        whole: &str,
    ) -> Result<Node, ApiError> {
        let missing = || {
            ApiError::not_found(
                format!("no {} with reference {whole}", ty.as_str()),
                Some(json!({ "ref": whole })),
            )
        };
        match ty {
            // `task:42` as well as `task:<uuid>`: the prefix says which kind of
            // node, not which spelling, and refusing the short id here while
            // accepting a bare `42` would be a distinction nobody can predict.
            NodeType::Task => match rest.parse::<i64>() {
                Ok(n) => Ok((NodeType::Task, self.task_by_short_on(conn, n)?.id)),
                Err(_) => {
                    require_node_uuid(ty, rest, whole)?;
                    Ok((NodeType::Task, self.task_by_id_on(conn, rest)?.id))
                }
            },
            // A project is the one node with a human name of its own, and that
            // name is what every task stores, so `project:work` has to work.
            // The uuid is tried first: a project NAMED like a uuid would
            // otherwise shadow the row with that id.
            NodeType::Project => {
                if Uuid::parse_str(rest).is_ok() && node_exists(conn, ty, rest)? {
                    return Ok((ty, rest.to_string()));
                }
                let id: Option<String> = conn
                    .query_row(
                        "SELECT id FROM projects WHERE name = ?1",
                        params![rest],
                        |r| r.get(0),
                    )
                    .optional()?;
                id.map(|id| (ty, id)).ok_or_else(missing)
            }
            NodeType::Memory | NodeType::Annotation => {
                require_node_uuid(ty, rest, whole)?;
                match node_exists(conn, ty, rest)? {
                    true => Ok((ty, rest.to_string())),
                    false => Err(missing()),
                }
            }
        }
    }

    // ---- link.add ------------------------------------------------------------

    /// `link.add` — record an explicit edge between two nodes (D160). Params:
    /// `from`, `to` (node references), `relation`, `metadata?`, `expected_rev?`.
    ///
    /// **A duplicate is idempotent and says so.** Re-adding the same
    /// `(from, to, relation)` hands back the link that is already there with
    /// `created: false`, never a second row and never an error — the answer
    /// `dependency.add`'s `inserted` gives one table over, and for the same
    /// reason: a caller re-running a command after losing its scrollback has to
    /// be able to tell "this is new" from "this was already true".
    ///
    /// **Cycles are permitted**, unlike a dependency edge. "A supersedes B" and
    /// "B references A" are both statements about knowledge; nothing schedules
    /// work off a link, so there is no scheduler for a cycle to hang.
    ///
    /// **A self-link is refused.** An edge from a node to itself carries no
    /// information and every traversal would have to special-case it.
    ///
    /// `expected_rev` is `task.modify`'s optimistic-concurrency guard, applied
    /// to the `from` endpoint when that endpoint HAS a rev — a task or a doc.
    /// An annotation and a project carry none, so the guard is ignored rather
    /// than refused: a client that sends `expected_rev` uniformly must not have
    /// to know which endpoint kinds have a counter.
    pub fn link_add(&self, p: &Value) -> Result<Value, ApiError> {
        let from = link_endpoint(p, "from")?;
        let to = link_endpoint(p, "to")?;
        let relation = req_str(p, "relation")?;
        if !LINK_RELATIONS.contains(&relation.as_str()) {
            return Err(ApiError::bad_request(format!(
                "relation must be one of {} (got {relation:?})",
                LINK_RELATIONS.join(", ")
            )));
        }
        // The caller's own JSON, stored verbatim and never interpreted. A
        // non-object is refused rather than wrapped: the column's type must not
        // depend on who wrote the row.
        let metadata = match p.get("metadata") {
            None | Some(Value::Null) => None,
            Some(v) if v.is_object() => Some(v.clone()),
            Some(other) => {
                return Err(ApiError::bad_request(format!(
                    "`metadata` must be an object, but {} was given ({other}) — send {{ … }} or \
                     omit it",
                    crate::util::type_of(other)
                )))
            }
        };
        let expected_rev = opt_i64(p, "expected_rev")?;

        let ts = now();
        let tx = self.begin_mutation()?;
        let from = self.parse_node_ref_on(&tx, from)?;
        let to = self.parse_node_ref_on(&tx, to)?;
        if from == to {
            return Err(ApiError::bad_request(format!(
                "a node cannot link to itself ({})",
                node_id(&from)
            )));
        }
        if let Some(expected) = expected_rev {
            check_node_rev(&tx, &from, expected)?;
        }

        // The existence check and the INSERT share the write lock, so a
        // concurrent `link.add` of the same edge serializes against us and this
        // can neither report `created: true` for a row somebody else wrote nor
        // trip the UNIQUE constraint.
        let existing: Option<Value> = tx
            .query_row(
                &format!(
                    "SELECT {LINK_COLS} FROM links \
                     WHERE from_type = ?1 AND from_id = ?2 AND to_type = ?3 AND to_id = ?4 \
                       AND relation = ?5"
                ),
                params![from.0.as_str(), from.1, to.0.as_str(), to.1, relation],
                link_row,
            )
            .optional()?;
        if let Some(mut link) = existing {
            tx.commit()?;
            link["created"] = json!(false);
            return Ok(link);
        }

        let id = crate::clock::uuid_v7().to_string();
        tx.execute(
            "INSERT INTO links \
             (id, from_type, from_id, to_type, to_id, relation, metadata, created, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'user')",
            params![
                id,
                from.0.as_str(),
                from.1,
                to.0.as_str(),
                to.1,
                relation,
                metadata.as_ref().map(Value::to_string),
                ts,
            ],
        )?;
        insert_event(
            &tx,
            Entity::Link,
            &id,
            "link.add",
            &json!({ "from": node_id(&from), "to": node_id(&to), "relation": relation }),
        )?;
        tx.commit()?;

        Ok(json!({
            "id": id,
            "from": node_id(&from),
            "to": node_id(&to),
            "relation": relation,
            "metadata": metadata.unwrap_or(Value::Null),
            // `created_at` and not `created`: this response already answers
            // "did this call create it?" as a boolean, and one object cannot
            // carry two keys of the same name meaning different things.
            "created_at": ts,
            "created": true,
        }))
    }

    // ---- link.remove ---------------------------------------------------------

    /// `link.remove` — delete one link by its own id. Params: `id`.
    ///
    /// By the link's id and not by its endpoints: the endpoints plus the
    /// relation do identify it, but a caller who has just listed the links is
    /// holding the id, and naming three things where one will do is three
    /// chances to name the wrong edge.
    ///
    /// An unknown id is `not_found`, including the second removal of a link
    /// this call already took: there is nothing left to remove either way, and
    /// answering ok would be the unfalsifiable write D33 refuses — the reasoning
    /// `tag.remove` applies to a tag the task never had.
    pub fn link_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        let tx = self.begin_mutation()?;
        let removed = tx.execute("DELETE FROM links WHERE id = ?1", params![id])?;
        if removed == 0 {
            return Err(ApiError::not_found(
                format!(
                    "no link with id {id} — `link.list` names the links a node carries; nothing \
                     was removed."
                ),
                Some(json!({ "id": id })),
            ));
        }
        insert_event(&tx, Entity::Link, &id, "link.remove", &json!({}))?;
        tx.commit()?;
        Ok(json!({ "id": id, "removed": true }))
    }

    // ---- link.list -----------------------------------------------------------

    /// `link.list` — the links a node carries, newest first. Params: `ref?`
    /// (a node reference), `relation?`, `limit?`, `offset?`.
    ///
    /// **Both directions.** A node's neighbourhood is what points AT it as much
    /// as what it points at, and a caller holding one id should not have to ask
    /// twice and merge the answers — so `ref` matches either endpoint. An
    /// omitted `ref` lists the store's links, which is what makes a bare call
    /// the way to see whether a store holds any at all.
    ///
    /// Ordered by `id` DESC, which is newest first because the ids are UUIDv7:
    /// unlike `memory.list`, no column here moves after the insert (a link is
    /// written once and deleted, never updated), so the id is both the
    /// tie-break and the recency key. Paged in the `task.list`/`memory.list`
    /// shape (D70): `total` is what matched before the window, `next_offset` is
    /// null once nothing is left.
    pub fn link_list(&self, p: &Value) -> Result<Value, ApiError> {
        let relation = opt_str_nonempty(p, "relation")?;
        if let Some(r) = &relation {
            if !LINK_RELATIONS.contains(&r.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "relation must be one of {} (got {r:?})",
                    LINK_RELATIONS.join(", ")
                )));
            }
        }
        let offset = opt_u64(p, "offset")?.unwrap_or(0);
        let offset_i64 = i64::try_from(offset).map_err(|_| {
            ApiError::bad_request(format!(
                "`offset` must be at most {}, or omitted for the default",
                i64::MAX
            ))
        })?;
        // Clamped rather than refused, exactly as `task.list` clamps its own
        // page: an oversized `limit` is a caller asking for everything, and the
        // useful answer to that is the biggest page this method will serve.
        let limit = opt_u64(p, "limit")?
            .unwrap_or(LINK_LIST_LIMIT)
            .min(LINK_LIST_MAX) as i64;
        // Resolved against the live store, so `link.list {ref: 9999}` is a
        // `not_found` rather than an empty page — D27's collapse, where a typo
        // and a genuinely empty neighbourhood read identically.
        let node = match p.get("ref").filter(|v| !v.is_null()) {
            Some(r) => Some(self.parse_node_ref_on(&self.conn, r)?),
            None => None,
        };

        let mut conds: Vec<&str> = Vec::new();
        if node.is_some() {
            conds.push(
                "((from_type = :type AND from_id = :id) OR (to_type = :type AND to_id = :id))",
            );
        }
        if relation.is_some() {
            conds.push("relation = :relation");
        }
        let where_clause = match conds.is_empty() {
            true => String::new(),
            false => format!("WHERE {}", conds.join(" AND ")),
        };

        let (node_type, node_uuid) = match &node {
            Some((ty, id)) => (ty.as_str(), id.as_str()),
            None => ("", ""),
        };
        let mut named: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if node.is_some() {
            named.push((":type", &node_type));
            named.push((":id", &node_uuid));
        }
        if let Some(r) = &relation {
            named.push((":relation", r));
        }

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM links {where_clause}"),
            named.as_slice(),
            |r| r.get(0),
        )?;

        let mut row_params = named.clone();
        row_params.push((":limit", &limit));
        row_params.push((":offset", &offset_i64));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {LINK_COLS} FROM links {where_clause} \
             ORDER BY id DESC LIMIT :limit OFFSET :offset"
        ))?;
        let links: Vec<Value> = stmt
            .query_map(row_params.as_slice(), link_row)?
            .collect::<Result<_, _>>()?;

        let next_offset = match offset + links.len() as u64 {
            reached if !links.is_empty() && (reached as i64) < total => json!(reached),
            _ => Value::Null,
        };
        Ok(json!({
            "count": links.len(),
            "total": total,
            "next_offset": next_offset,
            "links": links,
        }))
    }
}

/// Read a `link.list` / `link.add` row off a statement selecting [`LINK_COLS`].
///
/// One function for both readers, so the duplicate `link.add` hands back and
/// the rows `link.list` pages cannot come to disagree about what a link row is.
fn link_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let metadata: Option<String> = r.get(6)?;
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "from": format!("{}:{}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
        "to": format!("{}:{}", r.get::<_, String>(3)?, r.get::<_, String>(4)?),
        "relation": r.get::<_, String>(5)?,
        "metadata": metadata
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null),
        "created_at": r.get::<_, String>(7)?,
    }))
}

/// The `from` / `to` param of `link.add`, present and not null.
///
/// Its own function rather than `req_str`, because a node reference is legally
/// an integer as well as a string — the same reason `dependency.add` reaches
/// for `p.get("depends_on")` instead. Deliberately NOT named `*ref_param`:
/// `dispatch`'s accepted-key guard matches a helper call by substring, and a
/// name ending in `ref_param(` would make this handler look like it reads
/// `ref`.
fn link_endpoint<'a>(p: &'a Value, key: &str) -> Result<&'a Value, ApiError> {
    p.get(key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| ApiError::bad_request(format!("missing required field: {key}")))
}

/// Does a row of this kind exist with this id?
///
/// The table and column are literals chosen by the match, never caller text.
fn node_exists(conn: &Connection, ty: NodeType, id: &str) -> Result<bool, ApiError> {
    let sql = match ty {
        NodeType::Task => "SELECT 1 FROM tasks WHERE id = ?1",
        NodeType::Memory => "SELECT 1 FROM docs WHERE id = ?1",
        NodeType::Annotation => "SELECT 1 FROM annotations WHERE id = ?1",
        NodeType::Project => "SELECT 1 FROM projects WHERE id = ?1",
    };
    Ok(conn
        .query_row(sql, params![id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// A node id that can only ever be a uuid must LOOK like one before the store
/// is asked — `require_uuid_shape`'s rule (engine/memory.rs), extended to the
/// three prefixes that mint nothing else.
fn require_node_uuid(ty: NodeType, id: &str, whole: &str) -> Result<(), ApiError> {
    match Uuid::parse_str(id).is_ok() {
        true => Ok(()),
        false => Err(ApiError::bad_request(format!(
            "{whole} is not a {} reference: {id} is not a UUID",
            ty.as_str()
        ))),
    }
}

/// `link.add`'s `expected_rev` guard, on the endpoints that carry a `rev`.
///
/// An annotation and a project have no revision counter, so there is nothing to
/// compare and the guard passes — stated here once rather than at the call
/// site, so the two halves of the rule cannot drift apart.
fn check_node_rev(conn: &Connection, node: &Node, expected: i64) -> Result<(), ApiError> {
    let (what, sql) = match node.0 {
        NodeType::Task => ("task", "SELECT rev FROM tasks WHERE id = ?1"),
        NodeType::Memory => ("doc", "SELECT rev FROM docs WHERE id = ?1"),
        NodeType::Annotation | NodeType::Project => return Ok(()),
    };
    let current: i64 = conn.query_row(sql, params![node.1], |r| r.get(0))?;
    if current == expected {
        return Ok(());
    }
    Err(ApiError::new(
        crate::ErrorCode::Conflict,
        format!(
            "expected_rev {expected} but {what} {} is at rev {current}: re-read it and retry \
             with expected_rev {current}",
            node_id(node)
        ),
        Some(json!({ "expected": expected, "current": current, "node": node_id(node) })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`NodeType::ALL`] carries [`Entity::ALL`]'s silent-failure mode: a
    /// dropped entry is a node kind a link can hold and no reference can name,
    /// and a duplicate satisfies the declared length while covering one kind
    /// less than it looks like it does.
    #[test]
    fn node_type_all_lists_every_variant_exactly_once_and_round_trips() {
        let mut seen: Vec<&str> = NodeType::ALL.iter().map(|t| t.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "NodeType::ALL contains a duplicate");
        assert_eq!(before, 4, "NodeType::ALL must list every variant");
        for ty in NodeType::ALL {
            assert_eq!(NodeType::parse(ty.as_str()), Some(ty));
            assert!(NodeType::accepted().contains(ty.as_str()));
        }
        assert_eq!(NodeType::parse("doc"), None, "a near-miss must not parse");
        assert_eq!(NodeType::parse(""), None);
    }
}
