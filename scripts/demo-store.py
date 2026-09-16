#!/usr/bin/env python3
"""Build a demo store for screenshots: fictional work with a few months of history.

    scripts/demo-store.py                  # writes target/demo/tasks.db
    TASQX=./target/debug/tasqx scripts/demo-store.py

then render it, with the demo's own config so the machine's does not leak in:

    export TASQX_DB=$PWD/target/demo/tasks.db TASQX_CONFIG_DIR=$PWD/target/demo/config
    scripts/snap.sh list 100 -- --no-daemon list
    scripts/snap-tui.sh dashboard 132 36 -- --no-daemon dashboard

Why it exists: the README's screenshots have to come from somewhere, and a real
store is somebody's actual work, which does not belong on a public landing
page. Everything here is invented, and every date is relative to today, so the
pictures look current whenever they are regenerated.

Set TASQX_NOW to an RFC 3339 instant (2026-09-16T09:00:00Z) and "today" becomes
that day instead, for both this script's dates and the renders taken afterwards:
the CLI reads the same variable (crates/tasqx-cli/src/clock.rs), and it is
passed through to every tasqx call below, so a captured screen is the same bytes
on any calendar day. Without it the wall clock is today, as before.

It writes ONE path, target/demo/tasks.db (gitignored), and replaces it on
every run. It sets TASQX_DB and passes --no-daemon on every call, because a
reachable daemon ignores TASQX_DB and would answer from the real store instead.

The history goes in through `tasqx import`, which keeps backdated `created`,
`completed` and event timestamps, so the charts and the dashboard's burndown
have twelve weeks to draw. The memory docs, the notes and the acceptance checks
on the blocked task ride in the same import for the same reason: a live
`memory add` or `annotate` stamps the pinned instant and mints an id off the
entropy pool, and both of those are printed on a captured screen. Two seeded
RNGs keep it all stable from run to run — one for the shape of the history, one
for the ids — which is what lets `scripts/docs-capture.sh` compare a fresh
capture with the committed fixtures byte for byte.
"""

import datetime as dt
import json
import os
import random
import subprocess
import sys
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "target" / "demo" / "tasks.db"
# A config of its own, so the pictures show tasqx's defaults rather than
# whatever the machine that renders them has configured.
CONFIG = ROOT / "target" / "demo" / "config"
TASQX = os.environ.get("TASQX", "tasqx")


def _now():
    """Today, or the day TASQX_NOW pins — the same variable the CLI reads.

    The store's dates and the render's reference instant have to come from one
    day or the relative spellings ("due tomorrow") describe a different store
    than the one on screen. An unparsable value is fatal here too, for the
    reason clock.rs gives: a silent fallback to the wall clock produces exactly
    the drift the pin was set to prevent.

    What is parsed here is also what the children get. `fromisoformat` is more
    permissive than the RFC 3339 parser in core — it takes an ISO week date
    (`2026-W45-4T09:00:00+00:00`) that jiff refuses — so forwarding the ORIGINAL
    text meant a spelling this script accepted made the first `tasqx` call exit
    2 instead, fifty lines later and with the script's own diagnostic never
    printed. The canonical UTC form is written back into the environment before
    any subprocess exists, so parent and children agree by construction, and
    every child sees the same instant this script built its dates from.
    """
    def refuse(why):
        # Exit 2, not `sys.exit(message)`'s 1: a malformed pin is a bad argument
        # in the environment, and `tasqx` answers that same mistake with 2. One
        # pipeline, two tools, one code — a capture script exiting 1 where the
        # CLI exits 2 makes the caller's check depend on which of them noticed.
        print(f"demo-store: {why}", file=sys.stderr)
        sys.exit(2)

    pin = os.environ.get("TASQX_NOW", "").strip()
    if not pin:
        return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)
    try:
        when = dt.datetime.fromisoformat(pin.replace("Z", "+00:00"))
    except ValueError:
        refuse(f"TASQX_NOW is not an RFC 3339 instant: {pin}")
    if when.tzinfo is None:
        refuse(f"TASQX_NOW needs a zone offset or a trailing Z: {pin}")
    when = when.astimezone(dt.timezone.utc).replace(microsecond=0)
    os.environ["TASQX_NOW"] = when.strftime("%Y-%m-%dT%H:%M:%SZ")
    return when


