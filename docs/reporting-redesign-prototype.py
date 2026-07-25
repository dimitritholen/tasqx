#!/usr/bin/env python3
"""Standalone prototype of the redesigned `tasqx report --html` page.

This is a THROWAWAY renderer that exists to prove the design against real data
before any of it lands in `crates/tasqx-cli/src/html.rs`. It deliberately mirrors
that file's structure so the port is mechanical:

    html.rs::generate()      -> gather()        four pure reads of the core API
    html.rs::Report::render()-> render()        assemble one document
    html.rs::Report::css()   -> css()           one inline <style>
    html.rs::esc()           -> esc()           the D19 escaper, same rule
    html.rs::svg_*()         -> svg_*()         inline SVG from core's numbers

Regenerate with:

    python3 docs/reporting-redesign-prototype.py > docs/reporting-redesign-prototype.html

It shells out to `tasqx api` for every panel, which is the same claim DESIGN.md §8
makes about the real generator: "the report generator is just another client —
anything it shows, a plugin or the MCP server could compute the same way."
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone

# ---------------------------------------------------------------------------
# Theme — the four-bucket categorical palette
# ---------------------------------------------------------------------------
# tasqx themes are TERMINAL palettes: nord's `accent`/`warn`/`danger` are tuned
# for a dark terminal, and feeding them straight into SVG fills on the report's
# light `--card` surface fails every colour check (measured: lightness band,
# chroma floor and the normal-vision adjacency floor all FAIL; worst adjacent
# pair #ebcb8b<->#a3be8c is ΔE 10.9, below the 15 floor). So the chart palette is
# *derived* from the theme's hues and stepped per colour scheme, which is what a
# design system means by "snap each slot to the nearest passing step".
#
# Both rows below pass all five checks of the dataviz validator.
# Stack order is chosen so the CVD-confusable pairs are never adjacent:
# purple<->cyan collapses under deutan and orange<->green under protan, so the
# order is cyan, orange, purple, green.
BUCKETS = [
    # key,                     label,            light,     dark
    ("tokens_cache_read", "cache read", "#00688f", "#2f9fc6"),
    ("tokens_cache_creation", "cache write", "#a35d0a", "#c07a1e"),
    ("tokens_in", "input", "#8e4b9c", "#a962c0"),
    ("tokens_out", "output", "#41762b", "#5fa036"),
]

# Relative price per token, expressed as a multiple of the base input price.
# Anthropic publishes cache read at 0.1x input and cache write at 1.25x input.
# Output is model-dependent (roughly 5x input on current models) — the page never
# renders currency, but it does use these weights to say which bucket DOMINATES
# the bill, which is a different and much more stable claim than a dollar figure.
WEIGHTS = {
    "tokens_cache_read": 0.1,
    "tokens_cache_creation": 1.25,
    "tokens_in": 1.0,
    "tokens_out": 5.0,
}

METRICS = [
    "count",
    "est_total",
    "tracked_total",
    "overdue",
    "tokens_in",
    "tokens_out",
    "tokens_cache_read",
    "tokens_cache_creation",
    "tokens_total",
]


# ---------------------------------------------------------------------------
# The one escaper (html.rs D19)
# ---------------------------------------------------------------------------
# Strip C0/C1 control bytes (keeping tab and newline) BEFORE escaping the five
# markup characters. `report --html` defaults to stdout, so a hostile title that
# carried a raw ESC would otherwise rewrite the reader's terminal. Titles are
# untrusted: they arrive via store.import, the JSON API and MCP.
_CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]")


def esc(s) -> str:
    s = "" if s is None else str(s)
    s = _CONTROL.sub("", s)
    return (
        s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&#39;")
    )


# ---------------------------------------------------------------------------
# Data gathering — four pure reads, exactly like html.rs::generate
# ---------------------------------------------------------------------------
def api(method: str, params: dict):
    env = {"tasqx": "1", "id": method, "method": method, "params": params}
    out = subprocess.run(
        ["tasqx", "api"],
        input=json.dumps(env),
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    resp = json.loads(out)
    if not resp.get("ok"):
        raise SystemExit(f"{method} failed: {resp.get('error')}")
    return resp["result"]


def gather(filter_: str | None = None):
    summary_params = {"group_by": "project", "metrics": METRICS}
    if filter_:
        summary_params["filter"] = filter_
    export_params = {}
    if filter_:
        export_params["filter"] = filter_
    actionable_filter = f"({filter_}) and @working" if filter_ else "@working"

    export = api("store.export", export_params)

    # API DELTA (see docs/reporting-redesign.md): no single read returns tracked
    # time AND tokens AND annotations.
    #   store.export -> tokens[], annotations[]   but NO tracked time
    #   task.list    -> tracked, active_since     but NO tokens, NO annotations
    # The "cost per task in both currencies" widget needs all three, so this
    # prototype issues a fifth call and joins on `id`. That join is the argument
    # for adding `tracked_seconds` to the export payload rather than something a
    # future html.rs should have to re-implement.
    # NOTE: `fields: []` is accepted and projects EVERY key away — the rows come
    # back as empty objects rather than as an error or as "no projection". Name
    # the two fields explicitly.
    listed = api("task.list", {"filter": filter_ or "", "fields": ["id", "tracked"]})
    tracked_by_id = {
        t["id"]: duration_secs(t.get("tracked")) for t in listed.get("tasks", [])
    }
    for t in export.get("tasks", []):
        t["tracked_seconds"] = tracked_by_id.get(t["id"], 0)

    return {
        "summary": api("report.summary", summary_params),
        "export": export,
        "actionable": api(
            "task.list",
            {"filter": actionable_filter, "sort": ["-urgency"], "limit": 12},
        ),
        "events": api("event.list", {"limit": 100000}),
    }


# ---------------------------------------------------------------------------
# Small format helpers
# ---------------------------------------------------------------------------
_ISO_DUR = re.compile(
    r"^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+(?:\.\d+)?)S)?)?$"
)


def duration_secs(iso: str | None) -> int:
    if not iso:
        return 0
    m = _ISO_DUR.match(iso)
    if not m:
        return 0
    d, h, mi, s = (m.group(i) for i in range(1, 5))
    return (
        int(d or 0) * 86400
        + int(h or 0) * 3600
        + int(mi or 0) * 60
        + int(float(s or 0))
    )


def humanize(secs: int) -> str:
    if secs <= 0:
        return "—"
    h, rem = divmod(secs, 3600)
    m = rem // 60
    if h and m:
        return f"{h}h {m}m"
    if h:
        return f"{h}h"
    return f"{m}m"


def compact(n: int) -> str:
    """1234567 -> 1.23M. Token counts span six orders of magnitude on one page."""
    if n is None:
        return "0"
    a = abs(n)
    if a >= 1_000_000:
        return f"{n / 1_000_000:.2f}M"
    if a >= 1_000:
        return f"{n / 1_000:.1f}k"
    return str(n)


def parse_ts(s: str | None):
    if not s:
        return None
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        return None


def pretty_ts(s: str | None) -> str:
    t = parse_ts(s)
    return t.strftime("%Y-%m-%d %H:%M UTC") if t else "—"


def clock(s: str | None) -> str:
    t = parse_ts(s)
    return t.strftime("%H:%M") if t else "—"


def daystamp(s: str | None) -> str:
    t = parse_ts(s)
    return t.strftime("%a %d %b") if t else "—"


def slug(s: str) -> str:
    """A hash-fragment-safe id. Only [a-z0-9-] survives, so a project name that
    contains a quote or a space can never break out of the attribute or the
    selector it is interpolated into."""
    out = re.sub(r"[^a-z0-9]+", "-", (s or "none").lower()).strip("-")
    return out or "none"


# ---------------------------------------------------------------------------
# Widgets
# ---------------------------------------------------------------------------
def stat(n: str, label: str, sub: str = "", flag: bool = False) -> str:
    cls = "stat flag" if flag else "stat"
    subhtml = f'<div class="s">{esc(sub)}</div>' if sub else ""
    return (
        f'<div class="{cls}"><div class="n">{esc(n)}</div>'
        f'<div class="l">{esc(label)}</div>{subhtml}</div>'
    )


def section(sid: str, title: str, sub: str, body: str) -> str:
    return (
        f'<section id="{esc(sid)}"><h2>{esc(title)}</h2>'
        f'<p class="sub">{esc(sub)}</p>{body}</section>'
    )


def empty(msg: str, hint: str = "") -> str:
    """The empty state is a first-class widget, not a blank div. Token history is
    genuinely thin right now, so every token panel must look deliberate at zero."""
    hinthtml = f'<p class="hint">{esc(hint)}</p>' if hint else ""
    return f'<div class="empty"><p>{esc(msg)}</p>{hinthtml}</div>'


def legend() -> str:
    items = "".join(
        f'<button class="lg" type="button" data-bucket="{esc(k)}" aria-pressed="true">'
        f'<span class="sw sw-{esc(k)}"></span>{esc(label)}</button>'
        for k, label, _l, _d in BUCKETS
    )
    return f'<div class="legend" role="group" aria-label="Token buckets">{items}</div>'


def token_burn(summary: dict) -> str:
    """### Widget: Token burn by project.

    Question it answers: where did this range's tokens actually go, and which
    bucket dominates?
    Data source: report.summary group_by=project -> the four tokens_* metrics.
    Mark: horizontal stacked bar, one row per project, four segments, 2px surface
    gap between segments.
    Empty state: "No token data in this range" + the config hint.
    Interaction: click a row -> filters the timeline below to that project.
    """
    groups = [g for g in summary.get("groups", []) if g.get("tokens_total", 0) > 0]
    if not groups:
        return section(
            "tokens",
            "Token burn by project",
            "Where this range's tokens actually went.",
            empty(
                "No token data in this range.",
                "Token accounting is opt-in: `tasqx config set tokens.enabled true`, "
                "then run `tasqx daemon` so the attribution thread can reconstruct "
                "spend after each completion.",
            ),
        )

    groups.sort(key=lambda g: -g.get("tokens_total", 0))
    scale = max(g["tokens_total"] for g in groups)

    rows = []
    for g in groups:
        proj = g.get("project") or "(none)"
        sid = slug(proj)
        total = g["tokens_total"]
        width = total / scale * 100.0

        def segments(values: dict, denom: float, unit: str) -> str:
            out = []
            for key, label, _l, _d in BUCKETS:
                v = values.get(key, 0)
                if v <= 0:
                    continue
                pct = v / denom * 100.0
                out.append(
                    f'<span class="seg seg-{key}" style="flex:{pct:.4f} 1 0"'
                    f' title="{esc(label)}: {pct:.1f}% of {esc(unit)}"></span>'
                )
            return "".join(out)

        # TWO bars, not one. Looking at the rendered page is what forced this:
        # with cache read at 98% of volume, the other three buckets rendered at
        # 7px, 2px and 3px — three of the four buckets were visually nil, and the
        # one claim the panel exists to make ("volume is not cost") was carried
        # only by a footnote. The cost-weighted bar makes the argument the way a
        # chart is supposed to: by looking different from the one above it.
        weighted = {k: g.get(k, 0) * w for k, w in WEIGHTS.items()}
        wtotal = sum(weighted.values())

        vol_bar = (
            f'<span class="btrack vol"><span class="bbar" style="width:{width:.3f}%">'
            f'{segments(g, total, "volume")}</span></span>'
        )
        if wtotal > 0:
            top = max(weighted, key=weighted.get)
            toplabel = next(l for k, l, _a, _b in BUCKETS if k == top)
            share = weighted[top] / wtotal * 100.0
            costnote = f"{toplabel} drives ~{share:.0f}% of the cost"
            cost_bar = (
                '<span class="btrack cost"><span class="bbar" style="width:100%">'
                f'{segments(weighted, wtotal, "cost")}</span></span>'
            )
        else:
            costnote = ""
            cost_bar = '<span class="btrack cost"></span>'

        rows.append(
            f'<button type="button" class="brow" data-project="{esc(sid)}"'
            f' aria-pressed="false">'
            f'<span class="bname">{esc(proj)}</span>'
            f'<span class="blabel">volume</span>{vol_bar}'
            f'<span class="bval">{esc(compact(total))}</span>'
            f'<span class="blabel2">cost share</span>{cost_bar}'
            f'<span class="bnote">{esc(costnote)}</span>'
            "</button>"
        )

    return section(
        "tokens",
        "Token burn by project",
        "Four buckets, never blended — cache read costs 0.1x input and cache "
        "write 1.25x, so one total would lie. Click a row to filter the timeline.",
        legend() + f'<div class="bars">{"".join(rows)}</div>',
    )


def cost_per_task(tasks: list) -> str:
    """### Widget: Cost per task, in both currencies.

    Question it answers: what did this task cost me — in time and in tokens?
    Data source: store.export -> task.tracked_seconds + task.tokens[] measurements.
    Mark: table, one row per task; a mono time column and a four-segment
    micro-bar per row; the row is a link to that task's detail panel.
    Empty state: "No measured work yet" + how to make a task measurable.
    Interaction: click a row -> :target opens the task detail below.
    """
    measured = []
    for t in tasks:
        toks = t.get("tokens") or []
        tracked = t.get("tracked_seconds", 0) or 0
        if not toks and tracked <= 0:
            continue
        agg = {k: 0 for k, _l, _a, _b in BUCKETS}
        conf = set()
        src = set()
        for m in toks:
            agg["tokens_in"] += m.get("input_tokens", 0)
            agg["tokens_out"] += m.get("output_tokens", 0)
            agg["tokens_cache_read"] += m.get("cache_read_tokens", 0)
            agg["tokens_cache_creation"] += m.get("cache_creation_tokens", 0)
            conf.add(m.get("confidence", "?"))
            src.add(m.get("source", "?"))
        measured.append((t, agg, sum(agg.values()), tracked, conf, src))

    if not measured:
        return section(
            "cost",
            "Cost per task",
            "Time and tokens, side by side.",
            empty(
                "No measured work yet.",
                "A task is measurable once it has a timer interval or an "
                "attributed measurement. Start one with `tasqx start <id>`.",
            ),
        )

    measured.sort(key=lambda r: (-r[2], -r[3]))
    scale = max((r[2] for r in measured), default=1) or 1

    rows = []
    for t, agg, total, tracked, conf, src in measured:
        sid = t["short_id"]
        segs = []
        if total > 0:
            for key, label, _l, _d in BUCKETS:
                v = agg[key]
                if v <= 0:
                    continue
                segs.append(
                    f'<span class="seg seg-{key}" style="flex:{v / total:.6f} 1 0"'
                    f' title="{esc(label)}: {esc(f"{v:,}")}"></span>'
                )
            barw = total / scale * 100.0
            bar = (
                f'<span class="btrack mini"><span class="bbar" '
                f'style="width:{barw:.3f}%">{"".join(segs)}</span></span>'
            )
            tokcell = f"{bar}<span class=\"bval\">{esc(compact(total))}</span>"
            # Confidence is part of the number's meaning, so it is shown with the
            # number rather than hidden in a footnote. HIGH means the transcript
            # was parsed AND the session id was verified against it.
            badge = (
                f'<span class="conf conf-{esc(sorted(conf)[0])}">'
                f"{esc(sorted(conf)[0])}</span>"
            )
        else:
            tokcell = '<span class="muted">not attributed</span>'
            badge = ""

        rows.append(
            f'<tr><td class="id"><a href="#task-{sid}">#{sid}</a></td>'
            f'<td class="ttl">{esc(t.get("title", ""))}</td>'
            f'<td class="chip">{esc(t.get("project") or "—")}</td>'
            f'<td class="num">{esc(humanize(tracked))}</td>'
            f'<td class="tok">{tokcell}</td>'
            f"<td class=\"num\">{badge}</td></tr>"
        )

    return section(
        "cost",
        "Cost per task",
        "Two currencies, never merged into one score: wall-clock time on the "
        "left, tokens on the right. Click an id to open the task.",
        legend()
        + '<div class="tablewrap"><table class="grid">'
        '<thead><tr><th>id</th><th>task</th><th>project</th>'
        '<th class="num">tracked</th><th>tokens</th>'
        '<th class="num">conf</th></tr></thead>'
        f"<tbody>{''.join(rows)}</tbody></table></div>",
    )


def timeline(events: list, tasks_by_id: dict) -> str:
    """### Widget: Lifecycle timeline.

    Question it answers: what actually happened, in order, and how long did each
    working interval last?
    Data source: event.list -> ops start / stop / done / tokens.attributed,
    joined to store.export by entity_id.
    Mark: a day-grouped vertical list; one row per event; a rule per day.
    Theme roles: timer.active for start, accent for stop, urgency.ramp hot end
    for done, muted for tokens.attributed.
    Empty state: "No lifecycle events in this range."
    Interaction: filtered live by the project row clicked above; each row links
    to its task's detail panel.
    """
    keep = {"start", "stop", "done", "tokens.attributed"}
    evs = [e for e in events if e.get("op") in keep and e.get("entity") == "task"]
    if not evs:
        return section(
            "timeline",
            "Timeline",
            "Started, stopped, completed.",
            empty(
                "No lifecycle events in this range.",
                "`tasqx start` / `tasqx stop` / `tasqx done` each append one event.",
            ),
        )

    evs.sort(key=lambda e: e.get("ts", ""), reverse=True)
    evs = evs[:60]

    by_day = defaultdict(list)
    for e in evs:
        by_day[daystamp(e.get("ts"))].append(e)

    OPLABEL = {
        "start": "started",
        "stop": "stopped",
        "done": "completed",
        "tokens.attributed": "tokens attributed",
    }

    out = []
    for day, group in by_day.items():
        rows = []
        for e in group:
            t = tasks_by_id.get(e.get("entity_id"))
            sid = t.get("short_id") if t else None
            title = t.get("title", "") if t else "(task no longer in this scope)"
            proj = (t.get("project") if t else None) or "(none)"
            op = e["op"]

            detail = ""
            payload = e.get("payload") or {}
            if op == "tokens.attributed":
                totals = payload.get("totals") or {}
                if totals:
                    n = sum(totals.values())
                    detail = f"{compact(n)} tokens · {payload.get('samples', 0)} samples"
                else:
                    # The marker with no measurement — a task that terminated with
                    # nothing found. Showing it is the point: it is the difference
                    # between "cost nothing" and "was never measured".
                    detail = "no spend found in window"

            idcell = (
                f'<a class="id" href="#task-{sid}">#{sid}</a>'
                if sid is not None
                else '<span class="id muted">#—</span>'
            )
            rows.append(
                f'<li class="ev ev-{esc(op.replace(".", "-"))}" '
                f'data-project="{esc(slug(proj))}">'
                f'<span class="tm">{esc(clock(e.get("ts")))}</span>'
                f'<span class="dot"></span>'
                f'<span class="op">{esc(OPLABEL[op])}</span>'
                f"{idcell}"
                f'<span class="ttl">{esc(title)}</span>'
                + (f'<span class="det">{esc(detail)}</span>' if detail else "")
                + "</li>"
            )
        out.append(
            f'<div class="day"><h3>{esc(day)}</h3>'
            f'<ul class="events">{"".join(rows)}</ul></div>'
        )

    return section(
        "timeline",
        "Timeline",
        "Started, stopped, completed — straight off the event log. "
        "Filtered by the project you select above.",
        '<div class="filterbar" id="filterbar" hidden>'
        '<span class="fnote">Filtered to <b id="fname"></b></span>'
        '<button type="button" id="fclear" class="fclear">clear</button></div>'
        f'<div class="timeline">{"".join(out)}</div>',
    )


def task_details(tasks: list) -> str:
    """### Widget: Task detail panels (the drill-down target).

    Question it answers: what is this task, what did it cost, and what did I
    write down about it?
    Data source: store.export -> the full task object incl. annotations[] and
    tokens[].
    Mark: one panel per task, hidden until :target selects it.
    Interaction: reached by any `#task-<id>` link on the page; closing is a link
    back to `#cost`. No History API — pushState throws SecurityError on file://
    (DESIGN.md §8a), so this is pure :target CSS with zero JS.
    """
    panels = []
    for t in sorted(tasks, key=lambda x: x["short_id"]):
        sid = t["short_id"]
        toks = t.get("tokens") or []
        anns = t.get("annotations") or []

        if toks:
            agg = {k: 0 for k, _l, _a, _b in BUCKETS}
            for m in toks:
                agg["tokens_in"] += m.get("input_tokens", 0)
                agg["tokens_out"] += m.get("output_tokens", 0)
                agg["tokens_cache_read"] += m.get("cache_read_tokens", 0)
                agg["tokens_cache_creation"] += m.get("cache_creation_tokens", 0)
            cells = "".join(
                f'<div class="tk"><span class="sw sw-{k}"></span>'
                f'<span class="tkl">{esc(label)}</span>'
                f'<span class="tkv">{esc(f"{agg[k]:,}")}</span></div>'
                for k, label, _a, _b in BUCKETS
            )
            m0 = toks[0]
            prov = (
                f'<p class="prov">measured via <code>{esc(m0.get("source", "?"))}</code>'
                f' from <code>{esc(m0.get("tool", "?"))}</code>'
                f' · confidence <b>{esc(m0.get("confidence", "?"))}</b>'
                f' · {len(toks)} measurement(s)</p>'
            )
            tokblock = f'<div class="tkgrid">{cells}</div>{prov}'
        else:
            tokblock = (
                '<p class="muted small">No token measurement. A task is only '
                "attributed when its <code>done</code> event carries correlation "
                "(client / session_id / transcript_path).</p>"
            )

        if anns:
            annblock = "".join(
                f'<li><span class="when">{esc(pretty_ts(a.get("created")))}</span>'
                f'<div class="body">{esc(a.get("body", ""))}</div></li>'
                for a in anns
            )
            annblock = f'<ul class="anns">{annblock}</ul>'
        else:
            annblock = '<p class="muted small">No annotations.</p>'

        tags = "".join(f'<span class="tag">{esc(g)}</span>' for g in t.get("tags", []))

        panels.append(
            f'<article class="detail" id="task-{sid}">'
            f'<header><span class="id">#{sid}</span>'
            f'<h3>{esc(t.get("title", ""))}</h3>'
            f'<a class="close" href="#cost" aria-label="Close">close</a></header>'
            f'<dl class="meta">'
            f"<div><dt>status</dt><dd>{esc(t.get('status', ''))}</dd></div>"
            f"<div><dt>project</dt><dd>{esc(t.get('project') or '—')}</dd></div>"
            f"<div><dt>priority</dt><dd>{esc(t.get('priority') or '—')}</dd></div>"
            f"<div><dt>estimate</dt><dd>{esc(humanize(duration_secs(t.get('estimate'))))}</dd></div>"
            f"<div><dt>tracked</dt><dd>{esc(humanize(t.get('tracked_seconds', 0) or 0))}</dd></div>"
            f"<div><dt>urgency</dt><dd>{esc(t.get('urgency', '—'))}</dd></div>"
            f"</dl>"
            + (f'<div class="tags">{tags}</div>' if tags else "")
            + f"<h4>Token cost</h4>{tokblock}"
            + f"<h4>Annotations</h4>{annblock}"
            + "</article>"
        )
    return f'<div class="details">{"".join(panels)}</div>'


# ---------------------------------------------------------------------------
# Inline SVG — sparkline of daily activity
# ---------------------------------------------------------------------------
def svg_activity(events: list, now: datetime, days: int = 21) -> str:
    """Inline SVG, generated at render time so the chart is part of the DOCUMENT
    (mailable, printable, greppable) rather than something a runtime paints later.
    urgency.ramp is used as a real <linearGradient>, matching html.rs::ramp_stops."""
    start = (now - timedelta(days=days - 1)).date()
    counts = Counter()
    for e in events:
        if e.get("op") != "done":
            continue
        t = parse_ts(e.get("ts"))
        if t and t.date() >= start:
            counts[t.date()] += 1

    series = [counts.get(start + timedelta(days=i), 0) for i in range(days)]
    peak = max(series) if series else 0

    w, h = 720.0, 96.0
    pad_l, pad_b, pad_t = 26.0, 18.0, 8.0
    plot_w, plot_h = w - pad_l - 10.0, h - pad_b - pad_t
    slot = plot_w / days
    bar_w = min(slot * 0.62, 22.0)

    if peak == 0:
        return (
            '<figure class="chart">'
            + empty(
                "Nothing completed in the last 21 days.",
                "The bars appear as `tasqx done` events land.",
            )
            + "</figure>"
        )

    bars = []
    for i, v in enumerate(series):
        cx = pad_l + slot * (i + 0.5)
        bh = (v / peak) * plot_h if v else 0
        y = pad_t + plot_h - bh
        if v:
            bars.append(
                f'<rect x="{cx - bar_w / 2:.1f}" y="{y:.1f}" width="{bar_w:.1f}" '
                f'height="{bh:.1f}" rx="4" fill="url(#ramp)"><title>'
                f"{(start + timedelta(days=i)).isoformat()}: {v} done</title></rect>"
            )
        else:
            bars.append(
                f'<rect x="{cx - bar_w / 2:.1f}" y="{pad_t + plot_h - 2:.1f}" '
                f'width="{bar_w:.1f}" height="2" rx="1" class="zero"/>'
            )

    labels = "".join(
        f'<text class="ax" x="{pad_l + slot * (i + 0.5):.1f}" y="{h - 4:.1f}" '
        f'text-anchor="middle">{(start + timedelta(days=i)).strftime("%d")}</text>'
        for i in range(0, days, 3)
    )

    return (
        f'<figure class="chart"><svg viewBox="0 0 {w:.0f} {h:.0f}" '
        f'role="img" aria-label="Tasks completed per day, last {days} days">'
        "<defs><linearGradient id=\"ramp\" x1=\"0\" y1=\"1\" x2=\"0\" y2=\"0\">"
        '<stop offset="0%" stop-color="var(--ramp0)"/>'
        '<stop offset="50%" stop-color="var(--ramp1)"/>'
        '<stop offset="100%" stop-color="var(--ramp2)"/>'
        "</linearGradient></defs>"
        f'<text class="ax" x="0" y="{pad_t + 8:.1f}">{peak}</text>'
        f'<text class="ax" x="0" y="{pad_t + plot_h:.1f}">0</text>'
        f'{"".join(bars)}{labels}</svg></figure>'
    )


# ---------------------------------------------------------------------------
# CSS
# ---------------------------------------------------------------------------
def css() -> str:
    light = "\n".join(
        f"  --c-{k}: {l};" for k, _lab, l, _d in BUCKETS
    )
    dark = "\n".join(f"  --c-{k}: {d};" for k, _lab, _l, d in BUCKETS)
    swatches = "\n".join(
        f".sw-{k}, .seg-{k} {{ background: var(--c-{k}); }}"
        for k, _lab, _l, _d in BUCKETS
    )
    hides = "\n".join(
        f'.page[data-off~="{k}"] .seg-{k} {{ display: none; }}\n'
        f'.page[data-off~="{k}"] .lg[data-bucket="{k}"] {{ opacity: .45; '
        "text-decoration: line-through; }"
        for k, _lab, _l, _d in BUCKETS
    )
    return f""":root {{
  color-scheme: light dark;
  --bg: #ffffff; --fg: #1a1d23; --muted: #5b626e;
  --card: #fcfcfb; --line: #e3e6ea;
  --accent: #1a6e8c;
  --ramp0: #4f7a3f; --ramp1: #9a7a1e; --ramp2: #a33b45;
  --ok: #41762b; --danger: #a33b45;
{light}
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    /* The chart surface in dark mode is --bg, NOT the lighter --card the rest of
       the page uses: measured against a #545962 card every four-bucket step
       either left the OKLCH L 0.48-0.67 band or fell under 3:1 contrast. On
       #2e3440 the same four steps pass all five checks. */
    --bg: #2e3440; --fg: #d8dee9; --muted: #93a0b4;
    --card: #363d4a; --line: #4c566a;
    --accent: #88c0d0;
    --ramp0: #a3be8c; --ramp1: #ebcb8b; --ramp2: #bf616a;
    --ok: #a3be8c; --danger: #bf616a;
{dark}
  }}
}}
* {{ box-sizing: border-box; }}
body {{ margin: 0; background: var(--bg); color: var(--fg); line-height: 1.55;
  font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
.id, .num, .tm, .bval, .tkv, code, .mono {{ font-family: ui-monospace, "Cascadia Code", "SF Mono", Consolas, monospace; }}
main {{ max-width: 92ch; margin: 0 auto; padding: 1.5rem 1.25rem 4rem; }}
a {{ color: var(--accent); }}

/* The header shares main's measure. Full-bleed, the stats drifted to the far
   right of a 2055px viewport while the content column sat centred 640px away —
   they read as belonging to different pages. */
header.summary {{ position: sticky; top: 0; z-index: 5; border-bottom: 1px solid var(--line);
  background: var(--bg); }}
header.summary > .hwrap {{ max-width: 92ch; margin: 0 auto; padding: .85rem 1.25rem;
  display: flex; align-items: center; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }}
.brand {{ font-weight: 700; font-size: 1.1rem; letter-spacing: -.01em; }}
.brand span {{ font-weight: 400; color: var(--muted); }}
.stats {{ display: flex; gap: 1.5rem; flex-wrap: wrap; }}
.stat {{ text-align: right; }}
.stat .n {{ font-size: 1.45rem; font-weight: 700; line-height: 1.1; font-variant-numeric: tabular-nums;
  font-family: ui-monospace, monospace; }}
.stat .l {{ font-size: .7rem; color: var(--muted); text-transform: uppercase; letter-spacing: .06em; }}
.stat .s {{ font-size: .7rem; color: var(--muted); }}
.stat.flag .n {{ color: var(--danger); }}

section {{ margin-top: 2.4rem; scroll-margin-top: 8rem; }}
section > h2 {{ font-size: 1.05rem; margin: 0 0 .15rem; letter-spacing: -.01em; }}
section > .sub {{ color: var(--muted); font-size: .85rem; margin: 0 0 .9rem; max-width: 70ch; }}
h3 {{ font-size: .8rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin: 1.2rem 0 .4rem; }}
h4 {{ font-size: .78rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); margin: 1rem 0 .35rem; }}
.muted {{ color: var(--muted); }} .small {{ font-size: .85rem; }}

.empty {{ border: 1px dashed var(--line); border-radius: 12px; padding: 1.4rem 1.2rem;
  background: var(--card); text-align: center; }}
.empty p {{ margin: 0; }}
.empty .hint {{ margin-top: .4rem; color: var(--muted); font-size: .84rem; max-width: 62ch;
  margin-left: auto; margin-right: auto; }}

.legend {{ display: flex; gap: .4rem; flex-wrap: wrap; margin-bottom: .8rem; }}
.lg {{ display: inline-flex; align-items: center; gap: .4rem; font: inherit; font-size: .78rem;
  color: var(--fg); background: var(--card); border: 1px solid var(--line); border-radius: 999px;
  padding: .2rem .7rem; cursor: pointer; }}
.sw {{ width: 10px; height: 10px; border-radius: 3px; display: inline-block; }}
{swatches}
{hides}

.bars {{ display: flex; flex-direction: column; gap: .35rem; }}
.brow {{ display: grid; grid-template-columns: 16ch 6.5ch 1fr 7ch;
  grid-template-areas: "n l1 b1 v" ". l2 b2 ." ". . note note";
  gap: .25rem .7rem; align-items: center; width: 100%; text-align: left; font: inherit;
  background: none; border: 0; border-radius: 8px; padding: .55rem .5rem; cursor: pointer; }}
.blabel {{ grid-area: l1; }} .blabel2 {{ grid-area: l2; }}
.blabel, .blabel2 {{ font-size: .64rem; text-transform: uppercase; letter-spacing: .05em;
  color: var(--muted); text-align: right; }}
/* Explicit classes, not :nth-of-type — that pseudo-class counts among siblings
   of the same ELEMENT type, and every cell in this row is a <span>, so
   :nth-of-type(2) selected the second span rather than the second track and
   dropped the cost bar into the name column. Caught by looking at the page. */
.btrack.vol {{ grid-area: b1; }}
.btrack.cost {{ grid-area: b2; }}
.btrack.cost .bbar {{ height: 12px; }}
.brow:hover {{ background: var(--card); }}
.brow[aria-pressed="true"] {{ background: var(--card); outline: 2px solid var(--accent); }}
.bname {{ grid-area: n; font-weight: 600; font-size: .87rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
.btrack {{ background: var(--card); border-radius: 5px; overflow: hidden; }}
.bbar {{ display: flex; height: 20px; gap: 2px; }}
/* td.tok is a flex row, so without a basis the track collapses to its content
   and the four segments render ~2px wide each — measured 14px total in the
   browser before this line existed. Found by looking at the page, not by any
   structural check. */
.btrack.mini {{ flex: 1 1 auto; min-width: 8rem; }}
.btrack.mini .bbar {{ height: 12px; }}
.seg {{ min-width: 2px; }}
.seg:first-child {{ border-radius: 4px 0 0 4px; }}
.seg:last-child {{ border-radius: 0 4px 4px 0; }}
.bval {{ grid-area: v; text-align: right; font-size: .82rem; font-variant-numeric: tabular-nums; }}
.bnote {{ grid-area: note; font-size: .72rem; color: var(--muted); }}

.tablewrap {{ overflow-x: auto; }}
table.grid {{ width: 100%; border-collapse: collapse; font-size: .88rem; }}
table.grid th {{ text-align: left; color: var(--muted); font-weight: 600; font-size: .7rem;
  text-transform: uppercase; letter-spacing: .05em; border-bottom: 1px solid var(--line); padding: .4rem .5rem; }}
table.grid td {{ padding: .45rem .5rem; border-bottom: 1px solid var(--line); vertical-align: middle; }}
table.grid td.num, table.grid th.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
table.grid td.ttl {{ min-width: 22ch; }}
table.grid td.tok {{ display: flex; align-items: center; gap: .5rem; min-width: 16ch; }}
td.chip {{ color: var(--muted); font-size: .8rem; }}
.conf {{ font-size: .66rem; text-transform: uppercase; letter-spacing: .05em; border-radius: 999px;
  padding: .05rem .45rem; border: 1px solid var(--line); }}
.conf-high {{ color: var(--ok); border-color: var(--ok); }}

.filterbar {{ display: flex; align-items: center; gap: .6rem; margin-bottom: .8rem;
  background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: .35rem .7rem; font-size: .82rem; }}
.fclear {{ font: inherit; font-size: .78rem; background: none; border: 1px solid var(--line);
  border-radius: 999px; padding: .05rem .6rem; cursor: pointer; color: var(--fg); }}
.timeline .day {{ margin-bottom: .4rem; }}
ul.events {{ list-style: none; margin: 0; padding: 0; border-left: 2px solid var(--line); }}
li.ev {{ display: flex; align-items: baseline; gap: .55rem; padding: .3rem .1rem .3rem .9rem;
  position: relative; flex-wrap: wrap; font-size: .87rem; }}
li.ev .tm {{ color: var(--muted); font-size: .78rem; min-width: 4.5ch; }}
li.ev .dot {{ position: absolute; left: -5px; top: .75rem; width: 8px; height: 8px; border-radius: 50%;
  background: var(--muted); }}
li.ev-start .dot {{ background: var(--ok); }}
li.ev-done .dot {{ background: var(--ramp2); }}
li.ev-stop .dot {{ background: var(--accent); }}
li.ev .op {{ font-size: .72rem; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); min-width: 9ch; }}
li.ev .ttl {{ flex: 1; min-width: 14ch; }}
li.ev .det {{ font-size: .76rem; color: var(--muted); }}
.page[data-filter] li.ev {{ display: none; }}
.page[data-filter] li.ev.match {{ display: flex; }}

/* scroll-margin lives on the panel itself, not on :target — it must apply
   before the jump, and the sticky header measures 114px, not the 80px a
   5rem guess assumed. Measured in the browser. */
.details .detail {{ display: none; scroll-margin-top: 8rem; }}
.details .detail:target {{ display: block; border: 1px solid var(--accent); border-radius: 12px;
  background: var(--card); padding: 1rem 1.1rem; margin-top: 2rem; }}
.detail header {{ display: flex; align-items: baseline; gap: .6rem; }}
.detail header h3 {{ margin: 0; font-size: 1rem; text-transform: none; letter-spacing: 0; color: var(--fg); flex: 1; }}
.detail .close {{ font-size: .78rem; }}
dl.meta {{ display: flex; flex-wrap: wrap; gap: .3rem 1.5rem; margin: .8rem 0 0; }}
dl.meta dt {{ font-size: .68rem; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); }}
dl.meta dd {{ margin: 0; font-size: .9rem; }}
.tkgrid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr)); gap: .4rem; }}
.tk {{ display: flex; align-items: center; gap: .45rem; background: var(--bg); border: 1px solid var(--line);
  border-radius: 8px; padding: .35rem .6rem; }}
