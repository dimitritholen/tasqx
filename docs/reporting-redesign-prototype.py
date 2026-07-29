#!/usr/bin/env python3
"""Standalone prototype of the redesigned `tasqx report --html` page.

This is a THROWAWAY renderer that exists to prove the design against real data
before any of it lands in `crates/tasqx-cli/src/html.rs`. It deliberately mirrors
that file's structure so the port is mechanical:

    html.rs::generate()      -> gather()        five pure reads of the core API
    html.rs::Report::render()-> render()        assemble one document
    html.rs::Report::css()   -> css()           one inline <style>
    html.rs::esc()           -> esc()           the D19 escaper, same rule
    html.rs::svg_*()         -> svg_*()         inline SVG from core's numbers

The five reads are `report.summary` x2 (one unwindowed for the backlog, one
windowed for the flow — see `gather`), `store.export`, `task.list` (the tracked
join) and `event.list`.

Regenerate with:

    # full page, 30-day window (the default), from the live store
    python3 docs/reporting-redesign-prototype.py > docs/reporting-redesign-prototype.html

    # a different window — the range is a GENERATION-TIME parameter, not an
    # in-page control (fetch is banned, replaceState throws on file://)
    python3 docs/reporting-redesign-prototype.py --range all > alltime.html

    # the empty / sparse variant — filter and range compose
    python3 docs/reporting-redesign-prototype.py 'project:finly-next' --range 30d \
      > docs/reporting-redesign-prototype-empty.html

EVERY `#task-N` emitter on this page calls `TaskRefs.link()`. A panel that links
to a task any other way is the defect: the href would resolve to nothing, which
is exactly what drift guard §7.6 exists to catch.

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
from typing import NamedTuple

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


def gather(filter_: str | None, rng: Range):
    """Pure reads, exactly like html.rs::generate — but TWO summaries.

    The page mixes two kinds of question and they do not share a window:

      summary_now     scope only            backlog: open, overdue. About NOW.
      summary_window  scope AND rng.clause  throughput and tokens. About the WINDOW.

    One filtered summary cannot serve both, and the failure is silent rather
    than loud. Under any `completed.` predicate `report.summary`'s non-token
    metrics change meaning without changing name: `count` stops being "tasks"
    and becomes "completions in window", and `overdue` goes STRUCTURALLY to zero
    because reports.rs:142 guards it with `status.is_open()` while
    filter.rs:335 makes a null `completed` fail every completion bound — the two
    conditions are mutually exclusive by construction. Measured:
    `completed.after:-1d` returns `overdue: 0` for every group. A page that fed
    the windowed summary to the header would print a confident 0 OVERDUE meaning
    "we filtered out everything that could have been overdue".

    THE EXPORT IS NOT WINDOWED, on purpose. It is this page's task dictionary:
    `task_details` renders one `#task-<id>` panel per REACHABLE task, and every
    `#task-<id>` link on the page — timeline, cost table, unattributed panel —
    resolves into it. Scoping the export to the range would leave live links
    pointing at panels that were filtered out (drift guard §7.6). Panels window
    in Python instead, against `rng`.

    FIVE reads, not six: the old `@working` `task.list` fed a panel that was
    never built, so it was one subprocess per page for nothing. Deleted rather
    than preserved — see docs/reporting-redesign.md §9.
    """
    windowed = rng.compose(filter_)

    def scoped(base: dict, f: str | None) -> dict:
        """Add `filter` or leave it ABSENT — core reads a missing key as "no
        filter" and would reject a null (html.rs::scoped, same rule)."""
        p = dict(base)
        if f:
            p["filter"] = f
        return p

    summary_params = {"group_by": "project", "metrics": METRICS}
    summary_now = api("report.summary", scoped(summary_params, filter_))
    # With `all` the two reads are identical by construction, so don't issue the
    # call — and, more usefully, make it impossible for the page to show two
    # different answers to one question.
    summary_window = (
        summary_now
        if rng.clause is None
        else api("report.summary", scoped(summary_params, windowed))
    )

    export = api("store.export", scoped({}, filter_))

    # API DELTA (docs/reporting-redesign.md §5 D-1): no single read returns
    # tracked time AND tokens AND annotations.
    #   store.export -> tokens[], annotations[]   but NO tracked time
    #   task.list    -> tracked, active_since     but NO tokens, NO annotations
    # NOTE: `fields: []` is accepted and projects EVERY key away — rows come back
    # as empty objects rather than as an error. Name the two fields explicitly.
    listed = api("task.list", {"filter": filter_ or "", "fields": ["id", "tracked"]})
    tracked_by_id = {
        t["id"]: duration_secs(t.get("tracked")) for t in listed.get("tasks", [])
    }
    for t in export.get("tasks", []):
        t["tracked_seconds"] = tracked_by_id.get(t["id"], 0)

    return {
        "range": rng,
        "filter": filter_,
        "windowed_filter": windowed,
        "summary_now": summary_now,
        "summary_window": summary_window,
        "export": export,
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
# The reporting range — ONE window, stated
# ---------------------------------------------------------------------------
# The page is generated once and is static, so a date-range CONTROL is not
# available: re-querying needs fetch/XHR (banned, and dead on file:// anyway),
# re-aggregating in the browser needs a second implementation of the roll-up
# beside core's, and even reflecting the choice in the URL needs replaceState,
# which throws SecurityError on a file:// document (DESIGN.md §8a). So the range
# is a GENERATION-TIME parameter — a CLI arg beside the existing optional filter
# — and the page's job is to state it unmissably: in the header band, in every
# windowed panel's sub-line, and in the footer, with the exact filter clause
# printed verbatim so a reader can paste it into `tasqx list` and reproduce the
# set.
DEFAULT_RANGE = "30d"
RANGE_SPELLINGS = "Nd, Nw or all (e.g. 7d, 30d, 6w, all)"
# `all` still has to draw a sparkline, and a sparkline with one slot per day of
# a two-year store is not a chart. It charts the most recent window instead, and
# says so.
CHART_DAYS_CAP = 90
_RANGE_RE = re.compile(r"^-?(\d+)\s*(d|day|days|w|wk|week|weeks)?$")


class Range(NamedTuple):
    """The one window every FLOW panel on this page shares.

    `days is None` means all time: `since` is None, `clause` is None, and
    `covers()` is total.

    What a range can and cannot mean is not uniform across panels, and pretending
    otherwise is the defect this type exists to fix:

      * A BACKLOG panel (open, overdue, oldest) is about NOW. It cannot be
        windowed by completion at all — `filter.rs::instant_cmp` returns false
        for a null `completed`, so `completed.after:` excludes every open task by
        construction, and `reports.rs:142` guards `overdue` with
        `status.is_open()`, so a windowed summary reports `overdue: 0` for the
        structural reason that the two conditions are mutually exclusive.
        Measured: `completed.after:-1d` returns overdue 0 for every group.
      * A THROUGHPUT or TOKEN panel is about a WINDOW, and every one of them
        uses THIS window.

    Hence two `report.summary` reads in `gather()`, not one filtered one.
    """

    days: int | None
    since: datetime | None
    now: datetime
    label: str
    clause: str | None

    @property
    def bucket_days(self) -> int:
        """Slot count for the activity sparkline.

        Capped for EVERY range, not just `all`: `parse_range` accepts up to 3650
        days, and 365 slots inside one 684u plot is one bar per 1.7 CSS px. The
        cap is stated on the page via `chart_capped` rather than applied
        silently.
        """
        if self.days is None:
            return CHART_DAYS_CAP
        return min(self.days, CHART_DAYS_CAP)

    @property
    def chart_capped(self) -> bool:
        """True when the page range is wider than the chart can draw."""
        return self.days is None or self.days > CHART_DAYS_CAP

    def covers(self, ts) -> bool:
        """Membership, mirroring `filter.rs::instant_cmp` EXACTLY.

        Strict `>` against a midnight-UTC boundary, and an unreadable or absent
        instant is outside the window rather than inside it — the same rule the
        core applies to a task that never completed. If this drifts from the
        filter, the page's own buckets stop agreeing with the clause it prints.
        """
        if self.since is None:
            return True
        t = ts if isinstance(ts, datetime) else parse_ts(ts)
        return t is not None and t > self.since

    def boundary(self) -> str:
        if self.since is None:
            return "every task in the store, whenever it closed"
        return f"completed after {self.since.strftime('%Y-%m-%d %H:%M UTC')}"

    def compose(self, filter_: str | None) -> str | None:
        """AND the window onto the caller's scope.

        Parenthesised for the reason `html.rs:48` gives for `@working`: the DSL
        has `or`, so `project:a or project:b and completed.after:-30d` would bind
        the wrong half and quietly answer a different question.
        """
        if self.clause is None:
            return filter_
        return f"({filter_}) and {self.clause}" if filter_ else self.clause


def parse_range(spec: str | None, now: datetime) -> Range:
    """`30d` / `6w` / `all` -> a Range, or a refusal that names the spellings.

    Silently defaulting an unreadable range would produce a page whose stated
    window is not the window the caller asked for — the exact class of defect
    this whole change is about. So it refuses, in the shape `datetime.rs::
    unparseable` uses: the offending value, then the accepted forms.

    `-30d` is accepted as a synonym for `30d` because `completed.after:-30d` is
    the clause it composes and a reader who has seen that clause will type it.
    """
    raw = (DEFAULT_RANGE if spec is None else spec).strip().lower()
    if raw in ("all", "all-time", "alltime"):
        return Range(None, None, now, "all time", None)
    m = _RANGE_RE.match(raw)
    if not m:
        raise SystemExit(f"unreadable range {spec!r} (expected {RANGE_SPELLINGS})")
    n = int(m.group(1))
    unit = m.group(2) or "d"
    days = n * 7 if unit.startswith("w") else n
    if days <= 0:
        raise SystemExit(f"range {spec!r} selects no days (expected {RANGE_SPELLINGS})")
    if days > 3650:
        raise SystemExit(
            f"range {spec!r} exceeds 3650 days — say `all` (expected {RANGE_SPELLINGS})"
        )
    # Midnight UTC, matching `datetime.rs::short_offset` -> `add_units` -> a
    # Date, which `parse_when` then combines with `midnight()`. The window is
    # therefore calendar-aligned, not rolling, and the page says so.
    since = (now - timedelta(days=days)).replace(
        hour=0, minute=0, second=0, microsecond=0
    )
    label = "last 1 day" if days == 1 else f"last {days} days"
    return Range(days, since, now, label, f"completed.after:-{days}d")


def parse_argv(argv: list[str]) -> tuple[str | None, str]:
    """`[filter] [--range SPEC]`. Unknown flags are refused, not ignored."""
    filter_: str | None = None
    spec = DEFAULT_RANGE
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--range":
            i += 1
            if i >= len(argv):
                raise SystemExit(f"--range needs a value ({RANGE_SPELLINGS})")
            spec = argv[i]
        elif a.startswith("--range="):
            spec = a.split("=", 1)[1]
        elif a.startswith("--"):
            raise SystemExit(
                f"unknown flag {a!r} (accepted: --range <{RANGE_SPELLINGS}>)"
            )
        elif filter_ is None:
            filter_ = a
        else:
            raise SystemExit(f"unexpected argument {a!r}: at most one filter is accepted")
        i += 1
    return filter_, spec


# ---------------------------------------------------------------------------
# Confidence — the WEAKEST input, not the alphabetically first
# ---------------------------------------------------------------------------
# The vocabulary is closed and lives in crates/tasqx-core/src/tokens.rs:40-43
# (`TOKEN_CONFIDENCE`, D34). This is the RANK over it, which core does not
# publish because nothing in core needed to order them.
#
# The bug this replaces: `sorted(conf)[0]`. Over exactly this vocabulary,
# sorted({'high','low','medium'})[0] == 'high' — so alphabetical order does not
# pick arbitrarily, it reliably picks the STRONGEST value. A task with one
# `high` transcript parse and one `low` directory-scan guess displayed HIGH. A
# total is only as trustworthy as its weakest input; docs/reporting-redesign.md
# §5 D-3 already argues exactly this for the aggregate metric.
_CONF_RANK = {"high": 0, "medium": 1, "low": 2}
# Below `low` on purpose: `require_confidence` should have refused anything
# outside the vocabulary at write time, so a value that got through means
# something upstream is broken. It must not be allowed to look like a grade.
_CONF_UNKNOWN_RANK = 3
CONF_UNKNOWN = "unknown"

_CONF_MEANING = {
    "high": "per-request samples bucketed into an exact window",
    "medium": "plausible but unverifiable (an agent's self-report)",
    "low": "a whole-session number attributed by fuzzy time-window overlap",
    CONF_UNKNOWN: "a confidence value outside the published vocabulary",
}


def weakest_confidence(values) -> str | None:
    """The weakest grade among `values`, or None if there are none."""
    worst_rank = -1
    worst = None
    for v in values:
        r = _CONF_RANK.get(v, _CONF_UNKNOWN_RANK)
        if r > worst_rank:
            worst_rank = r
            worst = v if r < _CONF_UNKNOWN_RANK else CONF_UNKNOWN
    return worst


def conf_badge(values) -> str:
    """The badge for a row whose total folds together `values`.

    When a row mixes grades the badge carries a `*` and names every grade in its
    title, so "weakest" reads as the FLOOR over several measurements rather than
    as the only measurement taken.
    """
    seen = [v for v in values if v is not None]
    worst = weakest_confidence(seen)
    if worst is None:
        return ""
    uniq = sorted(set(seen), key=lambda v: _CONF_RANK.get(v, _CONF_UNKNOWN_RANK))
    if len(uniq) > 1:
        tip = (
            f"weakest of {len(seen)} measurements ({', '.join(uniq)}) — a total is "
            "only as trustworthy as its weakest input"
        )
        mark = '<span class="mixed" aria-hidden="true">*</span>'
    else:
        tip = _CONF_MEANING.get(worst, _CONF_MEANING[CONF_UNKNOWN])
        mark = ""
    return (
        f'<span class="conf conf-{esc(worst)}" title="{esc(tip)}">{esc(worst)}{mark}</span>'
    )


# ---------------------------------------------------------------------------
# Timer history — telling "never timed" apart from "timed zero"
# ---------------------------------------------------------------------------
def timer_history(events: list) -> set:
    """Task ids that have EVER had a `start` event.

    `tracked` alone cannot distinguish "no timer ever ran" from "a timer ran and
    measured nothing" — both are PT0S, and both render as `—`. The event log
    can, and the difference is not academic: `tasqx start 21; tasqx start 22`
    silently stops #21, so #21 shows PT0S despite being the task actually worked
    for 15 minutes. Measured on this store: #12/#13 have a `done` event and no
    `start` at all (never timed); #21/#23 have `start` events and PT0S (timed,
    measured nothing). Four tasks, two completely different facts, one dash.
    """
    return {
        e["entity_id"]
        for e in events
        if e.get("entity") == "task" and e.get("op") == "start" and e.get("entity_id")
    }


def touched_in_range(events: list, rng: Range) -> set:
    """Task ids with a start / stop / done event inside the window.

    This is what makes "cost per task, in range" answerable at all. A completion
    bound alone would drop the task you started on Monday and have not finished,
    which is exactly the work a weekly review is about.
    """
    ops = {"start", "stop", "done"}
    return {
        e["entity_id"]
        for e in events
        if e.get("entity") == "task"
        and e.get("op") in ops
        and e.get("entity_id")
        and rng.covers(e.get("ts"))
    }


def hms(secs: int) -> str:
    """A duration that is never `—`.

    `humanize` integer-divides by 60, so PT1S renders as `0m` — a real
    measurement shown as zero (live task #24) — and PT0S renders as `—`, a zero
    shown as an absence. This formatter's only job is to state a number; the
    ABSENCE of a measurement is a different cell entirely (see `tracked_cell`).
    `humanize` is left alone because other panels own it.
    """
    if secs <= 0:
        return "0s"
    if secs < 60:
        return f"{secs}s"
    return humanize(secs)


def tracked_cell(secs: int, ever_started: bool) -> str:
    """THREE states, not two.

    The token side is careful to distinguish "not attributed" from zero. This is
    the same distinction on the time side, which did not have it.
    """
    if secs > 0:
        return f'<span class="tv">{esc(hms(secs))}</span>'
    if not ever_started:
        return (
            '<span class="tnever" title="No `start` event for this task anywhere in '
            "the log: it never had a timer at all. That is a different fact from a "
            'timer that measured nothing.">never timed</span>'
        )
    return (
        '<span class="tzero" title="A timer ran and recorded 0s. '
        "`tasqx start &lt;other&gt;` stops the running timer silently, so a worked "
        'interval can land on the wrong task.">0s &#9888;</span>'
    )


# ---------------------------------------------------------------------------
# Token aggregation — one implementation, four buckets, never blended
# ---------------------------------------------------------------------------
def token_agg(task: dict) -> dict:
    """Fold a task's measurements into the four buckets. Never into one number:
    `token_total` exists only to answer "did we find anything?", which is the
    same and only use `tokens.rs::TokenTotals::total` documents for it."""
    agg = {k: 0 for k, _l, _a, _b in BUCKETS}
    for m in task.get("tokens") or []:
        agg["tokens_in"] += m.get("input_tokens") or 0
        agg["tokens_out"] += m.get("output_tokens") or 0
        agg["tokens_cache_read"] += m.get("cache_read_tokens") or 0
        agg["tokens_cache_creation"] += m.get("cache_creation_tokens") or 0
    return agg


def token_total(task: dict) -> int:
    return sum(token_agg(task).values())


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


def token_tiles(tot: dict) -> str:
    """The four bucket tiles, evicted from the sticky header and re-homed next to
    the bars they describe. Never blended: four tiles, or — at zero — one em-dash
    tile. Four zeros would claim four measurements of zero, which is a different
    and false statement from "nothing was measured"."""
    grand = sum(tot.values())
    if grand <= 0:
        return (
            '<div class="tokrow tokrow-empty">'
            '<div class="tile"><span class="tn">&#8212;</span>'
            '<span class="tl">tokens</span>'
            '<span class="ts">not measured yet</span></div></div>'
        )
    cells = "".join(
        f'<div class="tile" data-bucket="{esc(k)}">'
        f'<span class="sw sw-{esc(k)}"></span>'
        f'<span class="tn">{esc(compact(tot.get(k, 0)))}</span>'
        f'<span class="tl">{esc(label)}</span>'
        f'<span class="ts">{tot.get(k, 0) / grand * 100:.1f}% of volume</span></div>'
        for k, label, _l, _d in BUCKETS
    )
    return f'<div class="tokrow">{cells}</div>'


def token_burn(summary: dict, rng: Range) -> str:
    """### Widget: Token burn by project.

    Question it answers: where did this range's tokens actually go, and which
    bucket dominates?
    Data source: report.summary group_by=project -> the four tokens_* metrics.
    Mark: four bucket tiles, then two horizontal stacked bars per project row —
    volume scaled to the largest project's raw tokens, cost scaled to the largest
    project's weighted tokens. 2px surface gap between segments.
    Empty state: one em-dash tile + "No token data in <range>" + config hint.
    Interaction: click a row -> filters the timeline below to that project.
    """
    allgroups = summary.get("groups", [])
    tot = {k: sum(g.get(k, 0) for g in allgroups) for k, _lab, _l, _d in BUCKETS}
    tiles = token_tiles(tot)

    groups = [g for g in allgroups if g.get("tokens_total", 0) > 0]
    if not groups:
        return section(
            "tokens",
            f"Token burn by project · {rng.label}",
            f"Where {rng.label} tokens actually went.",
            tiles
            + empty(
                f"No token data in {rng.label}.",
                "Token accounting is opt-in: `tasqx config set tokens.enabled true`, "
                "then run `tasqx daemon` so the attribution thread can reconstruct "
                "spend after each completion."
                " Or widen the window with `--range all`.",
            ),
        )

    groups.sort(key=lambda g: -g.get("tokens_total", 0))
    vol_scale = max(g["tokens_total"] for g in groups)

    # Weighted cost is computed for every row FIRST, because the cost bar is scaled
    # against the heaviest row rather than against itself.
    rows_data = []
    for g in groups:
        wmap = {k: g.get(k, 0) * wt for k, wt in WEIGHTS.items()}
        rows_data.append((g, wmap, sum(wmap.values())))
    cost_scale = max((wt for _g, _w, wt in rows_data), default=0.0)
    cost_grand = sum(wt for _g, _w, wt in rows_data)

    def segments(values: dict, denom: float, unit: str) -> str:
        out = []
        for key, label, _l, _d in BUCKETS:
            v = values.get(key, 0)
            if v <= 0:
                continue
            pct = v / denom * 100.0
            out.append(
                f'<span class="seg seg-{key}" style="flex:{pct:.4f} 1 0"'
                f' title="{esc(label)}: {pct:.1f}% of this row&#39;s {esc(unit)}"></span>'
            )
        return "".join(out)

    rows = []
    for g, weighted, wtotal in rows_data:
        proj = g.get("project") or "(none)"
        sid = slug(proj)
        total = g["tokens_total"]

        # TWO bars, not one. Looking at the rendered page is what forced this: with
        # cache read at 98% of volume, the other three buckets rendered at 7px, 2px
        # and 3px — three of the four buckets were visually nil, and the one claim
        # the panel exists to make ("volume is not cost") was carried only by a
        # footnote. The cost-weighted bar makes the argument the way a chart is
        # supposed to: by looking different from the one above it.
        vol_bar = (
            f'<span class="btrack vol"><span class="bbar"'
            f' style="width:{total / vol_scale * 100.0:.3f}%">'
            f'{segments(g, total, "volume")}</span></span>'
        )

        # The cost bar used to be width:100% by construction. Within a row that read
        # correctly; down the column it claimed every project cost the same, and it
        # claimed it before any label was read. It is now scaled to the heaviest
        # weighted row, so the geometry is true by default and the labels confirm
        # rather than correct it. Composition is untouched — the segment flex ratios
        # are still v/wtotal.
        if wtotal > 0 and cost_scale > 0:
            top = max(weighted, key=weighted.get)
            toplabel = next(l for k, l, _a, _b in BUCKETS if k == top)
            costnote = f"{toplabel} drives ~{weighted[top] / wtotal * 100.0:.0f}% of it"
            costshare = (wtotal / cost_grand * 100.0) if cost_grand > 0 else 0.0
            cost_bar = (
                f'<span class="btrack cost"><span class="bbar"'
                f' style="width:{wtotal / cost_scale * 100.0:.3f}%">'
                f'{segments(weighted, wtotal, "cost")}</span></span>'
            )
            cost_val = (
                f'<span class="bval2" title="share of this range&#39;s total'
                f' weighted cost">{costshare:.0f}%</span>'
            )
        else:
            costnote = ""
            cost_bar = '<span class="btrack cost"></span>'
            cost_val = '<span class="bval2"></span>'

        rows.append(
            f'<button type="button" class="brow" data-project="{esc(sid)}"'
            f' aria-pressed="false">'
            f'<span class="bname" title="{esc(proj)}">{esc(proj)}</span>'
            f'<span class="blabel">volume</span>{vol_bar}'
            f'<span class="bval">{esc(compact(total))}</span>'
            f'<span class="blabel2">cost</span>{cost_bar}{cost_val}'
            f'<span class="bnote">{esc(costnote)}</span>'
            "</button>"
        )

    scalenote = (
        '<p class="scalenote">Each bar is scaled to the largest project on its own '
        "axis: the top bar in raw tokens, the bottom in tokens weighted by the "
        "published relative prices (cache read 0.1×, cache write 1.25×, "
        "input 1×, output ~5×). Lengths are comparable down a column, "
        "never between the two bars of one row — they count different things. "
        "The percentage on the right is this project&#39;s share of the range&#39;s "
        "weighted cost.</p>"
    )

    return section(
        "tokens",
        f"Token burn by project · {rng.label}",
        "Tokens belong to the tasks that COMPLETED in this window — "
        "a task that closed here can carry a measurement created before it. Four "
        "buckets, never blended — cache read costs 0.1x input and cache write "
        "1.25x, so one total would lie. Click a row to filter the timeline.",
        tiles + legend() + f'<div class="bars">{"".join(rows)}</div>' + scalenote,
    )


# ---------------------------------------------------------------------------
# Drill-down reachability
# ---------------------------------------------------------------------------
# Every task in the export used to get a hidden detail panel: 24 panels in a
# 94 KB page, and >90% of those bytes were never displayed. At 500 tasks the
# export is a multi-megabyte document whose bulk is annotation walls nobody
# scrolls to. The panels that matter are the ones the page can actually reach,
# so the page COLLECTS its own links instead of guessing: every `#task-N`
# reference goes through TaskRefs, and task_details() renders exactly the set
# that came back. EVERY `#task-N` emitter calls refs.link() — a panel that links
# to a task any other way is the defect (drift guard §7.6). Both bounded link
# sources (the cost table by COST_ROW_LIMIT, the timeline by TIMELINE_CAP) keep
# panel count O(1) in store size rather than O(tasks).
COST_ROW_LIMIT = 60
PANEL_BUDGET = 160


class TaskRefs:
    """Renders every `#task-N` reference on the page and records which ones got
    a panel.

    First come, first served up to `budget`. Section order in render() is
    decision order — cost table, then the unattributed panel, then the timeline
    — so the budget is spent on the rows carrying the page's argument. Past the
    budget a reference degrades to inert text with a title explaining why, which
    is what keeps drift guard §7.6 ("every #task-N href resolves to a #task-N
    panel") true *by construction* rather than by a separate consistency pass:
    an href is only ever emitted for an id that is simultaneously added to
    `self.ids`.
    """

    def __init__(self, budget: int | None = None):
        # Read the module constant at call time, not at def time - a default
        # argument would freeze whatever PANEL_BUDGET was when the class body
        # ran and silently ignore any later override.
        self.budget = PANEL_BUDGET if budget is None else budget
        self.ids: list[str] = []
        self._seen: set[str] = set()
        self.dropped = 0

    def link(self, short_id, label: str | None = None, cls: str = "id") -> str:
        sid = str(short_id)
        text = f"#{sid}" if label is None else label
        if sid not in self._seen:
            if len(self.ids) >= self.budget:
                self.dropped += 1
                return (
                    f'<span class="{esc(cls)} nolink" title="No detail panel: '
                    f'this export renders {self.budget} panels and this task is '
                    f'outside them.">{esc(text)}</span>'
                )
            self._seen.add(sid)
            self.ids.append(sid)
        return f'<a class="{esc(cls)}" href="#task-{esc(sid)}">{esc(text)}</a>'

    def note(self) -> str:
        if not self.dropped:
            return ""
        return (
            f'<p class="clip">{self.dropped} further reference(s) on this page '
            f"have no detail panel — the export renders {self.budget}. "
            "Narrow the report filter to bring them into scope.</p>"
        )


def sort_th(key: str, label: str, cls: str = "", state: str = "none") -> str:
    """A sortable column header: aria-sort on the <th>, a real <button> inside
    it. The button is what carries the click and the focus ring; aria-sort is
    what a screen reader reads back off the column."""
    c = f"sortable {cls}".strip()
    return (
        f'<th class="{esc(c)}" data-key="{esc(key)}" aria-sort="{esc(state)}">'
        f'<button type="button" class="sortbtn">{esc(label)}'
        f'<span class="arw" aria-hidden="true"></span></button></th>'
    )


def cost_per_task(tasks: list, rng: Range, counts: dict, refs: "TaskRefs") -> str:
    """### Widget: Cost per task, in both currencies.

    Question it answers: what did this task cost me, in the stated range — in
    time and in tokens, how much do I trust each number, and which one took
    longest?
    Data source: store.export -> task.tracked_seconds + task.tokens[]; event.list
    -> which tasks were touched in range, and which ever had a timer at all.
    Mark: table, one row per task; a mono time column and a four-segment
    micro-bar per row; the row id is a link to that task's detail panel.
    Empty state: "No measured work in this range" + how to widen it.
    Interaction: click an id -> :target opens the task detail below. Click a
    column header -> the rows already in the DOM are reordered in place.

    ROW SELECTION, stated because a row set nobody can predict is a row set
    nobody can trust: a task appears if it was started, stopped or completed
    inside the window, or carries a measurement created inside it. Not a pure
    completion bound — that would drop the task you started on Monday and have
    not finished, which is the work a weekly review is most about.

    The old "skip rows with no tokens and no tracked time" gate is deliberately
    gone: it hid exactly the rows this panel now exists to show (a completion
    with no spend and no timer). On this store it hid 2 of 15.

    Sorting is why the row carries data-tracked / data-tokens: the sort keys are
    the raw integers, not the rendered "3h 20m" / "1.23M" strings, so no sort
    ever re-parses formatted text.
    """
    timed = counts["timed"]
    touched = counts["touched"]

    rows_in = []
    for t in tasks:
        in_range = t["id"] in touched or any(
            rng.covers(m.get("created")) for m in (t.get("tokens") or [])
        )
        if not in_range:
            continue
        agg = token_agg(t)
        rows_in.append((t, agg, sum(agg.values()), t.get("tracked_seconds", 0) or 0,
                        [m.get("confidence") for m in (t.get("tokens") or [])]))

    if not rows_in:
        return section(
            "cost", f"Cost per task · {rng.label}",
            f"Time and tokens, side by side — {rng.label}.",
            empty("No measured work in this range.",
                  "A task appears here once it is started, stopped or completed "
                  "inside the range. Widen it with `--range 90d` or `--range all`, "
                  "or start something with `tasqx start <id>`."),
        )

    # The cap ranks by BOTH currencies. A pure tokens-desc cut would hide the
    # longest-tracked task (interaction's own risk note) AND decapitate the
    # zero-token `never timed` rows this panel now exists to surface.
    n = len(rows_in)
    ord_tok = sorted(range(n), key=lambda i: (-rows_in[i][2], -rows_in[i][3]))
    ord_trk = sorted(range(n), key=lambda i: (-rows_in[i][3], -rows_in[i][2]))
    rank = {i: p for p, i in enumerate(ord_tok)}
    for p, i in enumerate(ord_trk):
        rank[i] = min(rank[i], p)
    keep = sorted(range(n), key=lambda i: (rank[i], -rows_in[i][2]))[:COST_ROW_LIMIT]
    clipped = n - len(keep)
    # Server-side order stays tokens-descending, so the page answers "what cost
    # most?" with JS disabled and the tokens <th> can ship aria-sort=descending.
    rows_in = [rows_in[i] for i in
               sorted(keep, key=lambda i: (-rows_in[i][2], -rows_in[i][3]))]
    scale = max((r[2] for r in rows_in), default=1) or 1

    rows = []
    for t, agg, total, tracked, conf in rows_in:
        sid = t["short_id"]
        if total > 0:
            segs = []
            for key, label, _l, _d in BUCKETS:
                v = agg[key]
                if v <= 0:
                    continue
                segs.append(
                    f'<span class="seg seg-{key}" style="flex:{v / total:.6f} 1 0"'
                    f' title="{esc(label)}: {esc(f"{v:,}")}"></span>'
                )
            bar = (f'<span class="btrack mini"><span class="bbar" '
                   f'style="width:{total / scale * 100.0:.3f}%">'
                   f'{"".join(segs)}</span></span>')
            tokcell = f'{bar}<span class="bval">{esc(compact(total))}</span>'
            badge = conf_badge(conf)   # WEAKEST, not sorted(conf)[0]
        else:
            tokcell = '<span class="muted">not attributed</span>'
            badge = ""

        rows.append(
            f'<tr data-id="{esc(sid)}"'
            f' data-project="{esc((t.get("project") or "").lower())}"'
            f' data-tracked="{int(tracked)}" data-tokens="{int(total)}">'
            f'<td class="id">{refs.link(sid)}</td>'
            f'<td class="ttl">{esc(t.get("title", ""))}</td>'
            f'<td class="chip">{esc(t.get("project") or "—")}</td>'
            f'<td class="num">{tracked_cell(tracked, t["id"] in timed)}</td>'
            f'<td class="tok">{tokcell}</td>'
            f'<td class="num">{badge}</td></tr>'
        )

    clipnote = (
        f'<p class="clip">Showing {COST_ROW_LIMIT} of {n} rows in range, ranked by '
        "whichever currency puts a task highest — tokens or tracked time. "
        "Sorting reorders these rows only.</p>"
        if clipped > 0 else ""
    )

    return section(
        "cost", f"Cost per task · {rng.label}",
        "Two currencies, never merged into one score. Rows are "
        "the tasks started, stopped or completed inside the range. Click a column "
        "header to sort. `tracked` says which of three things happened: a "
        "duration, `0s` (a timer ran and measured nothing — `tasqx start` on "
        "another task stops the running one silently), or `never timed` (no start "
        "event, ever). The confidence badge is the WEAKEST measurement folded into "
        "the row, not the best one.",
        legend()
        + '<div class="tablewrap"><table class="grid" id="costtable"><thead><tr>'
        + sort_th("id", "id")
        + "<th>task</th>"
        + sort_th("project", "project")
        + sort_th("tracked", "tracked", "num")
        + sort_th("tokens", "tokens", "", "descending")
        + '<th class="num">conf</th></tr></thead>'
        f"<tbody>{''.join(rows)}</tbody></table></div>{clipnote}",
    )


def unattributed(rng: Range, counts: dict, refs: "TaskRefs") -> str:
    """### Widget: Completions with no attribution.

    Question it answers: which completions in this range recorded no token spend
    at all — and therefore, how incomplete is every token number on this page?
    Data source: store.export -> tokens[]; event.list -> `tokens.attributed`.
    Mark: a headline count and two lists, because there are two populations and
    they need two different fixes.
    Empty state: NONE. At zero this renders nothing at all — no heading, no
    anchor. A panel that says "all good" every week is a panel nobody reads on
    the week it matters.

    A `tokens.attributed` event carrying `{"samples": 0}` was already on the page
    as a muted timeline footnote ("no spend found in window"). That was the right
    instinct at the wrong volume: it is not a footnote, it is the reader's next
    action. And the SECOND population below was not on the page at all — because
    the footnote is keyed off an event that, in that case, was never written.

    Measured on this store over 30 days: 15 completions, 12 with no spend — 10
    where attribution never ran, 2 where it ran and found nothing.

    Every id goes through `refs.link()` like every other `#task-N` emitter on the
    page: a raw href here would point at a panel the collector never registered,
    which is exactly the dangling-link failure drift guard §7.6 exists to catch.
    """
    done_range = counts["done_range"]
    attributed_ev = counts["attributed_ev"]

    ran_empty, never_ran = [], []
    for t in done_range:
        if token_total(t) > 0:
            continue
        (ran_empty if t["id"] in attributed_ev else never_ran).append(t)

    n = len(ran_empty) + len(never_ran)
    if n == 0:
        return ""

    def lst(items: list) -> str:
        return (
            '<ul class="ualist">'
            + "".join(
                f'<li>{refs.link(t["short_id"])}'
                f'<span class="ttl">{esc(t.get("title", ""))}</span>'
                f'<span class="chip">{esc(t.get("project") or "—")}</span>'
                f'<span class="when">{esc(pretty_ts(t.get("completed")))}</span></li>'
                for t in sorted(
                    items, key=lambda x: x.get("completed") or "", reverse=True
                )
            )
            + "</ul>"
        )

    cols = []
    if never_ran:
        cols.append(
            '<div class="uacol"><h3>attribution never ran '
            f'<span class="uan">{len(never_ran)}</span></h3>'
            "<p class=\"hint\">No <code>tokens.attributed</code> event was ever "
            "appended for these, so nothing even tried. The attribution thread was "
            "not listening when they closed: no <code>tasqx daemon</code> running, "
            "or <code>tokens.enabled</code> still false "
            "(<code>tasqx config set tokens.enabled true</code>).</p>"
            + lst(never_ran)
            + "</div>"
        )
    if ran_empty:
        cols.append(
            '<div class="uacol"><h3>attribution ran, found nothing '
            f'<span class="uan">{len(ran_empty)}</span></h3>'
            "<p class=\"hint\">A <code>tokens.attributed</code> event exists carrying "
            "<code>samples: 0</code>. Worth checking in this order: the transcript "
            "had not flushed to disk when attribution fired; the <code>done</code> "
            "event carried no correlation (client / session_id / transcript_path), "
            "leaving only a fuzzy directory scan, which matched nothing; or the work "
            "genuinely happened outside any instrumented tool.</p>"
            + lst(ran_empty)
            + "</div>"
        )

    return section(
        "unattributed",
        "Completions with no attribution",
        f"{rng.label} — these closed without recording a single token. Each one is a "
        "hole in the token panels above, so every total on this page is a LOWER "
        "BOUND until they are explained.",
        f'<div class="unattr"><p class="bignum">{n}'
        f'<span class="of">of {len(done_range)} completions in range</span></p>'
        f'<div class="uacols">{"".join(cols)}</div></div>',
    )


TIMELINE_CAP = 200


def timeline(events: list, tasks_by_id: dict, rng: Range, refs: "TaskRefs") -> str:
    """### Widget: Lifecycle timeline.

    Question it answers: what actually happened, in order, and how long did each
    working interval last?
    Data source: event.list -> ops start / stop / done / tokens.attributed,
    joined to store.export by entity_id.
    Mark: a day-grouped vertical list; one row per event; a rule per day.
    Theme roles: timer.active for start, accent for stop, urgency.ramp hot end
    for done, muted for tokens.attributed.
    Empty state: "No lifecycle events in <range>."
    Interaction: filtered live by the project row clicked above; each row links
    to its task's detail panel.

    The old `evs[:60]` slice was neither a window nor a stated limit, so a reader
    could not tell a quiet week from a truncated one. It is now windowed by the
    page's range and capped at a number the page prints.
    """
    keep = {"start", "stop", "done", "tokens.attributed"}
    evs = [
        e
        for e in events
        if e.get("op") in keep
        and e.get("entity") == "task"
        and rng.covers(e.get("ts"))
    ]
    if not evs:
        return section(
            "timeline",
            f"Timeline · {rng.label}",
            "Started, stopped, completed.",
            empty(
                f"No lifecycle events in {rng.label}.",
                "`tasqx start` / `tasqx stop` / `tasqx done` each append one event.",
            ),
        )

    evs.sort(key=lambda e: e.get("ts", ""), reverse=True)
    total = len(evs)
    evs = evs[:TIMELINE_CAP]
    overflow = (
        f'<p class="hint">Showing the {TIMELINE_CAP} most recent of {total} events '
        f"in {esc(rng.label)}.</p>"
        if total > TIMELINE_CAP
        else ""
    )

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
                refs.link(sid)
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
        f"Timeline · {rng.label}",
        "Started, stopped, completed — straight off the event log. "
        "Filtered by the project you select above.",
        '<div class="filterbar" id="filterbar" hidden>'
        '<span class="fnote">Filtered to <b id="fname"></b></span>'
        '<button type="button" id="fclear" class="fclear">clear</button></div>'
        f'{overflow}<div class="timeline">{"".join(out)}</div>',
    )


