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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// `graph.query`'s bounds (D160): the default when the caller names none, and
/// the ceiling on one they do.
///
/// Two hops is the neighbourhood a person can read in one picture, four the
/// most this method will walk at all; 250 nodes and 750 edges render, 1,000 and
/// 5,000 are what a client that means it may ask for. The ceilings are refused
/// rather than clamped — see [`graph_bound`].
const GRAPH_DEPTH_DEFAULT: i64 = 2;
const GRAPH_DEPTH_MAX: i64 = 4;
const GRAPH_NODES_DEFAULT: i64 = 250;
const GRAPH_NODES_MAX: i64 = 1000;
const GRAPH_EDGES_DEFAULT: i64 = 750;
const GRAPH_EDGES_MAX: i64 = 5000;

/// How many `search_match` hits one projection may pull in. Bounded per CALL
/// and not per node, because the hits come from one FTS query run on the root.
const GRAPH_SEARCH_HITS: u64 = 10;

/// An annotation's label is its first line, cut to this many characters with an
/// ellipsis standing for the rest — a note is prose and a graph node is a box.
const GRAPH_LABEL_CHARS: usize = 80;

/// How many ids one `IN (…)` list carries.
///
/// SQLite's bound-parameter limit has been 999 on builds older than 3.32, and
/// one expansion here is a whole project's membership — so every batched lookup
/// runs in chunks rather than betting on the library's ceiling.
const GRAPH_SQL_CHUNK: usize = 500;

/// The relations read out of the store's own tables, named for what they assert
/// rather than for the table — the `source` on each edge names the table.
const GRAPH_STRUCTURAL_RELATIONS: [&str; 3] =
    ["depends_on", "has_annotation", "belongs_to_project"];

/// The relations computed per call and never written down (D160).
const GRAPH_INFERRED_RELATIONS: [&str; 2] = ["search_match", "shared_tag"];

/// One node of a projection, as the walk carries it: the resolved node, the hop
/// it was reached at, and everything the answer or a filter needs — read once,
/// in a batch, so no later step has to go back to the store for a label.
#[derive(Clone)]
struct GraphNode {
    node: Node,
    depth: i64,
    label: String,
    summary: Option<String>,
    project: Option<String>,
    status: Option<String>,
    modified: Option<String>,
    short_id: Option<i64>,
    /// The owning task's node id, on an annotation and nothing else.
    task: Option<String>,
    /// A task's tags, for the `tags` filter and for `shared_tag`. Never
    /// published: a node carries its label, and the tags are on the task.
    tags: Vec<String>,
}

impl GraphNode {
    /// The total order every node list in this method is sorted by: depth, then
    /// kind in [`NodeType::ALL`]'s order, then id. Deterministic for equal
    /// inputs is a contract here (D160), and it is what makes `max_nodes` a
    /// prefix of a stable list rather than a sample.
    fn order(&self) -> (i64, NodeType, &str) {
        (self.depth, self.node.0, self.node.1.as_str())
    }
}

/// One edge of a projection.
///
/// `kind` is `"structural"` for an edge read out of a table and `"inferred"`
/// for one computed here, and `confidence` is `None` on the first — a stored
/// fact has no confidence to report, and a number on it would invite a reader
/// to weigh the two together.
struct GraphEdge {
    id: String,
    from: String,
    to: String,
    relation: String,
    kind: &'static str,
    confidence: Option<f64>,
    source: String,
}

/// The node and relation filters one `graph.query` applies as it walks.
struct GraphFilters {
    types: Vec<NodeType>,
    relations: Vec<String>,
    project: Option<String>,
    status: Option<String>,
    tags: Vec<String>,
    after: Option<Timestamp>,
    before: Option<Timestamp>,
}