NOW = _now()
rng = random.Random(7)
id_rng = random.Random(11)


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def day(offset, hour=0, minute=0):
    """Midnight UTC `offset` days from today, plus an optional clock time."""
    d = NOW.replace(hour=0, minute=0, second=0) + dt.timedelta(days=offset)
    return d.replace(hour=hour, minute=minute)


def uid():
    """A v4 id drawn from the SEEDED rng, so two runs mint the same ids.

    `uuid.uuid4()` reads the OS entropy pool, which made every run a different
    store — invisible on a table that prints short ids, and fatal to
    `scripts/docs-capture.sh`, whose fixtures carry ids in full: a `task.get`
    envelope, a `memory list` row, a `memory search` hit. The drift job would
    then have fired on every capture instead of on a real change. Determinism
    here is the promise the rng already makes about the history's shape, applied
    to the one other thing that reaches a captured screen.

    It draws from an rng of its OWN rather than from `rng`: sharing the stream
    would shift every later draw, and the shape of twelve weeks of history —
    which task is on which day, and therefore the short id of every open task —
    would change under a caller who only wanted stable ids.
    """
    return str(uuid.UUID(int=id_rng.getrandbits(128), version=4))


PROJECTS = {
    "website": "Marketing site and docs portal",
    "api": "Public REST API and its SDKs",
    "mobile": "iOS and Android apps",
    "infra": "CI, deploys and on-call",
    "home": None,
}

# The working set: (title, project, priority, due offset in days or None,
# tags, estimate). Spread so every urgency band, every date spelling and the
# rail have something to show.
OPEN = [
    ("Renew the TLS certificate for api.example.dev", "infra", "H", -2, ["ops"], "PT1H"),
    ("Ship the v2 pricing page", "website", "H", 0, ["launch"], "PT6H"),
    ("Fix token refresh race on Android", "mobile", "H", 1, ["bug"], "PT4H"),
    ("Rate-limit the /search endpoint", "api", "H", 3, ["perf"], "PT3H"),
    ("Write the migration guide for SDK 3.0", "api", "M", 5, ["docs"], "PT5H"),
    ("Dark mode for the settings screen", "mobile", "M", 9, ["ui"], "PT8H"),
    ("Move nightly backups to object storage", "infra", "M", None, ["ops"], "PT3H"),
    ("Cut build time under five minutes", "infra", "M", None, ["ci"], "PT6H"),
    ("Add OpenAPI examples for every endpoint", "api", "M", None, ["docs"], "PT4H"),
    ("Accessibility pass on the signup flow", "website", "M", 12, ["a11y"], "PT5H"),
    ("Replace the carousel on the home page", "website", "L", None, ["ui"], "PT2H"),
    ("Book the dentist", "home", "L", 6, [], None),
    ("Archive last year's analytics dashboards", "website", "L", None, [], "PT1H"),
    ("Investigate flaky checkout test", "mobile", None, None, ["bug", "ci"], None),
    ("Sketch ideas for the Q4 offsite", "home", None, None, [], None),
]
# Index into OPEN of the task that is running, and of one that is blocked by
# another (blocked, blocker).
RUNNING = 1
BLOCKED = (4, 3)

DONE_TITLES = [
    "Upgrade Postgres to 16", "Add health check endpoint", "Localise the onboarding",
    "Fix crash on rotate", "Cache the pricing API response", "Retire the v1 webhooks",
    "Set up error budget alerts", "Publish the changelog feed", "Compress hero images",
    "Add SSO for the admin panel", "Paginate the audit log", "Fix typo in the terms page",
    "Rotate the staging secrets", "Add retry to the email worker", "Ship push notifications",
    "Write the incident postmortem", "Tune the autoscaler", "Add sitemap.xml",
    "Fix double charge on retry", "Remove the legacy SDK", "Speed up cold start",
    "Add a status page", "Clean up feature flags", "Document the release process",
]