# ---------------------------------------------------------------------------
# Annotations
# ---------------------------------------------------------------------------
# Real annotation bodies in this store are walls of text hundreds of lines long.
# Dumping them raw made a detail panel unreadable, so they clamp to six lines
# with a disclosure. The clamp is pure CSS over a SINGLE copy of the text: a
# visually-hidden checkbox before the body, a <label> after it, and
# `.annx:checked ~ .body` lifting the clamp. Duplicating the text into a
# <summary> would have been the obvious <details> route and would have doubled
# the bytes of the exact content this page is already trying to shed — and a
# <summary> holding a 300-line body announces that entire body as the button's
# accessible name, which is worse than no affordance at all.
ANN_CLAMP_LINES = 6
ANN_CLAMP_CHARS = 420


def annotation_item(sid: str, i: int, a: dict) -> str:
    body = a.get("body", "") or ""
    lines = body.count("\n") + 1
    when = f'<span class="when">{esc(pretty_ts(a.get("created")))}</span>'
    if lines <= ANN_CLAMP_LINES and len(body) <= ANN_CLAMP_CHARS:
        # Short enough to show whole: no control, no clamp, no wasted bytes.
        return f'<li>{when}<div class="body">{esc(body)}</div></li>'
    bid = f"ab-{esc(sid)}-{i}"
    cid = f"ax-{esc(sid)}-{i}"
    return (
        f'<li class="clamped">{when}'
        f'<input class="annx" type="checkbox" id="{cid}" aria-controls="{bid}">'
        f'<div class="body clamp" id="{bid}">{esc(body)}</div>'
        f'<label class="annmore" for="{cid}">'
        f'<span class="more">Show all {lines} lines</span>'
        f'<span class="less">Show less</span></label></li>'
    )