impl GraphFilters {
    /// Does this node belong in the projection?
    ///
    /// Each filter narrows the kind of node it is ABOUT and leaves the rest
    /// alone. A note has no status, a project carries no tags and neither is
    /// filed under a project, so applying those three to them would answer an
    /// empty graph for a live store — the collapse D27 names, where a filter
    /// that cannot be satisfied and a neighbourhood that is really empty read
    /// identically.
    fn keeps(&self, n: &GraphNode) -> bool {
        if !self.types.is_empty() && !self.types.contains(&n.node.0) {
            return false;
        }
        if let Some(want) = &self.project {
            if matches!(n.node.0, NodeType::Task | NodeType::Memory)
                && n.project.as_deref() != Some(want.as_str())
            {
                return false;
            }
        }
        if let Some(want) = &self.status {
            if n.node.0 == NodeType::Task && n.status.as_deref() != Some(want.as_str()) {
                return false;
            }
        }
        if !self.tags.is_empty()
            && n.node.0 == NodeType::Task
            && !n.tags.iter().any(|t| self.tags.contains(t))
        {
            return false;
        }
        // A node with no date at all — a project — is outside the window's
        // subject rather than outside the window.
        if let Some(at) = n.modified.as_deref().and_then(parse_ts) {
            if self.after.is_some_and(|a| at < a) || self.before.is_some_and(|b| at > b) {
                return false;
            }
        }
        true
    }