# Memory docs: (title, project, source, age in days, body). Invented like
# everything else. The age spreads the UPDATED column `memory list` prints, and
# fixes the order that column sorts by; five docs all written "today" ordered by
# their ids, which is an order no reader can predict.
MEMORY = [
    ("release-process", "api", "docs/release.md", 6,
     "---\ndescription: How an SDK release is cut, tagged and announced\n---\n"
     "Cut the release branch on Monday, tag after the canary has run for a day, "
     "and announce in the changelog feed once the packages are live."),
    ("on-call-handover", "infra", None, 13,
     "The handover happens Friday at 16:00. Walk the open incidents, the error "
     "budget and anything paged twice in the week, then rotate the pager."),
    ("pricing-page-decisions", "website", "notes/pricing.md", 2,
     "# Pricing page\nThree tiers, annual billing shown first. The release of v2 "
     "waits for legal to sign off the new terms."),
    ("android-token-refresh", "mobile", None, 20,
     "The refresh race happens when two requests see an expired token at once. "
     "Serialise refresh behind one lock and retry the losing request."),
    ("dentist", None, None, 34,
     "Dr. Visser, Tuesdays and Thursdays, book two weeks ahead."),
]

# Annotations and acceptance checks on the blocked task (`BLOCKED[0]`), so
# `show`, `brief` and the `task.get` envelope have the thing they are for:
# (days ago, body). Everything else in the store carries neither, and a `show`
# of a task with no note under it documents the layout but not the screen.
NOTES = [
    (12, "Scope: the 2.x → 3.0 rename table, the auth change, and a worked "
         "example per SDK. Not the reference — that generates itself."),
    (5, "Ruling: the guide ships WITH the 3.0 release, not after it. A migration "
        "note that lands a week late is a support ticket that already happened."),
    (1, "Blocked until the rate limit lands: the guide has to state the real "
        "ceiling, and quoting a number that then changes is worse than waiting."),
]
CHECKS = [
    ("passed", "Every renamed symbol has a row in the table"),
    ("passed", "One worked example per SDK (js, py, go)"),
    ("open", "The rate-limit ceiling is stated with its real number"),
    ("open", "Reviewed by whoever ships 3.0"),
]


