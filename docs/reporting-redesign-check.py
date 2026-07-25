#!/usr/bin/env python3
"""STRUCTURAL checker for the redesign prototype's output.

Every assertion below is made over PARSED structure — attribute values, the
text inside <style>, the text inside <script> — never over a substring scan of
the whole document. That is not fussiness: annotation bodies in this store
legitimately quote "https://", "@import", "url(" and "pushState", so a raw
`'pushState' in doc` check reports a violation that is actually prose inside a
<div class="body">.

Usage: python3 check.py FILE [FILE ...]
"""

from __future__ import annotations

import re
import sys
from html.parser import HTMLParser

# Attributes that make the browser fetch something. `src` is banned outright;
# `href` is allowed only as an in-page fragment.
FETCHING_ATTRS = {"src", "srcset", "data", "poster", "action", "formaction",
                  "codebase", "background", "ping", "manifest"}

# CSS constructs that would reach outside the document.
RE_CSS_IMPORT = re.compile(r"@import\b", re.I)
RE_CSS_URL = re.compile(r"\burl\(\s*([^)]*)\)", re.I)
RE_SCHEME = re.compile(r"https?://", re.I)

# JS the page must never contain. These are inert STRING LITERALS searched for
# in the page's <script> text — this checker never evaluates anything.
BANNED_JS = [
    "pushState", "replaceState", "fetch(", "XMLHttpRequest", "WebSocket",
    "EventSource", "importScripts", "eval(", "new Function", "Function(",
    "localStorage", "sessionStorage", "document.write",
]

# The palette and the stack order are FIXED.
EXPECTED_BUCKETS = [
    ("tokens_cache_read", "#00688f", "#2f9fc6"),
    ("tokens_cache_creation", "#a35d0a", "#c07a1e"),
    ("tokens_in", "#8e4b9c", "#a962c0"),
    ("tokens_out", "#41762b", "#5fa036"),
]