def task_details(tasks: list, refs: "TaskRefs", timed_ids: set) -> str:
    """### Widget: Task detail panels (the drill-down target).

    Question it answers: what is this task, what did it cost, and what did I
    write down about it?
    Data source: store.export -> the full task object incl. annotations[] and
    tokens[].
    Mark: one panel per REACHABLE task (see TaskRefs), hidden until :target
    selects it.
    Interaction: reached by any `#task-<id>` link on the page; closing is a link
    back to `#cost` or the Escape key. No History API — pushState throws
    SecurityError on file:// (DESIGN.md §8a), so the reveal stays :target CSS.

    On keeping :target rather than moving to <details>/<summary>: :target is the
    only mechanism here that survives being mailed. `report.html#task-14` opens
    on the panel in a fresh window; a <details> cannot be addressed at all
    (fragment-triggered auto-expansion is recent and uneven, and it needs the
    target to be *inside* the element). <details> would also put one always-
    visible <summary> row per task at the foot of the document — 120 rows of
    visual noise where today there are zero — and it would still need the same
    JS to move focus, because the browser does not focus a details it expands
    for a fragment either. So :target stays, and the accessibility gap that made
    <details> tempting is closed directly: tabindex="-1" + role="region" +
    aria-labelledby here, focus() + a polite live region in the script.
    """
    if not refs.ids:
        return ""
    by_sid = {str(t["short_id"]): t for t in tasks}

    def order(s: str):
        return (0, int(s), "") if s.isdigit() else (1, 0, s)

    panels = []
    for sid in sorted(refs.ids, key=order):
        t = by_sid.get(sid)
        if t is None:
            # A reference to a task the export does not carry. Render the panel
            # anyway: an anchor that resolves to "out of scope" is a far better
            # failure than one that silently does nothing.
            panels.append(
                f'<article class="detail" id="task-{esc(sid)}" tabindex="-1"'
                f' role="region" aria-labelledby="task-{esc(sid)}-t">'
                f'<header><span class="id">#{esc(sid)}</span>'
                f'<h3 id="task-{esc(sid)}-t">Outside this report’s scope</h3>'
                f'<a class="close" href="#cost">close'
                f'<span class="vh"> task detail</span></a></header>'
                '<p class="muted small">This task is referenced by the page but '
                "is not in the current export. Widen the report filter to see "
                "it.</p></article>"
            )
            continue

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
                f" · {len(toks)} measurement(s)</p>"
            )
            tokblock = f'<div class="tkgrid">{cells}</div>{prov}'
        else:
            tokblock = (
                '<p class="muted small">No token measurement. A task is only '
                "attributed when its <code>done</code> event carries correlation "
                "(client / session_id / transcript_path).</p>"
            )

        if anns:
            items = "".join(annotation_item(sid, i, a) for i, a in enumerate(anns))
            annblock = f'<ul class="anns">{items}</ul>'
        else:
            annblock = '<p class="muted small">No annotations.</p>'

        tags = "".join(f'<span class="tag">{esc(g)}</span>' for g in t.get("tags", []))

        panels.append(
            f'<article class="detail" id="task-{esc(sid)}" tabindex="-1"'
            f' role="region" aria-labelledby="task-{esc(sid)}-t">'
            f'<header><span class="id">#{esc(sid)}</span>'
            f'<h3 id="task-{esc(sid)}-t">{esc(t.get("title", ""))}</h3>'
            f'<a class="close" href="#cost">close'
            f'<span class="vh"> task detail</span></a></header>'
            f'<dl class="meta">'
            f"<div><dt>status</dt><dd>{esc(t.get('status', ''))}</dd></div>"
            f"<div><dt>project</dt><dd>{esc(t.get('project') or '—')}</dd></div>"
            f"<div><dt>priority</dt><dd>{esc(t.get('priority') or '—')}</dd></div>"
            f"<div><dt>estimate</dt><dd>{esc(humanize(duration_secs(t.get('estimate'))))}</dd></div>"
            f"<div><dt>tracked</dt><dd>"
            f"{tracked_cell(t.get('tracked_seconds', 0) or 0, t['id'] in timed_ids)}"
            f"</dd></div>"
            f"<div><dt>urgency</dt><dd>{esc(t.get('urgency', '—'))}</dd></div>"
            f"</dl>"
            + (f'<div class="tags">{tags}</div>' if tags else "")
            + f"<h4>Token cost</h4>{tokblock}"
            + f"<h4>Annotations</h4>{annblock}"
            + "</article>"
        )
    return f'<div class="details">{"".join(panels)}{refs.note()}</div>'