def main():
    projects, tasks, events = [], [], []
    start = day(-90)

    for name, desc in PROJECTS.items():
        pid = uid()
        projects.append({"archived": False, "created": iso(start), "description": desc,
                         "id": pid, "name": name})
        events.append({"actor": "user", "entity": "project", "entity_id": pid, "id": uid(),
                       "op": "create", "ts": iso(start),
                       "payload": {"default": name == "website", "description": desc,
                                   "name": name}})

    def task(sid, title, project, prio, due, tags, est, created, completed=None):
        tid = uid()
        tasks.append({
            "_rev": 1, "annotations": [], "completed": iso(completed) if completed else None,
            "created": iso(created), "depends_on": [],
            "due": iso(due) if due else None, "estimate": est, "id": tid,
            "modified": iso(completed or created), "priority": prio, "project": project,
            "recurrence": None, "remind": None, "scheduled": None, "short_id": sid,
            "status": "done" if completed else "pending", "tags": tags, "title": title,
            "wait": None,
        })
        events.append({"actor": "user", "entity": "task", "entity_id": tid, "id": uid(),
                       "op": "add", "ts": iso(created),
                       "payload": {"priority": prio, "project": project, "recurrence": None,
                                   "status": "pending", "tags": tags, "title": title}})
        if completed:
            events.append({"actor": "user", "entity": "task", "entity_id": tid, "id": uid(),
                           "op": "done", "ts": iso(completed),
                           "payload": {"completed": iso(completed)}})
        return tid

    # Twelve weeks of finished work, weekdays mostly, and a little more of it
    # lately, so the burndown trends down and the heatmap has a rhythm.
    sid = 1
    for n in range(84, 0, -1):
        when = day(-n, 9 + rng.randrange(8), rng.randrange(60))
        weekday = when.weekday() < 5
        chance = (0.55 if weekday else 0.12) + (0.15 if n < 21 else 0)
        for _ in range(1 + (rng.random() < 0.3)):
            if rng.random() < chance:
                title = rng.choice(DONE_TITLES)
                project = rng.choice(["website", "api", "mobile", "infra"])
                # This week's finished work was all captured before it began,
                # so the dashboard's seven-day burndown goes down.
                lead = (10, 30) if n <= 7 else (2, 20)
                created = when - dt.timedelta(days=rng.randrange(*lead))
                task(sid, title, project, rng.choice(["H", "M", "M", "L", None]), None,
                     [], None, created, when)
                sid += 1

    ids = []
    for title, project, prio, due, tags, est in OPEN:
        created = day(-rng.randrange(8, 40), 10)
        due_at = day(due, 17, 0) if due in (0, 1) else (day(due) if due is not None else None)
        ids.append((sid, task(sid, title, project, prio, due_at, tags, est, created)))
        sid += 1

    blocked, blocker = BLOCKED
    subject = tasks[-len(OPEN) + blocked]
    subject["depends_on"] = [ids[blocker][1]]
    subject["annotations"] = [{"id": uid(), "body": body, "created": iso(day(-ago, 11, 20))}
                              for ago, body in NOTES]
    subject["checks"] = [{"id": uid(), "body": body, "state": state, "evidence": None,
                          "position": i, "created": iso(day(-12, 11, 30)),
                          "modified": iso(day(-2, 9, 10))}
                         for i, (state, body) in enumerate(CHECKS)]

    # Memory docs go through `import` rather than `memory add`, so their ids and
    # their dates are this script's to choose: a live add mints a v7 id off the
    # clock and stamps "today", and both reach a captured screen (`memory list`
    # prints the id, `memory search` prints it beside the hit).
    docs = [{"id": uid(), "title": title, "project": project, "source": source,
             "body": body, "created": iso(day(-ago - 5, 9, 15)),
             "modified": iso(day(-ago, 9, 15))}
            for title, project, source, ago, body in MEMORY]

    payload = {"default_project": "website", "docs": docs, "dropped_dependencies": [],
               "events": sorted(events, key=lambda e: e["ts"]), "projects": projects,
               "tasks": tasks}

    OUT.parent.mkdir(parents=True, exist_ok=True)
    for suffix in ("", "-wal", "-shm"):
        Path(f"{OUT}{suffix}").unlink(missing_ok=True)
    src = OUT.parent / "import.json"
    src.write_text(json.dumps(payload))

    CONFIG.mkdir(parents=True, exist_ok=True)
    (CONFIG / "config.toml").unlink(missing_ok=True)
    # os.environ is carried whole, so TASQX_NOW reaches every call below and
    # the live `start` lands on the pinned day rather than the real one.
    env = {**os.environ, "TASQX_DB": str(OUT), "TASQX_CONFIG_DIR": str(CONFIG)}
    run = lambda *args: subprocess.run([TASQX, "--no-daemon", *args], env=env, check=True,
                                       stdout=subprocess.DEVNULL)
    run("import", str(src))
    # The running timer is started live: an active interval is store state an
    # export does not carry, and it should read "running since just now".
    run("start", str(ids[RUNNING][0]))
    # The demo records no AI token spend, and an empty TOKENS panel is space
    # a screenshot has better uses for.
    run("config", "set", "dashboard.panels", "tasks,projects,burndown,pulse,effort")
    print(OUT)


if __name__ == "__main__":
    sys.exit(main())