    /// May the walk cross this relation? An omitted `relation_types` crosses
    /// every one, which is what makes the filter additive rather than a list a
    /// caller has to keep complete.
    fn allows(&self, relation: &str) -> bool {
        self.relations.is_empty() || self.relations.iter().any(|r| r == relation)
    }
}

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
        let from = required_node_ref(p, "from")?;
        let to = required_node_ref(p, "to")?;
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

    // ---- graph.query ---------------------------------------------------------

    /// `graph.query` — one bounded, deterministically ordered projection of the
    /// knowledge graph around `root` (D160). Params: `root`, `depth?`,
    /// `node_types?`, `relation_types?`, `project?`, `status?`, `tags?`,
    /// `modified_after?`, `modified_before?`, `include_inferred?`, `max_nodes?`,
    /// `max_edges?`.
    ///
    /// **Bounded in three different places, and each one is deliberate.**
    /// `depth` bounds how far the walk goes and is checked before a row is
    /// read. The filters bound the frontier AS it expands — a node they drop is
    /// neither returned nor expanded — so `node_types: ["task"]` never loads a
    /// document, and the cost of a narrow question is small rather than merely
    /// the answer. `max_nodes` is applied while the frontier grows, not only at
    /// the end: nodes are ordered by (depth, type, id), so a node discovered
    /// past the cap can never rise into the answer and neither can anything
    /// only that node would have reached — which is what keeps a depth-4 query
    /// out of a ten-thousand-task project. `max_edges` is applied last, after
    /// the edges whose endpoints did not survive are gone, because an edge to a
    /// node nobody kept is not an edge anybody can draw.
    ///
    /// **The root survives its own filters.** It is the position the caller
    /// named, and answering `nodes: []` to `graph.query {root: 42, status:
    /// "done"}` would be a projection with nothing to project from.
    ///
    /// **Nothing inferred is stored.** `include_inferred` adds `search_match`
    /// and `shared_tag` edges computed for this call only, each carrying a
    /// `confidence` and the `source` that produced it, so a derived edge can
    /// never be mistaken for one the store was told about.
    pub fn graph_query(&self, p: &Value) -> Result<Value, ApiError> {
        let depth = graph_bound(p, "depth", GRAPH_DEPTH_DEFAULT, 0, GRAPH_DEPTH_MAX)?;
        let max_nodes =
            graph_bound(p, "max_nodes", GRAPH_NODES_DEFAULT, 1, GRAPH_NODES_MAX)? as usize;
        let max_edges =
            graph_bound(p, "max_edges", GRAPH_EDGES_DEFAULT, 1, GRAPH_EDGES_MAX)? as usize;
        let include_inferred = opt_bool(p, "include_inferred")?.unwrap_or(false);
        let filters = graph_filters(p)?;
        // Resolved last of the parameters, so a malformed `depth` is answered
        // without a store read, and first of the work, so a missing root is
        // `not_found` before anything is walked.
        let root = self.parse_node_ref_on(&self.conn, required_node_ref(p, "root")?)?;

        let mut kept = self.load_graph_nodes(std::slice::from_ref(&root), 0)?;
        let root_node = kept
            .first()
            .cloned()
            .ok_or_else(|| ApiError::internal(format!("{} resolved to no row", node_id(&root))))?;
        let mut seen: HashSet<Node> = HashSet::new();
        seen.insert(root.clone());
        // Keyed by edge id so the same dependency, found once from each end,
        // is one edge rather than two.
        let mut edges: HashMap<String, GraphEdge> = HashMap::new();
        let mut omitted_nodes = 0usize;

        for hop in 0..depth {
            // Cloned rather than borrowed: the loop pushes the level it is
            // discovering onto the same vector it is reading the frontier from.
            let frontier: Vec<GraphNode> =
                kept.iter().filter(|n| n.depth == hop).cloned().collect();
            if frontier.is_empty() {
                break;
            }
            let (found, crossed) = self.expand_graph(&frontier, &filters)?;
            for edge in crossed {
                edges.entry(edge.id.clone()).or_insert(edge);
            }
            let mut fresh: Vec<Node> = found
                .into_iter()
                .filter(|n| seen.insert(n.clone()))
                .collect();
            // (type, id) here and (depth, type, id) overall: every node of this
            // level shares one depth, so sorting the level is sorting the whole
            // list, and the cap below can therefore be a prefix rule.
            fresh.sort();
            for node in self.load_graph_nodes(&fresh, hop + 1)? {
                if !filters.keeps(&node) {
                    continue;
                }
                match kept.len() < max_nodes {
                    true => kept.push(node),
                    false => omitted_nodes += 1,
                }
            }
        }

        let mut inferred: Vec<GraphEdge> = Vec::new();
        if include_inferred && filters.allows("search_match") {
            let (hits, matched) = self.graph_search_match(&root_node, &filters, &seen)?;
            kept.extend(hits);
            inferred.extend(matched);
        }

        kept.sort_by(|a, b| a.order().cmp(&b.order()));
        if kept.len() > max_nodes {
            omitted_nodes += kept.len() - max_nodes;
            kept.truncate(max_nodes);
        }
        let kept_ids: HashSet<String> = kept.iter().map(|n| node_id(&n.node)).collect();

        // After the node cut, so a pair is only drawn between two tasks that
        // are really in the answer.
        if include_inferred && filters.allows("shared_tag") {
            inferred.extend(graph_shared_tags(&kept));
        }

        let mut drawn: Vec<GraphEdge> = edges
            .into_values()
            .chain(inferred)
            .filter(|e| kept_ids.contains(&e.from) && kept_ids.contains(&e.to))
            .collect();
        drawn.sort_by(|a, b| (&a.relation, &a.from, &a.to).cmp(&(&b.relation, &b.from, &b.to)));
        let omitted_edges = drawn.len().saturating_sub(max_edges);
        drawn.truncate(max_edges);

        Ok(json!({
            "root": node_id(&root),
            "depth": depth,
            "nodes": kept.iter().map(graph_node_json).collect::<Vec<_>>(),
            "edges": drawn.iter().map(graph_edge_json).collect::<Vec<_>>(),
            "node_count": kept.len(),
            "edge_count": drawn.len(),
            "truncated": omitted_nodes > 0 || omitted_edges > 0,
            "omitted_nodes": omitted_nodes,
            "omitted_edges": omitted_edges,
            "include_inferred": include_inferred,
        }))
    }

    /// One BFS level: every structural edge touching `frontier`, and every node
    /// at the other end of one.
    ///
    /// Batched by kind — one statement per table per chunk of ids, never one
    /// per node. A per-node query is the failure this shape exists to avoid,
    /// and it is the thing the 1,200-task test in `tests/graph.rs` measures.
    fn expand_graph(
        &self,
        frontier: &[GraphNode],
        filters: &GraphFilters,
    ) -> Result<(Vec<Node>, Vec<GraphEdge>), ApiError> {
        let ids_of = |ty: NodeType| -> Vec<String> {
            frontier
                .iter()
                .filter(|n| n.node.0 == ty)
                .map(|n| n.node.1.clone())
                .collect()
        };
        let tasks = ids_of(NodeType::Task);
        let notes = ids_of(NodeType::Annotation);
        let mut found: Vec<Node> = Vec::new();
        let mut edges: Vec<GraphEdge> = Vec::new();

        // `depends_on`, both ways. The edge always points from the dependent to
        // the blocker, whichever end of it the frontier is standing on: an edge
        // that flipped direction depending on where the walk started would make
        // two projections of one store disagree about which task waits.
        if filters.allows("depends_on") && !tasks.is_empty() {
            for chunk in tasks.chunks(GRAPH_SQL_CHUNK) {
                let ph = placeholders(chunk.len());
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT task_id, depends_on_id FROM dependencies \
                     WHERE task_id IN ({ph}) OR depends_on_id IN ({ph})"
                ))?;
                let binds = chunk.iter().chain(chunk.iter());
                let rows = stmt.query_map(rusqlite::params_from_iter(binds), |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (dependent, blocker) = row?;
                    let from = (NodeType::Task, dependent);
                    let to = (NodeType::Task, blocker);
                    edges.push(structural_edge(
                        format!("dep:{}:{}", node_id(&from), node_id(&to)),
                        &from,
                        &to,
                        "depends_on",
                        "dependencies",
                    ));
                    found.push(from);
                    found.push(to);
                }
            }
        }

        // `has_annotation`, both ways: a task reaches its live notes, and a note
        // reaches the task it is on. A tombstone (D113) is skipped — its body is
        // gone, so the node would be a label nobody can read.
        if filters.allows("has_annotation") {
            for (ids, column) in [(&tasks, "task_id"), (&notes, "id")] {
                for chunk in ids.chunks(GRAPH_SQL_CHUNK) {
                    let ph = placeholders(chunk.len());
                    let mut stmt = self.conn.prepare(&format!(
                        "SELECT id, task_id FROM annotations \
                         WHERE removed IS NULL AND {column} IN ({ph})"
                    ))?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?;
                    for row in rows {
                        let (note, task) = row?;
                        let from = (NodeType::Task, task);
                        let to = (NodeType::Annotation, note);
                        edges.push(structural_edge(
                            format!("ann:{}", to.1),
                            &from,
                            &to,
                            "has_annotation",
                            "annotations",
                        ));
                        found.push(from);
                        found.push(to);
                    }
                }
            }
        }

        if filters.allows("belongs_to_project") {
            self.expand_projects(frontier, &mut found, &mut edges)?;
        }

        // Explicit links, in whichever direction they were written. Skipped
        // entirely when `relation_types` names none of the five, so a caller
        // asking only about dependencies does not pay for the links table.
        if LINK_RELATIONS.iter().any(|r| filters.allows(r)) {
            for ty in NodeType::ALL {
                let ids = ids_of(ty);
                for chunk in ids.chunks(GRAPH_SQL_CHUNK) {
                    let ph = placeholders(chunk.len());
                    let mut stmt = self.conn.prepare(&format!(
                        "SELECT id, from_type, from_id, to_type, to_id, relation FROM links \
                         WHERE (from_type = ? AND from_id IN ({ph})) \
                            OR (to_type = ? AND to_id IN ({ph}))"
                    ))?;
                    let name = ty.as_str();
                    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::new();
                    for _ in 0..2 {
                        binds.push(&name);
                        binds.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
                    }
                    let rows = stmt.query_map(binds.as_slice(), |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                        ))
                    })?;
                    for row in rows {
                        let (id, from_type, from_id, to_type, to_id, relation) = row?;
                        if !filters.allows(&relation) {
                            continue;
                        }
                        let (Some(ft), Some(tt)) =
                            (NodeType::parse(&from_type), NodeType::parse(&to_type))
                        else {
                            continue;
                        };
                        let from = (ft, from_id);
                        let to = (tt, to_id);
                        edges.push(structural_edge(
                            format!("link:{id}"),
                            &from,
                            &to,
                            &relation,
                            "links",
                        ));
                        found.push(from);
                        found.push(to);
                    }
                }
            }
        }

        Ok((found, edges))
    }

    /// The `belongs_to_project` half of [`Engine::expand_graph`], both
    /// directions, split out because it is the only expansion that reads two
    /// tables and joins them on a NAME rather than an id (a task stores its
    /// project's name, not its uuid — see `Task::project`).
    fn expand_projects(
        &self,
        frontier: &[GraphNode],
        found: &mut Vec<Node>,
        edges: &mut Vec<GraphEdge>,
    ) -> Result<(), ApiError> {
        let filed: Vec<&GraphNode> = frontier
            .iter()
            .filter(|n| matches!(n.node.0, NodeType::Task | NodeType::Memory))
            .filter(|n| n.project.is_some())
            .collect();
        let mut names: Vec<String> = filed.iter().filter_map(|n| n.project.clone()).collect();
        names.sort();
        names.dedup();
        let mut by_name: HashMap<String, String> = HashMap::new();
        for chunk in names.chunks(GRAPH_SQL_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id, name FROM projects WHERE name IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, name) = row?;
                by_name.insert(name, id);
            }
        }
        for node in filed {
            let Some(id) = node.project.as_ref().and_then(|n| by_name.get(n)) else {
                continue;
            };
            let to = (NodeType::Project, id.clone());
            edges.push(structural_edge(
                format!("member:{}", node_id(&node.node)),
                &node.node,
                &to,
                "belongs_to_project",
                graph_member_source(node.node.0),
            ));
            found.push(to);
        }

        // The other way: a project node reaches everything filed under it. This
        // is the one expansion whose fan-out is a whole table, which is exactly
        // why `max_nodes` is enforced as the frontier grows rather than at the
        // end — the rows come back, but nothing past the cap is ever expanded.
        let mut owners: HashMap<String, String> = HashMap::new();
        for node in frontier.iter().filter(|n| n.node.0 == NodeType::Project) {
            owners.insert(node.label.clone(), node.node.1.clone());
        }
        if owners.is_empty() {
            return Ok(());
        }
        let labels: Vec<String> = owners.keys().cloned().collect();
        for (table, ty) in [("tasks", NodeType::Task), ("docs", NodeType::Memory)] {
            for chunk in labels.chunks(GRAPH_SQL_CHUNK) {
                // `table` is one of two literals chosen by this loop, never
                // caller text; the project names are bound.
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT id, project FROM {table} WHERE project IN ({})",
                    placeholders(chunk.len())
                ))?;
                let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (id, project) = row?;
                    let Some(owner) = owners.get(&project) else {
                        continue;
                    };
                    let from = (ty, id);
                    let to = (NodeType::Project, owner.clone());
                    edges.push(structural_edge(
                        format!("member:{}", node_id(&from)),
                        &from,
                        &to,
                        "belongs_to_project",
                        graph_member_source(ty),
                    ));
                    found.push(from);
                }
            }
        }
        Ok(())
    }

    /// Read the rows behind `want` and build the node objects, one statement
    /// per kind per chunk. Returns them in (type, id) order, and silently drops
    /// a reference whose row is gone — a removed annotation is discovered by an
    /// expansion that ran before it was scrubbed.
    fn load_graph_nodes(&self, want: &[Node], depth: i64) -> Result<Vec<GraphNode>, ApiError> {
        let ids_of = |ty: NodeType| -> Vec<String> {
            want.iter()
                .filter(|(t, _)| *t == ty)
                .map(|(_, id)| id.clone())
                .collect()
        };
        let mut out: Vec<GraphNode> = Vec::new();

        let tasks = ids_of(NodeType::Task);
        for chunk in tasks.chunks(GRAPH_SQL_CHUNK) {
            // `map_task_row` and not a column list of this method's own: it is
            // what applies `effective_status`, so a task parked behind a future
            // `wait` reads `backlog` here exactly as it does in `task.list`.
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {TASK_COLS} FROM tasks WHERE id IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), map_task_row)?;
            for task in rows {
                let task = task?;
                let status = task
                    .status_raw
                    .clone()
                    .unwrap_or_else(|| task.status.as_str().to_string());
                let summary = match task.priority {
                    Some(priority) => {
                        format!("#{} · {status} · {}", task.short_id, priority.as_str())
                    }
                    None => format!("#{} · {status}", task.short_id),
                };
                out.push(GraphNode {
                    node: (NodeType::Task, task.id),
                    depth,
                    label: task.title,
                    summary: Some(summary),
                    project: task.project,
                    status: Some(status),
                    modified: Some(task.modified),
                    short_id: Some(task.short_id),
                    task: None,
                    tags: Vec::new(),
                });
            }
        }
        // One statement for every task's tags, not one per task: the tags are
        // read for the `tags` filter and again for `shared_tag`, and a per-node
        // query here is what the scale test would catch.
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        for chunk in tasks.chunks(GRAPH_SQL_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT tt.task_id, g.name FROM task_tags tt JOIN tags g ON g.id = tt.tag_id \
                 WHERE tt.task_id IN ({}) ORDER BY g.name",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (task, tag) = row?;
                tags.entry(task).or_default().push(tag);
            }
        }
        for node in out.iter_mut() {
            if let Some(names) = tags.remove(&node.node.1) {
                node.tags = names;
            }
        }

        for chunk in ids_of(NodeType::Memory).chunks(GRAPH_SQL_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id, title, source, project, modified FROM docs WHERE id IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok(GraphNode {
                    node: (NodeType::Memory, r.get(0)?),
                    depth,
                    label: r.get(1)?,
                    // A document's summary is where it came from — the one line
                    // that tells an imported ADR from a note somebody typed.
                    summary: r.get(2)?,
                    project: r.get(3)?,
                    status: None,
                    modified: r.get(4)?,
                    short_id: None,
                    task: None,
                    tags: Vec::new(),
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }

        for chunk in ids_of(NodeType::Annotation).chunks(GRAPH_SQL_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id, task_id, body, created FROM annotations \
                 WHERE removed IS NULL AND id IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                let owner = node_id(&(NodeType::Task, r.get::<_, String>(1)?));
                let body: String = r.get(2)?;
                Ok(GraphNode {
                    node: (NodeType::Annotation, r.get(0)?),
                    depth,
                    label: graph_first_line(&body),
                    summary: Some(owner.clone()),
                    project: None,
                    status: None,
                    // A note is never edited, so `created` is the only date it
                    // has and the one the date window reads.
                    modified: r.get(3)?,
                    short_id: None,
                    task: Some(owner),
                    tags: Vec::new(),
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }

        for chunk in ids_of(NodeType::Project).chunks(GRAPH_SQL_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id, name, description FROM projects WHERE id IN ({})",
                placeholders(chunk.len())
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok(GraphNode {
                    node: (NodeType::Project, r.get(0)?),
                    depth,
                    label: r.get(1)?,
                    summary: r.get(2)?,
                    // Null, not its own name: `project` says what a node is
                    // filed under, and a project is not filed under itself.
                    project: None,
                    status: None,
                    // A project row carries no `modified` column at all, which
                    // is why the date window cannot exclude one.
                    modified: None,
                    short_id: None,
                    task: None,
                    tags: Vec::new(),
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }

        out.sort_by(|a, b| (a.node.0, &a.node.1).cmp(&(b.node.0, &b.node.1)));
        Ok(out)
    }

    /// The `search_match` half of the inferred layer: the documents and notes
    /// the ROOT's own title finds.
    ///
    /// From the root and nowhere else, because each one is an FTS query and a
    /// projection of 250 nodes would be 250 of them. `memory.search` is CALLED
    /// rather than copied — the ranking, the phrase escaping D41 needs and the
    /// doc/annotation union are one implementation, so a change to how tasqx
    /// searches cannot leave the graph searching the old way.
    fn graph_search_match(
        &self,
        root: &GraphNode,
        filters: &GraphFilters,
        seen: &HashSet<Node>,
    ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), ApiError> {
        // A title of punctuation and nothing else has no searchable word, and
        // `memory.search` would refuse it. Nothing is inferred instead.
        let words: Vec<&str> = root
            .label
            .split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect();
        if words.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let query = words.join(" ");
        let answer = self.memory_search(&json!({ "query": query, "limit": GRAPH_SEARCH_HITS }))?;

        let mut ranks: HashMap<Node, f64> = HashMap::new();
        let mut wanted: Vec<Node> = Vec::new();
        for hit in answer["hits"].as_array().into_iter().flatten() {
            let (Some(kind), Some(id), Some(rank)) = (
                hit["kind"].as_str(),
                hit["id"].as_str(),
                hit["rank"].as_f64(),
            ) else {
                continue;
            };
            let ty = match kind {
                "doc" => NodeType::Memory,
                "annotation" => NodeType::Annotation,
                _ => continue,
            };
            let node = (ty, id.to_string());
            // Already in the projection: the structural walk got there first,
            // and an edge to a node the graph already holds would say only that
            // the title matches itself.
            if seen.contains(&node) || ranks.contains_key(&node) {
                continue;
            }
            ranks.insert(node.clone(), rank);
            wanted.push(node);
        }
        let nodes: Vec<GraphNode> = self
            .load_graph_nodes(&wanted, 1)?
            .into_iter()
            .filter(|n| filters.keeps(n))
            .collect();

        // bm25 is a score where LOWER is better and the scale depends on the
        // corpus, so it is not a confidence. Normalised across the hits this
        // call is emitting — best 1.0, worst 0.0 — which makes the number a
        // ranking within one answer and says so in `source`.
        let scores: Vec<f64> = nodes
            .iter()
            .filter_map(|n| ranks.get(&n.node))
            .copied()
            .collect();
        let best = scores.iter().copied().fold(f64::INFINITY, f64::min);
        let worst = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let span = worst - best;
        let from = node_id(&root.node);
        let edges = nodes
            .iter()
            .map(|n| {
                let score = ranks.get(&n.node).copied().unwrap_or(best);
                let confidence = match span > 0.0 {
                    true => (worst - score) / span,
                    false => 1.0,
                };
                let to = node_id(&n.node);
                GraphEdge {
                    id: format!("search:{from}:{to}"),
                    from: from.clone(),
                    to,
                    relation: "search_match".to_string(),
                    kind: "inferred",
                    confidence: Some(confidence),
                    source: format!("memory.search: {query}"),
                }
            })
            .collect();
        Ok((nodes, edges))
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

/// One required node-reference param, present and not null — `link.add`'s two
/// endpoints and `graph.query`'s `root`.
///
/// Its own function rather than `req_str`, because a node reference is legally
/// an integer as well as a string — the same reason `dependency.add` reaches
/// for `p.get("depends_on")` instead. Deliberately NOT named `*ref_param`:
/// `dispatch`'s accepted-key guard matches a helper call by substring, and a
/// name ending in `ref_param(` would make its callers look like they read
/// `ref`.
fn required_node_ref<'a>(p: &'a Value, key: &str) -> Result<&'a Value, ApiError> {
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

/// Read one of `graph.query`'s integer bounds, defaulted and range-checked.
///
/// Refused by name rather than clamped: a caller who asked for depth 5 wants
/// five, and quietly serving two is an answer they cannot tell apart from the
/// graph really ending there. `link.list` clamps its `limit` for the opposite
/// reason — an oversized page is a caller asking for everything, and the useful
/// answer to that is the biggest page there is.
fn graph_bound(p: &Value, key: &str, default: i64, low: i64, high: i64) -> Result<i64, ApiError> {
    let Some(n) = opt_i64(p, key)? else {
        return Ok(default);
    };
    match (low..=high).contains(&n) {
        true => Ok(n),
        false => Err(ApiError::bad_request(format!(
            "`{key}` must be between {low} and {high}, but {n} was given — omit it for the \
             default of {default}"
        ))),
    }
}

/// One RFC 3339 instant out of the params, or `None`.
///
/// RFC 3339 and not the `yesterday`/`2026-09-01` grammar the CLI's date fields
/// take: this is a machine-facing window on a projection, the two ends have to
/// mean the same instant in every timezone the caller might be in, and a
/// relative word would make the same request answer differently tomorrow.
fn graph_instant(p: &Value, key: &str) -> Result<Option<Timestamp>, ApiError> {
    let Some(s) = opt_str_nonempty(p, key)? else {
        return Ok(None);
    };
    parse_ts(&s).map(Some).ok_or_else(|| {
        ApiError::bad_request(format!(
            "`{key}` must be an RFC 3339 instant like 2026-09-01T00:00:00Z, but {s:?} was given"
        ))
    })
}

/// Every relation a `relation_types` filter may name: the structural three, the
/// five explicit link relations, and the two inferred ones.
fn graph_relations_accepted() -> String {
    GRAPH_STRUCTURAL_RELATIONS
        .iter()
        .chain(LINK_RELATIONS.iter())
        .chain(GRAPH_INFERRED_RELATIONS.iter())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse `graph.query`'s filter params, refusing every unknown name by naming
/// the accepted set (D34).
///
/// Its own function and not a block inside the handler, because `dispatch`'s
/// accepted-key guard follows a helper that is handed the whole `p` — so these
/// seven keys are read here and declared in `PARAMS` once.
fn graph_filters(p: &Value) -> Result<GraphFilters, ApiError> {
    let mut types = Vec::new();
    for name in opt_str_array(p, "node_types")? {
        let Some(ty) = NodeType::parse(&name) else {
            return Err(ApiError::bad_request(format!(
                "`{name}` is not a node type — expected one of {}",
                NodeType::accepted()
            )));
        };
        types.push(ty);
    }
    let mut relations = Vec::new();
    for name in opt_str_array(p, "relation_types")? {
        let known = GRAPH_STRUCTURAL_RELATIONS.contains(&name.as_str())
            || LINK_RELATIONS.contains(&name.as_str())
            || GRAPH_INFERRED_RELATIONS.contains(&name.as_str());
        if !known {
            return Err(ApiError::bad_request(format!(
                "`{name}` is not a graph relation — expected one of {}",
                graph_relations_accepted()
            )));
        }
        relations.push(name);
    }
    let project = opt_str_nonempty(p, "project")?;
    let status = opt_str_nonempty(p, "status")?;
    if let Some(s) = &status {
        if Status::parse(s).is_none() {
            return Err(ApiError::bad_request(format!(
                "`{s}` is not a status — expected one of {}",
                Status::accepted()
            )));
        }
    }
    Ok(GraphFilters {
        types,
        relations,
        project,
        status,
        tags: opt_str_array(p, "tags")?,
        after: graph_instant(p, "modified_after")?,
        before: graph_instant(p, "modified_before")?,
    })
}

/// `?,?,?` for an `IN (…)` list of `n` bound values.
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// One edge read out of a table, with the table as its provenance.
fn structural_edge(
    id: String,
    from: &Node,
    to: &Node,
    relation: &str,
    source: &'static str,
) -> GraphEdge {
    GraphEdge {
        id,
        from: node_id(from),
        to: node_id(to),
        relation: relation.to_string(),
        kind: "structural",
        confidence: None,
        source: source.to_string(),
    }
}

/// Which column a `belongs_to_project` edge was read from — the two kinds of
/// node that carry a project name keep their own `source`, so a reader can see
/// which table said so.
fn graph_member_source(ty: NodeType) -> &'static str {
    match ty {
        NodeType::Memory => "docs.project",
        _ => "tasks.project",
    }
}

/// An annotation's first line, cut to [`GRAPH_LABEL_CHARS`].
///
/// By CHARACTER and not by byte: a note is prose, prose is UTF-8, and slicing
/// one mid-codepoint panics.
fn graph_first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    if line.chars().count() <= GRAPH_LABEL_CHARS {
        return line.to_string();
    }
    let kept: String = line.chars().take(GRAPH_LABEL_CHARS - 1).collect();
    format!("{kept}…")
}

/// The `shared_tag` half of the inferred layer: every pair of kept tasks that
/// share at least one tag, scored by how much of their two tag sets is common
/// ground.
///
/// Over the tags already loaded with the nodes, so this adds no query at all —
/// and over the KEPT tasks, after the node cap, so a pair is never drawn
/// between a node in the answer and one that was cut.
///
/// ponytail: O(n²) over the kept tasks, and n is `max_nodes` — at most 1,000
/// by contract, so half a million set comparisons in the worst case anybody can
/// ask for. An index would be a structure to keep, for a loop that does not
/// touch the store.
fn graph_shared_tags(kept: &[GraphNode]) -> Vec<GraphEdge> {
    let tasks: Vec<(&GraphNode, String)> = kept
        .iter()
        .filter(|n| n.node.0 == NodeType::Task && !n.tags.is_empty())
        .map(|n| (n, node_id(&n.node)))
        .collect();
    let mut out = Vec::new();
    for (i, (a, a_id)) in tasks.iter().enumerate() {
        for (b, b_id) in &tasks[i + 1..] {
            let shared: Vec<&String> = a.tags.iter().filter(|t| b.tags.contains(t)).collect();
            if shared.is_empty() {
                continue;
            }
            let union = a.tags.len() + b.tags.len() - shared.len();
            // One direction per pair, lower id first, so the same two tasks are
            // one edge however the walk reached them.
            let (from, to) = match a_id <= b_id {
                true => (a_id, b_id),
                false => (b_id, a_id),
            };
            out.push(GraphEdge {
                id: format!("tag:{from}:{to}"),
                from: from.clone(),
                to: to.clone(),
                relation: "shared_tag".to_string(),
                kind: "inferred",
                confidence: Some(shared.len() as f64 / union as f64),
                source: format!(
                    "tags: {}",
                    shared
                        .iter()
                        .map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            });
        }
    }
    out
}

/// One node, as `graph.query` publishes it.
fn graph_node_json(n: &GraphNode) -> Value {
    json!({
        "id": node_id(&n.node),
        "type": n.node.0.as_str(),
        "label": n.label,
        "summary": n.summary,
        "project": n.project,
        "status": n.status,
        "modified": n.modified,
        "short_id": n.short_id,
        "task": n.task,
    })
}

/// One edge, as `graph.query` publishes it.
fn graph_edge_json(e: &GraphEdge) -> Value {
    json!({
        "id": e.id,
        "from": e.from,
        "to": e.to,
        "relation": e.relation,
        "kind": e.kind,
        "confidence": e.confidence,
        "source": e.source,
    })
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