.tkl {{ font-size: .78rem; color: var(--muted); flex: 1; }}
.tkv {{ font-size: .84rem; font-variant-numeric: tabular-nums; }}
.prov {{ font-size: .76rem; color: var(--muted); margin: .5rem 0 0; }}
ul.anns {{ list-style: none; margin: 0; padding: 0; }}
ul.anns li {{ border-left: 2px solid var(--line); padding: .3rem 0 .6rem .8rem; margin-bottom: .3rem; }}
ul.anns .when {{ font-size: .72rem; color: var(--muted); }}
ul.anns .body {{ white-space: pre-wrap; font-size: .87rem; }}
.tags {{ display: flex; gap: .35rem; flex-wrap: wrap; margin-top: .6rem; }}
.tag {{ font-size: .74rem; border: 1px solid var(--line); border-radius: 999px; padding: .05rem .55rem; color: var(--muted); }}

figure.chart {{ margin: 0; border: 1px solid var(--line); border-radius: 12px; background: var(--bg); padding: .8rem; }}
figure.chart svg {{ display: block; width: 100%; height: auto; }}
text.ax {{ fill: var(--muted); font-size: 10px; font-family: ui-monospace, monospace; }}
rect.zero {{ fill: var(--line); }}

footer {{ max-width: 92ch; margin: 0 auto; padding: 1rem 1.25rem 3rem; color: var(--muted); font-size: .8rem; }}
@media print {{ header.summary {{ position: static; }} .details .detail {{ display: block; }} }}
"""


# ---------------------------------------------------------------------------
# The whole page
# ---------------------------------------------------------------------------
SCRIPT = """
// The entire interaction budget of this page, in vanilla JS.
// The History API is deliberately untouched: its state-pushing methods throw
// SecurityError on a file:// document (origin null), which is exactly how this
// page is opened (DESIGN.md 8a). Drill-down is :target CSS instead.
// Everything below is attribute flips; CSS does the rendering.
(function () {
  var page = document.querySelector('.page');

  // 1. Legend toggles a bucket off across every stacked bar at once.
  page.addEventListener('click', function (e) {
    var lg = e.target.closest('.lg');
    if (!lg) return;
    var key = lg.dataset.bucket;
    var off = (page.dataset.off || '').split(/\\s+/).filter(Boolean);
    var i = off.indexOf(key);
    if (i < 0) { off.push(key); } else { off.splice(i, 1); }
    page.dataset.off = off.join(' ');
    // Keep every copy of the legend in sync - the page renders it twice.
    var on = off.indexOf(key) < 0;
    page.querySelectorAll('.lg[data-bucket="' + key + '"]').forEach(function (b) {
      b.setAttribute('aria-pressed', on ? 'true' : 'false');
    });
  });

  // 2. A project row filters the timeline. Clicking the active row clears it.
  var bar = document.getElementById('filterbar');
  var fname = document.getElementById('fname');
  function applyFilter(slug, label) {
    page.querySelectorAll('.brow').forEach(function (b) {
      b.setAttribute('aria-pressed', b.dataset.project === slug ? 'true' : 'false');
    });
    if (slug) {
      page.dataset.filter = slug;
      fname.textContent = label;
      bar.hidden = false;
    } else {
      delete page.dataset.filter;
      bar.hidden = true;
    }
    page.querySelectorAll('li.ev').forEach(function (li) {
      li.classList.toggle('match', !slug || li.dataset.project === slug);
    });
  }
  page.addEventListener('click', function (e) {
    var row = e.target.closest('.brow');
    if (!row) return;
    var active = row.getAttribute('aria-pressed') === 'true';
    applyFilter(active ? null : row.dataset.project,
                active ? '' : row.querySelector('.bname').textContent);
  });
  document.getElementById('fclear').addEventListener('click', function () {
    applyFilter(null, '');
  });
})();
"""


def render(data: dict) -> str:
    tasks = data["export"].get("tasks", [])
    by_id = {t["id"]: t for t in tasks}
    events = data["events"].get("events", [])
    now = datetime.now(timezone.utc)
    cutoff = now - timedelta(days=7)

    open_n = sum(1 for t in tasks if t.get("status") in ("pending", "active", "waiting"))
    overdue_n = sum(
        1
        for t in tasks
        if t.get("status") in ("pending", "active", "waiting")
        and (parse_ts(t.get("due")) or now + timedelta(days=1)) < now
    )
    done_recent = sum(
        1
        for t in tasks
        if t.get("status") == "done" and (parse_ts(t.get("completed")) or cutoff) >= cutoff
    )
    tracked_total = sum(t.get("tracked_seconds", 0) or 0 for t in tasks)

    groups = data["summary"].get("groups", [])
    tot = {k: sum(g.get(k, 0) for g in groups) for k, _l, _a, _b in BUCKETS}
    grand = sum(tot.values())
    measured_tasks = sum(1 for t in tasks if t.get("tokens"))

    # The header shows the four buckets as four stats. It deliberately does NOT
    # show one blended "AI tokens" number the way html.rs:273 does today:
    # engine/reports.rs:73 keeps the buckets apart precisely because a blended
    # total would lie, and then the presentation layer blended them anyway.
    if grand > 0:
        tokstats = "".join(
            stat(compact(tot[k]), label, "")
            for k, label, _a, _b in BUCKETS
        )
    else:
        tokstats = stat("—", "tokens", "not measured yet")

    head = (
        '<header class="summary"><div class="hwrap">'
        '<div class="brand">tasqx <span>review</span></div>'
        f'<div class="stats">{stat(str(open_n), "open")}'
        f'{stat(str(done_recent), "done / 7d")}'
        f'{stat(humanize(tracked_total), "tracked")}'
        f'{stat(str(overdue_n), "overdue", flag=overdue_n > 0)}'
        f"{tokstats}</div></div></header>"
    )

    body = [
        head,
        '<main>',
        section(
            "activity",
            "Completions, last 21 days",
            f"{measured_tasks} of {len(tasks)} tasks carry a token measurement.",
            svg_activity(events, now),
        ),
        token_burn(data["summary"]),
        cost_per_task(tasks),
        timeline(events, by_id),
        task_details(tasks),
        "</main>",
        f'<footer>Generated {esc(pretty_ts(now.isoformat()))} · '
        "every panel is a pure read of the tasqx core API · "
        "prototype for docs/reporting-redesign.md</footer>",
    ]

    return (
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n"
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        "<title>tasqx report · redesign prototype</title>\n"
        f"<style>\n{css()}</style>\n</head>\n"
        f'<body>\n<div class="page">\n{"".join(body)}\n</div>\n'
        f"<script>{SCRIPT}</script>\n</body>\n</html>\n"
    )


def main() -> None:
    filter_ = sys.argv[1] if len(sys.argv) > 1 else None
    sys.stdout.write(render(gather(filter_)))


if __name__ == "__main__":
    main()