# ---------------------------------------------------------------------------
# Inline SVG — sparkline of daily activity
# ---------------------------------------------------------------------------
def svg_activity(
    events: list, now: datetime, days: int = 21, capped: bool = False
) -> str:
    """Inline SVG, generated at render time so the chart is part of the DOCUMENT
    (mailable, printable, greppable) rather than something a runtime paints later.
    urgency.ramp is used as a real <linearGradient>, matching html.rs::ramp_stops.

    Axis policy: every day carries a number. The label step is derived from the
    real geometry, never hardcoded — see the block comment at `label_w` below.
    """
    days = max(1, int(days))
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

    if peak == 0:
        return (
            '<figure class="chart">'
            + empty(
                f"Nothing completed in the last {days} days.",
                "The bars appear as `tasqx done` events land.",
            )
            + "</figure>"
        )

    # Geometry, in viewBox user units. The SVG scales uniformly to its container,
    # so every collision test below is exact at any rendered width.
    w = 720.0
    pad_l, pad_r, pad_t = 26.0, 10.0, 8.0
    row_day, row_mon = 15.0, 14.0            # two label rows beneath the plot
    plot_w = w - pad_l - pad_r               # 684
    plot_h = 74.0
    h = pad_t + plot_h + row_day + row_mon   # 111
    slot = plot_w / days                     # 32.571 at days=21
    bar_w = min(slot * 0.62, 22.0)
    base = pad_t + plot_h
    y_day = base + row_day - 3.0
    y_mon = h - 3.0

    bars = []
    for i, v in enumerate(series):
        cx = pad_l + slot * (i + 0.5)
        bh = (v / peak) * plot_h if v else 0
        y = base - bh
        day = start + timedelta(days=i)
        if v:
            bars.append(
                f'<rect x="{cx - bar_w / 2:.1f}" y="{y:.1f}" width="{bar_w:.1f}" '
                f'height="{bh:.1f}" rx="4" fill="url(#ramp)"><title>'
                f"{day.isoformat()}: {v} done</title></rect>"
            )
        else:
            # 2px stub, not a gap: a measured zero must not look like missing data.
            bars.append(
                f'<rect x="{cx - bar_w / 2:.1f}" y="{base - 2:.1f}" '
                f'width="{bar_w:.1f}" height="2" rx="1" class="zero"><title>'
                f"{day.isoformat()}: 0 done</title></rect>"
            )

    # EVERY day gets a number, for as long as two numbers cannot touch. Both terms
    # are measured rather than assumed:
    #   advance - text.ax is ui-monospace at 11px and every font named in css() has
    #     an advance <= 0.6em (Cascadia .6, SF Mono .6, Consolas .55), so a
    #     two-digit label occupies 2 * 11 * 0.6 = 13.2u.
    #   gutter  - 4u of clear space, about a third of a glyph, so "07" "08" reads
    #     as two numbers and not as "0708".
    # At days=21, slot = 684/21 = 32.57u against a 17.2u requirement: every day is
    # labelled with 89% headroom. Break-even is slot >= 17.2u, i.e. days <= 39.
    # DEGRADATION, stated rather than silent: past 39 days the step grows by one
    # whole day at a time (40-79 -> every 2nd, 80-119 -> every 3rd, ...) and the
    # figcaption says so in words. The run is anchored on the LAST index so today
    # is always numbered and the dropped label is always the oldest one.
    LABEL_PX = 11.0
    ADVANCE = 0.6
    GUTTER = 4.0
    label_w = 2.0 * LABEL_PX * ADVANCE + GUTTER
    step = 1
    while slot * step < label_w:
        step += 1

    labels = "".join(
        f'<text class="ax" x="{pad_l + slot * (i + 0.5):.1f}" y="{y_day:.1f}" '
        f'text-anchor="middle">{(start + timedelta(days=i)).strftime("%d")}</text>'
        for i in sorted(range(days - 1, -1, -step))
    )

    # Day-of-month numbers stop being identifiers as soon as the range can repeat
    # one (any range longer than 28 days), so month starts are ruled and named.
    # Worth drawing at 21 days too: the old chart never said WHICH month it was.
    marks = []
    prev_key = None
    for i in range(days):
        d = start + timedelta(days=i)
        key = (d.year, d.month)
        if key != prev_key:
            marks.append((i, d))
            prev_key = key

    months = []
    for n, (i, d) in enumerate(marks):
        x = pad_l + slot * i
        if i > 0:
            months.append(
                f'<line class="mrule" x1="{x:.1f}" y1="{pad_t:.1f}" '
                f'x2="{x:.1f}" y2="{base + 4:.1f}"/>'
            )
        lbl = d.strftime("%b %Y") if n == 0 else d.strftime("%b")
        # text.mon is 10px monospace -> 6u per glyph. Keep the label inside the plot.
        tx = min(x + 3.0, pad_l + plot_w - len(lbl) * 6.0)
        months.append(
            f'<text class="mon" x="{max(tx, 0.0):.1f}" y="{y_mon:.1f}">{esc(lbl)}</text>'
        )

    last = start + timedelta(days=days - 1)
    aria = (
        f"Tasks completed per day, {start.strftime('%d %b %Y')} to "
        f"{last.strftime('%d %b %Y')}; peak {peak} in a day"
    )
    cap = (
        "One bar and one number per day; month starts are ruled."
        if step == 1
        else (
            f"{days} days will not carry a number each at this width "
            f"— one number every {step} days, ending today. Month starts are ruled."
        )
    )
    if capped:
        cap += (
            f" The page range is wider than this chart: it shows the most "
            f"recent {days} days."
        )

    # The scroll width is proportional to the day count, emitted inline rather
    # than pinned in the sheet: a fixed 38rem forced a phone scrollbar for seven
    # bars, and 90 days still has to scroll. No url(), still self-contained.
    min_rem = max(20.0, days * 1.05)

    return (
        '<figure class="chart"><div class="svgwrap">'
        f'<svg style="min-width:{min_rem:.0f}rem" viewBox="0 0 {w:.0f} {h:.0f}" '
        f'role="img" aria-label="{esc(aria)}">'
        '<defs><linearGradient id="ramp" x1="0" y1="1" x2="0" y2="0">'
        '<stop offset="0%" stop-color="var(--ramp0)"/>'
        '<stop offset="50%" stop-color="var(--ramp1)"/>'
        '<stop offset="100%" stop-color="var(--ramp2)"/>'
        "</linearGradient></defs>"
        f'<text class="ax" x="0" y="{pad_t + 9:.1f}">{peak}</text>'
        f'<text class="ax" x="0" y="{base:.1f}">0</text>'
        f'{"".join(months)}{"".join(bars)}{labels}</svg></div>'
        f"<figcaption>{esc(cap)}</figcaption></figure>"
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
    # The third rule dims the matching tile: with the tiles now sitting directly
    # above the bars they describe, toggling `cache read` off visibly empties the
    # bars while an undimmed tile still reads 13.63M.
    hides = "\n".join(
        f'.page[data-off~="{k}"] .seg-{k} {{ display: none; }}\n'
        f'.page[data-off~="{k}"] .lg[data-bucket="{k}"] {{ opacity: .45; '
        "text-decoration: line-through; }\n"
        f'.page[data-off~="{k}"] .tile[data-bucket="{k}"] {{ opacity: .45; }}'
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
/* The sticky header used to be 114px, then 181px at 420px. It is now four work
   tiles with a sub-line and no token tiles. Hardcoding a measured header height
   in three places is exactly how those numbers went stale, so the clearance is
   ONE variable. Re-measure --hh in the browser after any header change
   (DESIGN doc §8a).

   MEASURED in Chrome on the shipped fixture, 2026-07-25: 82.45px at 1576px
   viewport, 80.45px at 700px (stats do not wrap in the desktop branch), 73.28px
   in the <=600px branch. 5.5rem = 88px clears the 82.45px worst case by 5.5px,
   about one line of leading. The previous 6.5rem/8rem pair was a safe
   over-estimate carried in from the spec — safe, but it threw away 55px of a
   726px phone screen on every drill-down. */
:root {{ --hh: 5.5rem; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; background: var(--bg); color: var(--fg); line-height: 1.55;
  font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
.id, .num, .tm, .bval, .tkv, code, .mono {{ font-family: ui-monospace, "Cascadia Code", "SF Mono", Consolas, monospace; }}
main {{ max-width: 92ch; margin: 0 auto; padding: 1.5rem 1.25rem 4rem; }}
a {{ color: var(--accent); }}

/* The header shares main's measure. Full-bleed, the stats drifted to the far
   right of a 2055px viewport while the content column sat centred 640px away —
   they read as belonging to different pages. */
/* Four work tiles instead of eight mixed ones, on a tighter vertical rhythm.
   The four token buckets left this row for §tokens, beside the bars that
   decompose them. */
header.summary {{ position: sticky; top: 0; z-index: 5; border-bottom: 1px solid var(--line);
  background: var(--bg); }}
header.summary > .hwrap {{ max-width: 92ch; margin: 0 auto; padding: .7rem 1.25rem;
  display: flex; align-items: baseline; justify-content: space-between;
  gap: .5rem 1rem; flex-wrap: wrap; }}
.brand {{ font-weight: 700; font-size: 1.05rem; letter-spacing: -.01em; }}
.brand span {{ font-weight: 400; color: var(--muted); }}
.stats {{ display: flex; gap: 1.25rem; flex-wrap: wrap; }}
.stat {{ text-align: right; }}
.stat .n {{ font-size: 1.3rem; font-weight: 700; line-height: 1.1;
  font-variant-numeric: tabular-nums; font-family: ui-monospace, monospace; }}
.stat .l {{ font-size: .68rem; color: var(--muted); text-transform: uppercase;
  letter-spacing: .06em; }}
.stat .s {{ font-size: .7rem; color: var(--muted); }}
.stat.flag .n {{ color: var(--danger); }}

section {{ margin-top: 2.4rem; scroll-margin-top: var(--hh); }}
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

/* The four bucket tiles, re-homed from the sticky header into §tokens.
   Still four tiles; nothing here sums them. */
.tokrow {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(9.5rem, 1fr));
  gap: .5rem; margin: 0 0 .9rem; }}
.tokrow-empty {{ grid-template-columns: minmax(0, 18rem); }}
.tile {{ display: grid; grid-template-columns: auto minmax(0, 1fr);
  grid-template-areas: "sw n" "l l" "s s"; align-items: center; gap: .05rem .45rem;
  background: var(--card); border: 1px solid var(--line); border-radius: 10px;
  padding: .5rem .7rem; }}
.tile .sw {{ grid-area: sw; }}
.tile .tn {{ grid-area: n; font-family: ui-monospace, monospace; font-size: 1.15rem;
  font-weight: 700; line-height: 1.15; font-variant-numeric: tabular-nums; }}
.tile .tl {{ grid-area: l; font-size: .68rem; text-transform: uppercase;
  letter-spacing: .06em; color: var(--muted); }}
.tile .ts {{ grid-area: s; font-size: .7rem; color: var(--muted); }}

.bars {{ display: flex; flex-direction: column; gap: .35rem; }}
/* Two changes over the original: a v2 cell for the cost-share percentage, and
   minmax(0,1fr) on the bar column so it can actually shrink instead of forcing
   an overflow. */
.brow {{ display: grid; grid-template-columns: 16ch 6.5ch minmax(0, 1fr) 7ch;
  grid-template-areas: "n l1 b1 v" ". l2 b2 v2" ". . note note";
  gap: .25rem .7rem; align-items: center; width: 100%; text-align: left; font: inherit;
  color: inherit; background: none; border: 0; border-radius: 8px; padding: .55rem .5rem;
  cursor: pointer; }}
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
.bname {{ grid-area: n; font-weight: 600; font-size: .87rem; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
.btrack {{ background: var(--card); border-radius: 5px; overflow: hidden; }}
/* The cost bar is now scaled to the heaviest weighted project rather than pinned
   at 100%, so a light project renders genuinely short. 2px keeps a non-zero row
   visible without inflating it back into a lie. */
.bbar {{ display: flex; height: 20px; gap: 2px; min-width: 2px; }}
/* td.tok is a flex row, so without a basis the track collapses to its content
   and the four segments render ~2px wide each — measured 14px total in the
   browser before this line existed. Found by looking at the page, not by any
   structural check. */
.btrack.mini {{ flex: 1 1 auto; min-width: 8rem; }}
.btrack.mini .bbar {{ height: 12px; }}
.seg {{ min-width: 2px; }}
.seg:first-child {{ border-radius: 4px 0 0 4px; }}
.seg:last-child {{ border-radius: 0 4px 4px 0; }}
.bval {{ grid-area: v; text-align: right; font-size: .82rem;
  font-variant-numeric: tabular-nums; }}
.bval2 {{ grid-area: v2; text-align: right; font-size: .78rem; color: var(--muted);
  font-variant-numeric: tabular-nums; font-family: ui-monospace, monospace; }}
.bnote {{ grid-area: note; font-size: .72rem; color: var(--muted); }}
/* The two bars now have two different denominators. This says so. */
.scalenote {{ color: var(--muted); font-size: .75rem; margin: .7rem 0 0; max-width: 72ch; }}

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
li.ev .ttl {{ flex: 1; min-width: 14ch; overflow-wrap: anywhere; }}
li.ev .det {{ font-size: .76rem; color: var(--muted); }}
.page[data-filter] li.ev {{ display: none; }}
.page[data-filter] li.ev.match {{ display: flex; }}

.detail header {{ display: flex; align-items: baseline; gap: .6rem; }}
.detail header h3 {{ margin: 0; font-size: 1rem; text-transform: none; letter-spacing: 0; color: var(--fg); flex: 1; }}
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
.tags {{ display: flex; gap: .35rem; flex-wrap: wrap; margin-top: .6rem; }}
.tag {{ font-size: .74rem; border: 1px solid var(--line); border-radius: 999px; padding: .05rem .55rem; color: var(--muted); }}

/* text.ax goes 10px -> 11px: inside the phone-width figure a 10px viewBox label
   rendered at ~8.4 CSS px. The 11px also feeds the label-step maths in
   svg_activity() — change one and you must change the other.
   The scroll lives on .svgwrap, not on figure, so the figcaption keeps the
   figure's width and wraps normally. The svg's min-width is emitted INLINE and
   is proportional to the day count (max(20rem, days * 1.05rem)): a fixed 38rem
   forced a phone scrollbar for a seven-bar chart while still crushing 90. */
figure.chart {{ margin: 0; border: 1px solid var(--line); border-radius: 12px;
  background: var(--bg); padding: .8rem; max-width: 100%; }}
figure.chart .svgwrap {{ overflow-x: auto; }}
figure.chart svg {{ display: block; width: 100%; min-width: 0; height: auto; }}
figure.chart figcaption {{ color: var(--muted); font-size: .74rem; margin-top: .45rem; }}
text.ax {{ fill: var(--muted); font-size: 11px; font-family: ui-monospace, monospace; }}
text.mon {{ fill: var(--muted); font-size: 10px; font-family: ui-monospace, monospace; }}
line.mrule {{ stroke: var(--line); stroke-width: 1; }}
rect.zero {{ fill: var(--line); }}

/* --- utility ------------------------------------------------------------ */
.vh {{ position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
  overflow: hidden; clip-path: inset(50%); white-space: nowrap; border: 0; }}
p.clip {{ font-size: .78rem; color: var(--muted); margin: .55rem 0 0; }}
.nolink {{ color: var(--muted); border-bottom: 1px dotted var(--line); cursor: help; }}

/* --- sortable columns --------------------------------------------------- */
/* The <th> carries aria-sort (what a screen reader reads off the column); the
   <button> inside carries the click target and the focus ring. Padding moves
   from the th to the button so the whole cell is clickable. */
table.grid th.sortable {{ padding: 0; }}
.sortbtn {{ font: inherit; font-weight: inherit; color: inherit; background: none;
  border: 0; margin: 0; padding: .4rem .5rem; border-radius: 6px; cursor: pointer;
  text-transform: inherit; letter-spacing: inherit;
  display: inline-flex; align-items: center; gap: .3rem; }}
.sortbtn:hover {{ color: var(--fg); background: var(--card); }}
.sortbtn:focus-visible {{ outline: 2px solid var(--accent); outline-offset: -2px; }}
.arw::after {{ content: "\\2195"; opacity: .35; }}
th[aria-sort="ascending"] .arw::after {{ content: "\\2191"; opacity: 1; }}
th[aria-sort="descending"] .arw::after {{ content: "\\2193"; opacity: 1; }}
th[aria-sort="ascending"], th[aria-sort="descending"] {{ color: var(--fg); }}

/* --- detail panels ------------------------------------------------------ */
/* scroll-margin lives on the panel itself, not on :target - it must apply
   before the jump. The clearance is --hh, one variable, because hardcoding a
   measured header height in three places is exactly how 5rem then 8rem went
   stale (DESIGN doc §8a). Re-measure --hh in the browser after any header
   change. */
.details .detail {{ display: none; scroll-margin-top: var(--hh); }}
.details .detail:target {{ display: block; border: 1px solid var(--accent); border-radius: 12px;
  background: var(--card); padding: 1rem 1.1rem; margin-top: 2rem; }}
/* The panel is focused programmatically (script step 3). Suppress the plain
   focus ring - the accent border IS the "you are here" - but keep a real one
   for keyboard navigation, where the ring is the only positional feedback. */
.details .detail:focus {{ outline: none; }}
.details .detail:focus-visible {{ outline: 3px solid var(--accent); outline-offset: 3px; }}
.detail .close {{ font-size: .78rem; white-space: nowrap; }}

/* --- annotations: clamp to six lines, one copy of the text --------------- */
/* The checkbox sits BEFORE the body so `:checked ~ .body` can reach it; it is
   clipped rather than display:none so it stays focusable and Space still
   toggles it. The fade is a gradient mask, so it needs no knowledge of the
   body's height and vanishes automatically when the clamp lifts. */
ul.anns li {{ position: relative; border-left: 2px solid var(--line);
  padding: .3rem 0 .6rem .8rem; margin-bottom: .3rem; }}
ul.anns .when {{ display: block; font-size: .72rem; color: var(--muted); }}
ul.anns .body {{ white-space: pre-wrap; font-size: .87rem; overflow-wrap: anywhere; }}
ul.anns .body.clamp {{ display: -webkit-box; -webkit-box-orient: vertical;
  -webkit-line-clamp: 6; line-clamp: 6; overflow: hidden;
  -webkit-mask-image: linear-gradient(to bottom, #000 78%, transparent 100%);
  mask-image: linear-gradient(to bottom, #000 78%, transparent 100%); }}
.annx {{ position: absolute; width: 1px; height: 1px; margin: 0; opacity: 0; }}
.annx:checked ~ .body.clamp {{ display: block; -webkit-line-clamp: none; line-clamp: none;
  overflow: visible; -webkit-mask-image: none; mask-image: none; }}
.annmore {{ display: inline-block; margin-top: .3rem; font-size: .74rem; color: var(--accent);
  cursor: pointer; border-bottom: 1px dotted currentColor; }}
.annmore .less, .annx:checked ~ .annmore .more {{ display: none; }}
.annx:checked ~ .annmore .less {{ display: inline; }}
.annx:focus-visible ~ .annmore {{ outline: 2px solid var(--accent); outline-offset: 2px; }}

/* --- The stated range ---------------------------------------------------
   Shares main's measure for the reason header.summary already does: at 2055px
   a full-bleed band would sit 600px away from the column it describes.
   Emitted OUTSIDE the sticky header: two or three wrapped rows of italic prose
   inside a position:sticky element re-breaks the scroll-margin this file has
   already fixed twice. */
.rangeband {{ max-width: 92ch; margin: 0 auto; padding: 0 1.25rem .7rem;
  display: flex; align-items: baseline; gap: .35rem 1rem; flex-wrap: wrap; font-size: .78rem; }}
.rangeband .rk {{ text-transform: uppercase; letter-spacing: .06em; font-size: .64rem;
  color: var(--muted); }}
.rangeband b {{ font-size: .9rem; }}
.rangeband .rs {{ color: var(--muted); }}
/* The caveats get their own row so they read as prose, not as another chip. */
.rangeband .rn {{ flex-basis: 100%; color: var(--muted); font-style: italic; }}
.rangeband code {{ font-size: .74rem; background: var(--card); border: 1px solid var(--line);
  border-radius: 5px; padding: .02rem .35rem; }}

/* --- Confidence: the WEAKEST input ---------------------------------------
   Four states, and `low` must not be able to pass for `high` at a glance.
   No new colour tokens: these are the semantic roles already in the sheet, so
   the validated four-bucket chart palette is untouched. */
.conf-high {{ color: var(--ok); border-color: var(--ok); }}
.conf-medium {{ color: var(--fg); border-color: var(--muted); }}
.conf-low {{ color: var(--danger); border-color: var(--danger); }}
/* Dashed, because `unknown` is not a grade — it means a value got past
   tokens.rs::require_confidence and something upstream is broken. */
.conf-unknown {{ color: var(--danger); border-color: var(--danger); border-style: dashed; }}
.conf .mixed {{ margin-left: .12rem; font-weight: 700; }}

/* --- Tracked: three states, not two -------------------------------------- */
.tv {{ font-variant-numeric: tabular-nums; }}
.tnever {{ color: var(--muted); font-size: .78rem; font-style: italic; }}
.tzero {{ color: var(--danger); font-size: .8rem; white-space: nowrap; }}

/* --- Completions with no attribution -------------------------------------
   Bordered in --danger because the panel only exists when there is something
   to fix; at zero the section is not rendered at all. */
.unattr {{ border: 1px solid var(--danger); border-radius: 12px; background: var(--card);
  padding: 1rem 1.1rem; }}
.unattr .bignum {{ margin: 0 0 .8rem; font-family: ui-monospace, monospace; font-size: 2.1rem;
  font-weight: 700; line-height: 1; color: var(--danger); font-variant-numeric: tabular-nums; }}
/* "12" alone cannot say whether that is most of them or a rounding error. */
.unattr .bignum .of {{ font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  font-size: .82rem; font-weight: 400; color: var(--muted); margin-left: .5rem; }}
.uacols {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(26ch, 1fr));
  gap: 1rem 1.8rem; }}
.uacol h3 {{ margin-top: 0; color: var(--fg); text-transform: none; letter-spacing: 0;
  font-size: .84rem; }}
.uacol .uan {{ font-family: ui-monospace, monospace; color: var(--danger); margin-left: .3rem; }}
.uacol .hint {{ color: var(--muted); font-size: .8rem; margin: 0 0 .6rem; }}
ul.ualist {{ list-style: none; margin: 0; padding: 0; }}
ul.ualist li {{ display: flex; align-items: baseline; gap: .5rem; flex-wrap: wrap;
  padding: .28rem 0; border-top: 1px solid var(--line); font-size: .84rem; }}
ul.ualist .ttl {{ flex: 1; min-width: 14ch; }}
ul.ualist .chip, ul.ualist .when {{ color: var(--muted); font-size: .74rem; }}

footer {{ max-width: 92ch; margin: 0 auto; padding: 1rem 1.25rem 3rem; color: var(--muted); font-size: .8rem; }}

/* Responsive. Verified target: 420px, body must not scroll.
   .brow's fixed px/ch columns left the bar column at ~133px, so four segments
   became slivers. Below 600px the row stacks: name and volume figure on one
   line, then label/bar pairs at full container width, then the note. */
@media (max-width: 600px) {{
  :root {{ --hh: 5rem; }}   /* measured 73.28px; 80px clears it by 6.7px */
  main {{ padding: 1rem .85rem 3rem; }}
  header.summary > .hwrap {{ padding: .6rem .85rem; gap: .3rem .8rem; }}
  .stats {{ gap: .8rem 1rem; }}
  .stat .n {{ font-size: 1.1rem; }}
  .stat .l {{ font-size: .62rem; letter-spacing: .04em; }}
  .stat .s {{ font-size: .62rem; }}

  .tokrow {{ grid-template-columns: repeat(auto-fit, minmax(7.5rem, 1fr)); gap: .4rem; }}
  .tile {{ padding: .4rem .55rem; }}
  .tile .tn {{ font-size: 1rem; }}

  .brow {{ grid-template-columns: minmax(0, 1fr) auto;
    grid-template-areas: "n v" "l1 l1" "b1 b1" "l2 v2" "b2 b2" "note note";
    gap: .1rem .6rem; padding: .6rem .35rem; align-items: baseline; }}
  .bname {{ white-space: normal; overflow: visible; text-overflow: clip;
    overflow-wrap: anywhere; }}
  .blabel, .blabel2 {{ text-align: left; margin-top: .3rem; }}
  .bnote {{ margin-top: .25rem; }}
  .bbar {{ height: 18px; }}
  .btrack.cost .bbar {{ height: 14px; }}

  figure.chart {{ padding: .55rem; }}
  .scalenote {{ font-size: .72rem; }}

  /* The regions the cost table and the detail panels own: without these they
     keep desktop metrics on a phone while everything around them shrinks. */
  table.grid {{ font-size: .82rem; }}
  table.grid td, table.grid th {{ padding: .35rem .4rem; }}
  .sortbtn {{ padding: .3rem .4rem; }}
  .detail header {{ flex-wrap: wrap; }}
  dl.meta {{ gap: .3rem .9rem; }}
  .details .detail:target {{ padding: .8rem .85rem; }}
  ul.ualist li {{ gap: .35rem; }}
}}

/* ONE print block. Paper has no scrollbar and no disclosure: everything is
   shown, nothing is masked, nothing is clipped at a container edge. */
@media print {{
  header.summary {{ position: static; }}
  .details .detail {{ display: block; }}
  ul.anns .body.clamp {{ display: block; -webkit-line-clamp: none; line-clamp: none;
    overflow: visible; -webkit-mask-image: none; mask-image: none; }}
  .annx, .annmore, .arw {{ display: none; }}
  .sortbtn {{ padding: 0; }}
  li.ev .ttl {{ min-width: 0; }}
  figure.chart .svgwrap {{ overflow: visible; }}
  /* !important is load-bearing: svg_activity emits min-width INLINE (it is
     proportional to the day count), and an inline declaration outranks any
     author rule without it. Without this the chart clips at the paper edge
     instead of fitting — .svgwrap is overflow:visible in print, so there is
     no scrollbar to rescue it. */
  figure.chart svg {{ min-width: 0 !important; }}
  .tokrow {{ grid-template-columns: repeat(4, 1fr); }}
  .unattr {{ break-inside: avoid; }}
}}
"""


# ---------------------------------------------------------------------------
# The whole page
# ---------------------------------------------------------------------------
SCRIPT = """
// The entire interaction budget of this page, in vanilla JS.
// The History API is deliberately untouched: its state-pushing methods throw
// SecurityError on a file:// document (origin null), which is exactly how this
// page is opened (DESIGN.md 8a). Drill-down stays :target CSS; the JS below only
// moves FOCUS to what :target already revealed - the one part of a drill-down
// that CSS cannot do. Everything else is attribute flips; CSS renders.
(function () {
  var page = document.querySelector('.page');
  var live = document.getElementById('live');
  var $ = function (id) { return document.getElementById(id); };

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
  var bar = $('filterbar'), fname = $('fname'), fclear = $('fclear');
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
  if (bar) {
    page.addEventListener('click', function (e) {
      var row = e.target.closest('.brow');
      if (!row) return;
      var active = row.getAttribute('aria-pressed') === 'true';
      applyFilter(active ? null : row.dataset.project,
                  active ? '' : row.querySelector('.bname').textContent);
    });
    fclear.addEventListener('click', function () { applyFilter(null, ''); });
  }

  // 3. Drill-down focus. :target reveals the panel, but a keyboard or
  //    screen-reader user is told nothing and the caret stays put, so Tab then
  //    walks the page BEHIND the panel that just opened. Move focus to it and
  //    name it. Runs on load too, so a mailed #task-N deep link lands correctly.
  function land() {
    var el = $(location.hash.slice(1));
    if (!el) return;
    if (!el.hasAttribute('tabindex')) el.setAttribute('tabindex', '-1');
    el.focus();
    // The panel is a named region, so focus alone already announces its title.
    // The live line adds only what focus cannot say: how to get back out.
    var h = el.querySelector('h2, h3');
    if (live) {
      live.textContent = el.classList.contains('detail')
        ? 'Task detail opened. Press Escape to close.'
        : (h ? h.textContent : el.id);
    }
  }
  window.addEventListener('hashchange', land);
  if (location.hash) land();
  document.addEventListener('keydown', function (e) {
    // Assigning location.hash is a navigation, not History state - file:// safe.
    if (e.key === 'Escape' && location.hash.slice(0, 6) === '#task-') {
      location.hash = '#cost';
    }
  });

  // 4. Sort the cost table in place. Every key is a data-* number already on the
  //    row, so nothing is re-parsed and no cell is rewritten - only row order.
  var tbl = $('costtable');
  if (tbl) tbl.addEventListener('click', function (e) {
    var btn = e.target.closest('.sortbtn');
    if (!btn) return;
    var th = btn.parentNode, k = th.dataset.key;
    var dir = th.getAttribute('aria-sort') === 'descending' ? 1 : -1;
    var body = tbl.tBodies[0], rows = [].slice.call(body.rows);
    rows.sort(function (x, y) {
      var a = x.dataset[k], b = y.dataset[k], n = a - b;
      return (n === n ? n : String(a).localeCompare(String(b))) * dir;  // n!==n is NaN
    });
    rows.forEach(function (r) { body.appendChild(r); });
    [].forEach.call(th.parentNode.children, function (c) {
      if (c.dataset.key) c.setAttribute('aria-sort', 'none');
    });
    th.setAttribute('aria-sort', dir < 0 ? 'descending' : 'ascending');
    if (live) {
      live.textContent = 'Sorted by ' + k + ', ' +
                         (dir < 0 ? 'highest first' : 'lowest first') + '.';
    }
  });
})();
"""


OPEN_STATES = ("pending", "active", "waiting")


def derive_counts(tasks: list, events: list, summary_now: dict, rng: Range) -> dict:
    """Every count the page states, in one place, each tagged NOW or RANGE.

    Lifting this out of `render` is the point: the window is auditable here and
    invisible when the counts are inlined among markup.
    """
    timed = timer_history(events)
    touched = touched_in_range(events, rng)
    # "Was attribution ever ATTEMPTED for this task" is a lifetime question, not
    # a windowed one — the marker may land seconds after the range boundary.
    attributed_ev = {
        e["entity_id"]
        for e in events
        if e.get("entity") == "task"
        and e.get("op") == "tokens.attributed"
        and e.get("entity_id")
    }

    # --- NOW: the backlog. Not windowed, and not windowABLE by completion. ---
    open_n = sum(1 for t in tasks if t.get("status") in OPEN_STATES)
    # Core's own overdue rule (reports.rs:142), not a second implementation of it
    # in the presentation layer. Read off the UNWINDOWED summary: a windowed one
    # answers 0 for the structural reason `gather` documents.
    overdue_n = sum(g.get("overdue", 0) for g in summary_now.get("groups", []))

    # --- RANGE: the flow. ----------------------------------------------------
    done_range = [
        t for t in tasks if t.get("status") == "done" and rng.covers(t.get("completed"))
    ]
    tracked_range = sum(t.get("tracked_seconds", 0) or 0 for t in done_range)
    zero_timed = sum(
        1
        for t in done_range
        if (t.get("tracked_seconds", 0) or 0) <= 0 and t["id"] in timed
    )
    never_timed = sum(
        1
        for t in done_range
        if (t.get("tracked_seconds", 0) or 0) <= 0 and t["id"] not in timed
    )
    measured = sum(1 for t in done_range if token_total(t) > 0)

    # Windowing by COMPLETION is not the same as windowing by SPEND: a task that
    # closed inside the range can carry a measurement created before it, whose
    # tokens then count toward a window they were not spent in. Counted so the
    # page can say so — and rendered only when non-zero, because a caveat that is
    # always on screen stops being read.
    stale = 0
    if rng.since is not None:
        for t in done_range:
            for m in t.get("tokens") or []:
                c = parse_ts(m.get("created"))
                if c is not None and c <= rng.since:
                    stale += 1

    return {
        "open": open_n,               # NOW
        "overdue": overdue_n,         # NOW
        "done_range": done_range,     # RANGE
        "done_n": len(done_range),    # RANGE
        "tracked_range": tracked_range,
        "zero_timed": zero_timed,
        "never_timed": never_timed,
        "measured": measured,
        "stale_measurements": stale,
        "timed": timed,
        "touched": touched,
        "attributed_ev": attributed_ev,
    }


def render(data: dict) -> str:
    rng: Range = data["range"]
    now = rng.now
    tasks = data["export"].get("tasks", [])
    by_id = {t["id"]: t for t in tasks}
    events = data["events"].get("events", [])
    c = derive_counts(tasks, events, data["summary_now"], rng)

    # Emission order is load-bearing: EVERY #task-N emitter must run before
    # task_details() reads the set they filled, and the ones carrying the page's
    # argument must run first so they win the panel budget.
    refs = TaskRefs()
    cost_html = cost_per_task(tasks, rng, c, refs)
    unattr_html = unattributed(rng, c, refs)
    timeline_html = timeline(events, by_id, rng, refs)
    details_html = task_details(tasks, refs, c["timed"])

    # FOUR work stats, all about NOW-or-RANGE and tagged as such. The four token
    # buckets are NOT here any more — they live in token_tiles() inside §tokens,
    # beside the bars that decompose them. Still four, still never blended.
    head = (
        '<header class="summary"><div class="hwrap">'
        '<div class="brand">tasqx <span>review</span></div>'
        f'<div class="stats">{stat(str(c["open"]), "open", "now")}'
        f'{stat(str(c["done_n"]), "done", "in range")}'
        f'{stat(hms(c["tracked_range"]), "tracked", "in range")}'
        f'{stat(str(c["overdue"]), "overdue", "now", flag=c["overdue"] > 0)}'
        "</div></div></header>"
    )

    scope = (
        f'<span class="rk">scope</span><code>{esc(data["filter"])}</code>'
        if data.get("filter") else ""
    )
    stale = (
        f'<span class="rn">{c["stale_measurements"]} measurement(s) predate the '
        "window but belong to tasks that closed inside it, so they count toward a "
        "window they were not spent in</span>"
        if c["stale_measurements"] else ""
    )
    band = (
        '<div class="rangeband">'
        f'<span class="rk">range</span><b>{esc(rng.label)}</b>'
        f'<span class="rs">{esc(rng.boundary())}'
        + (f' · <code>{esc(rng.clause)}</code>' if rng.clause else "")
        + "</span>" + scope
        + '<span class="rn">open and overdue are as of NOW, not windowed: no '
        "completion bound can select a task that never completed "
        "(filter.rs::instant_cmp)</span>" + stale + "</div>"
    )

    body = [
        head,
        band,                       # OUTSIDE the sticky header — see --hh below
        "<main>",
        '<p class="vh" id="live" role="status" aria-live="polite"></p>',
        section(
            "activity",
            f"Completions, {rng.label}",
            f'{c["measured"]} of {c["done_n"]} completions in range carry a token '
            f'measurement · {c["never_timed"]} were never timed, '
            f'{c["zero_timed"]} had a timer that recorded 0s.',
            svg_activity(events, now, rng.bucket_days, capped=rng.chart_capped),
        ),
        token_burn(data["summary_window"], rng),
        cost_html,
        unattr_html,
        timeline_html,
        details_html,
        "</main>",
        f'<footer>Generated {esc(pretty_ts(now.isoformat()))} · range '
        f"{esc(rng.label)} ({esc(rng.boundary())}) · "
        "backlog figures are as of generation time · "
        "every panel is a pure read of the tasqx core API · "
        "prototype for docs/reporting-redesign.md</footer>",
    ]

    return (
        '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>tasqx report · {esc(rng.label)} · redesign prototype</title>\n"
        f"<style>\n{css()}</style>\n</head>\n"
        f'<body>\n<div class="page">\n{"".join(body)}\n</div>\n'
        f"<script>{SCRIPT}</script>\n</body>\n</html>\n"
    )


def main() -> None:
    filter_, spec = parse_argv(sys.argv[1:])
    rng = parse_range(spec, datetime.now(timezone.utc))
    sys.stdout.write(render(gather(filter_, rng)))


if __name__ == "__main__":
    main()