class Doc(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.tags = []                 # (tag, {attrs})
        self.styles = []               # text of each <style>
        self.scripts = []              # text of each <script>
        self.links = []                # href values
        self.ids = []                  # id values
        self.classes = []              # (tag, class-string, {attrs})
        self._stack = []
        self.text_by_class = []        # (classlist, text) for spot checks

    def handle_starttag(self, tag, attrs):
        a = {k: (v if v is not None else "") for k, v in attrs}
        self.tags.append((tag, a))
        if "id" in a:
            self.ids.append(a["id"])
        if "href" in a:
            self.links.append(a["href"])
        if "class" in a:
            self.classes.append((tag, a["class"], a))
        self._stack.append(tag)

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if self._stack:
            self._stack.pop()

    def handle_endtag(self, tag):
        while self._stack:
            if self._stack.pop() == tag:
                break

    def handle_data(self, data):
        if self._stack and self._stack[-1] == "style":
            self.styles.append(data)
        elif self._stack and self._stack[-1] == "script":
            self.scripts.append(data)


def check(path: str) -> list[str]:
    raw = open(path, encoding="utf-8").read()
    d = Doc()
    d.feed(raw)
    fail: list[str] = []

    def bad(msg):
        fail.append(msg)

    # --- 1. exactly one inline <script>, and it has no src -------------------
    scripts = [(t, a) for t, a in d.tags if t == "script"]
    if len(scripts) != 1:
        bad(f"expected exactly 1 <script>, found {len(scripts)}")
    for _t, a in scripts:
        if "src" in a:
            bad("<script> carries a src attribute")

    # --- 2. exactly one <style>, no <link>, no fetching attributes anywhere --
    styles = [(t, a) for t, a in d.tags if t == "style"]
    if len(styles) != 1:
        bad(f"expected exactly 1 <style>, found {len(styles)}")
    for t, a in d.tags:
        if t == "link":
            bad(f"<link> element present (rel={a.get('rel')!r})")
        for k in a:
            if k in FETCHING_ATTRS:
                bad(f"<{t}> carries fetching attribute {k}={a[k]!r}")

    # --- 3. every href is an in-page fragment -------------------------------
    for h in d.links:
        if not h.startswith("#"):
            bad(f"non-anchor href: {h!r}")

    # --- 4. no absolute URL in any ATTRIBUTE VALUE (not in text) ------------
    for t, a in d.tags:
        for k, v in a.items():
            if RE_SCHEME.search(v):
                bad(f"<{t} {k}=...> attribute value contains an http(s) URL")

    # --- 5. <style> content: no @import, and url() only as url(#...) --------
    css = "".join(d.styles)
    if RE_CSS_IMPORT.search(css):
        bad("<style> contains @import")
    for m in RE_CSS_URL.finditer(css):
        arg = m.group(1).strip().strip("'\"")
        if not arg.startswith("#"):
            bad(f"<style> contains url({arg!r}) — only in-document url(#...) allowed")
    if RE_SCHEME.search(css):
        bad("<style> contains an http(s) URL")

    # --- 6. inline style="" attributes: same rule ---------------------------
    for t, a in d.tags:
        s = a.get("style", "")
        if s:
            if RE_CSS_IMPORT.search(s):
                bad(f"<{t} style> contains @import")
            for m in RE_CSS_URL.finditer(s):
                arg = m.group(1).strip().strip("'\"")
                if not arg.startswith("#"):
                    bad(f"<{t} style> contains url({arg!r})")

    # --- 7. SVG fill=url(#...) must reference an id that exists -------------
    idset = set(d.ids)
    for t, a in d.tags:
        for k in ("fill", "stroke", "mask", "filter", "clip-path"):
            v = a.get(k, "")
            m = RE_CSS_URL.search(v)
            if m:
                arg = m.group(1).strip().strip("'\"")
                if not arg.startswith("#"):
                    bad(f"<{t} {k}> references {arg!r}, not an in-document id")
                elif arg[1:] not in idset:
                    bad(f"<{t} {k}=url({arg})> references a missing id")

    # --- 8. <script> content: no network, no History API, no eval ----------
    js = "".join(d.scripts)
    for tok in BANNED_JS:
        if tok in js:
            bad(f"<script> contains banned construct {tok!r}")
    if RE_SCHEME.search(js):
        bad("<script> contains an http(s) URL")

    # --- 9. drill-down integrity, both directions --------------------------
    href_tasks = {h[len("#task-"):] for h in d.links if h.startswith("#task-")}
    panel_tasks = {i[len("task-"):] for i in d.ids
                   if i.startswith("task-") and not i.endswith("-t")}
    if href_tasks - panel_tasks:
        bad(f"dangling #task- hrefs with no panel: {sorted(href_tasks - panel_tasks)}")
    if panel_tasks - href_tasks:
        bad(f"panels nothing links to: {sorted(panel_tasks - href_tasks)}")

    # --- 10. every id is unique --------------------------------------------
    dupes = {i for i in d.ids if d.ids.count(i) > 1}
    if dupes:
        bad(f"duplicate ids: {sorted(dupes)}")

    # --- 11. palette + stack order are FIXED -------------------------------
    order = []
    for k, lightv, darkv in EXPECTED_BUCKETS:
        if f"--c-{k}: {lightv};" not in css:
            bad(f"light palette value for {k} is not {lightv}")
        if f"--c-{k}: {darkv};" not in css:
            bad(f"dark palette value for {k} is not {darkv}")
        m = re.search(rf"\.sw-{k}, \.seg-{k} \{{", css)
        order.append(m.start() if m else -1)
    if -1 in order:
        bad("a bucket swatch rule is missing from <style>")
    elif order != sorted(order):
        bad("bucket stack order in <style> is not cyan, orange, purple, green")

    # --- 12. four buckets are never blended into one headline --------------
    # The four tiles must be four distinct elements carrying four distinct
    # bucket labels, and no .tile may carry more than one bucket.
    tiles = [a for t, cl, a in d.classes
             if t == "div" and "tile" in cl.split() and "data-bucket" in a]
    if tiles:
        got = [a["data-bucket"] for a in tiles]
        want = [k for k, _l, _d in EXPECTED_BUCKETS]
        if got != want:
            bad(f"token tiles are {got}, expected {want} in that order")
    else:
        # zero-state: exactly one em-dash tile, never four zeros
        alltiles = [a for t, cl, a in d.classes
                    if t == "div" and "tile" in cl.split()]
        if len(alltiles) != 1:
            bad(f"no per-bucket tiles and {len(alltiles)} tiles present "
                "(zero-state must be exactly one)")

    # --- 13. the sticky header carries no token tile -----------------------
    # Structural: find the header's own extent by re-parsing that slice.
    hm = re.search(r"<header class=\"summary\">.*?</header>", raw, re.S)
    if not hm:
        bad("no <header class=\"summary\"> found")
    else:
        hd = Doc()
        hd.feed(hm.group(0))
        for t, cl, a in hd.classes:
            if "tile" in cl.split() or "tokrow" in cl.split():
                bad("a token tile is inside the sticky header")
        if any("rangeband" in cl.split() for _t, cl, _a in hd.classes):
            bad("the range band is inside the sticky header (breaks --hh)")
        stats = [1 for _t, cl, _a in hd.classes if "stat" in cl.split()]
        if len(stats) != 4:
            bad(f"header carries {len(stats)} stat tiles, expected 4")

    # --- 14. --hh is used, not hardcoded, for every scroll clearance -------
    for m in re.finditer(r"scroll-margin-top:\s*([^;}]+)", css):
        if m.group(1).strip() != "var(--hh)":
            bad(f"scroll-margin-top: {m.group(1).strip()} — must be var(--hh)")
    if not re.search(r"--hh:\s*[\d.]+rem", css):
        bad("--hh is not defined in <style>")

    # --- 15. the live region exists and is polite --------------------------
    live = [a for t, a in d.tags if a.get("id") == "live"]
    if len(live) != 1:
        bad(f"expected exactly 1 #live region, found {len(live)}")
    elif live[0].get("aria-live") != "polite" or live[0].get("role") != "status":
        bad("#live is not role=status aria-live=polite")

    # --- 16. every detail panel is focusable and named ---------------------
    for t, a in d.tags:
        if t == "article" and "detail" in a.get("class", "").split():
            if a.get("tabindex") != "-1":
                bad(f"panel {a.get('id')} has no tabindex=-1")
            if a.get("role") != "region":
                bad(f"panel {a.get('id')} has no role=region")
            lb = a.get("aria-labelledby")
            if not lb or lb not in idset:
                bad(f"panel {a.get('id')} aria-labelledby points nowhere")

    return fail


def main() -> int:
    rc = 0
    for path in sys.argv[1:]:
        fails = check(path)
        if fails:
            rc = 1
            print(f"FAIL {path}")
            for f in fails:
                print(f"  - {f}")
        else:
            print(f"PASS {path}")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
