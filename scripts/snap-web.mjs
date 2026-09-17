#!/usr/bin/env node
//
// Rasterise the WHOLE generated docs site, page by page, at two widths and
// two themes, so a product-owner pass can judge it as images rather than by
// opening every hash route by hand (docs/maintainers/terminal-style.md §14).
//
//   node scripts/snap-web.mjs <site.html> [out-dir] [anchor,anchor,…]
//
//   node scripts/snap-web.mjs target/site.html target/snaps/site
//   node scripts/snap-web.mjs target/site.html target/snaps/site api-task.done,obj-task-fields
//
// With the third argument, ONLY those in-page ids are shot — the part of a
// long page the 4-viewport cap on full shots never reaches. Each is opened by
// hash (the site reveals its page and scrolls it into view) and clipped from
// the element's top, capped at 3 viewport heights:
//   anchor-<id>-<width>-<theme>.png   (no report.json on an anchor run)
//
// Env:
//   CHROME  headless browser (default: the macOS Google Chrome.app, else
//           google-chrome / chromium on PATH)
//
// <site.html> is `tasqx docs --stdout` (or `--out`) redirected to a file — see
// docs/maintainers/terminal-style.md §14 for the dev-build command line.
//
// Why CDP (Chrome DevTools Protocol) and not `--window-size`: headless Chrome
// on macOS floors `--window-size` at 500px, so asking for a 390px-wide mobile
// screenshot that way silently gives back a 500px layout cropped to 390 — the
// wrong picture with no error. Driving Chrome over CDP's
// Emulation.setDeviceMetricsOverride instead sets the *viewport* Chrome lays
// pixels out against, which has no such floor. This script starts Chrome
// itself with remote debugging on, talks to it with Node's built-in
// WebSocket and fetch (no npm dependencies), and tears it down on exit.
//
// Output (all under `out-dir`, gitignored via target/):
//   <page>-<width>-<theme>.png        first viewport
//   <page>-<width>-<theme>-full.png   full page, capped at 4 viewport heights
//   report.json                       overflow + console diagnostics, once
//                                      per page/width (theme-independent)

import { spawn, execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [, , siteArg, outArg, anchorArg] = process.argv;
if (!siteArg) {
    console.error("usage: node scripts/snap-web.mjs <site.html> [out-dir]");
    process.exit(2);
}
const sitePath = resolve(siteArg);
const outDir = resolve(outArg || "target/snaps/site");
const anchors = anchorArg ? anchorArg.split(",").filter(Boolean) : [];

const WIDTHS = [
    { width: 1400, height: 900 },
    { width: 390, height: 844 },
];
const THEMES = ["light", "dark"];
const MAX_VIEWPORTS = 4;
// The site's page-switch fade is `animation: fade 0.18s ease-out`
// (crates/tasqx-cli/src/docs.rs, `.js .page.active`). Waiting comfortably
// past it avoids a half-transparent shot.
const FADE_MS = 400;

// Waits out the page's own fade-in after a hash change, before touching
// emulation, diagnostics or the screenshot itself.
async function settleFade() {
    await new Promise((r) => setTimeout(r, FADE_MS));
}

function findChrome() {
    if (process.env.CHROME) return process.env.CHROME;
    const candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ];
    for (const c of candidates) {
        if (c.startsWith("/")) {
            if (existsSync(c)) return c;
            continue;
        }
        try {
            execFileSync("command", ["-v", c], { shell: true, stdio: "ignore" });
            return c;
        } catch {
            // try the next candidate
        }
    }
    return null;
}

async function freePort() {
    const net = await import("node:net");
    return new Promise((resolve, reject) => {
        const srv = net.createServer();
        srv.listen(0, "127.0.0.1", () => {
            const { port } = srv.address();
            srv.close(() => resolve(port));
        });
        srv.on("error", reject);
    });
}

async function waitForCdp(port, deadlineMs) {
    const until = Date.now() + deadlineMs;
    for (;;) {
        try {
            const res = await fetch(`http://127.0.0.1:${port}/json/version`);
            if (res.ok) return;
        } catch {
            // Chrome not listening yet.
        }
        if (Date.now() > until) throw new Error("Chrome did not open its CDP port in time");
        await new Promise((r) => setTimeout(r, 100));
    }
}

// A minimal CDP client: one WebSocket, request ids, and an event listener
// list. That is the whole protocol surface this script needs.
class Cdp {
    constructor(ws) {
        this.ws = ws;
        this.nextId = 1;
        this.pending = new Map();
        this.listeners = new Map();
        ws.addEventListener("message", (ev) => {
            const msg = JSON.parse(ev.data);
            if (msg.id && this.pending.has(msg.id)) {
                const { resolve, reject } = this.pending.get(msg.id);
                this.pending.delete(msg.id);
                if (msg.error) reject(new Error(msg.error.message));
                else resolve(msg.result);
            } else if (msg.method) {
                const fns = this.listeners.get(msg.method);
                if (fns) fns.forEach((fn) => fn(msg.params));
            }
        });
        // Chrome dying mid-run (a crash, `--no-sandbox` getting killed) used
        // to leave every in-flight `send()` waiting on a response that would
        // never come — a silent hang instead of a failed run. Closing or
        // erroring the socket now fails every pending request so `main`'s
        // `await` chain unwinds into its `finally` cleanup instead.
        const failPending = (err) => {
            for (const { reject } of this.pending.values()) reject(err);
            this.pending.clear();
        };
        ws.addEventListener("close", () => failPending(new Error("Chrome closed the CDP connection")));
        ws.addEventListener("error", () => failPending(new Error("CDP websocket errored")));
    }

    static async connect(url) {
        const ws = new WebSocket(url);
        await new Promise((resolve, reject) => {
            ws.addEventListener("open", resolve, { once: true });
            ws.addEventListener("error", () => reject(new Error("CDP websocket failed to open")), {
                once: true,
            });
        });
        return new Cdp(ws);
    }

    send(method, params = {}) {
        if (this.ws.readyState !== WebSocket.OPEN) {
            return Promise.reject(new Error(`CDP websocket is not open (state ${this.ws.readyState})`));
        }
        const id = this.nextId++;
        return new Promise((resolve, reject) => {
            this.pending.set(id, { resolve, reject });
            this.ws.send(JSON.stringify({ id, method, params }));
        });
    }

    on(method, fn) {
        if (!this.listeners.has(method)) this.listeners.set(method, []);
        this.listeners.get(method).push(fn);
    }

    close() {
        this.ws.close();
    }
}

// Drops one listener a caller registered with `cdp.on(...)`, once it is done
// with that event — otherwise it keeps firing into a buffer nobody reads,
// and a later listener for the same method fires alongside it.
function removeListener(cdp, method, fn) {
    const fns = cdp.listeners.get(method);
    if (fns) fns.splice(fns.indexOf(fn), 1);
}

async function main() {
    const chrome = findChrome();
    if (!chrome) {
        console.error("snap-web.mjs: no headless Chrome found; set CHROME to one");
        process.exit(1);
    }

    await mkdir(outDir, { recursive: true });
    const profileDir = await mkdtemp(join(tmpdir(), "snap-web-"));
    const port = await freePort();

    const proc = spawn(
        chrome,
        [
            "--headless=new",
            `--remote-debugging-port=${port}`,
            `--user-data-dir=${profileDir}`,
            "--disable-gpu",
            "--hide-scrollbars",
            "--no-sandbox",
        ],
        { stdio: "ignore" },
    );

    let cleanedUp = false;
    const cleanup = async () => {
        if (cleanedUp) return;
        cleanedUp = true;
        // Wait for Chrome to exit before deleting its profile: it is still
        // writing there while it shuts down, and rm fails with ENOTEMPTY.
        const exited = proc.exitCode !== null ? null : new Promise((r) => proc.once("exit", r));
        proc.kill();
        if (exited) await exited;
        await rm(profileDir, { recursive: true, force: true });
    };
    process.on("exit", () => {
        // Best-effort synchronous fallback; the async cleanup above already
        // ran on the success and error paths below.
        proc.kill();
    });

    try {
        await waitForCdp(port, 10_000);
        const listRes = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" });
        const target = await listRes.json();
        const cdp = await Cdp.connect(target.webSocketDebuggerUrl);

        await cdp.send("Page.enable");
        await cdp.send("Runtime.enable");

        // Listening only starts once the per-page loop below does, so an
        // error thrown while the site's own script first runs — before any
        // page is ever hashed to — was silently dropped. Registering here,
        // before `navigate`, catches those; they land in `report.json` as a
        // `phase: "startup"` entry rather than nowhere.
        const startupErrors = [];
        const onStartupConsole = (p) => {
            if (p.type === "error") {
                startupErrors.push(p.args.map((a) => a.value ?? a.description ?? "").join(" "));
            }
        };
        const onStartupException = (p) => {
            startupErrors.push(p.exceptionDetails.text + ": " + (p.exceptionDetails.exception?.description ?? ""));
        };
        cdp.on("Runtime.consoleAPICalled", onStartupConsole);
        cdp.on("Runtime.exceptionThrown", onStartupException);

        // The site is one HTML file: every `.page` section is already in the
        // DOM, and a click on a nav link (or setting `location.hash`) toggles
        // which one is visible (`SCRIPT`'s `go`/`show`, docs.rs). So this loads
        // the file exactly once and then switches pages the same way a reader
        // does — by changing the hash — never re-navigating Chrome, which
        // would not even fire a fresh `Page.loadEventFired` for a same-document
        // hash change.
        const fileUrl = pathToFileURL(sitePath).href;
        await navigate(cdp, fileUrl);

        // Page ids come from the site itself — `<section class="page" id="…">`
        // is what crates/tasqx-cli/src/docs.rs::page_open emits for every page
        // — so a new page in the site is picked up with no change here.
        if (anchors.length) {
            await shootAnchors(cdp);
            cdp.close();
            return;
        }

        const pageIds = await evalJs(
            cdp,
            `Array.prototype.slice.call(document.querySelectorAll('.page')).map(function (p) { return p.id; })`,
        );
        if (pageIds.length === 0) {
            console.error(`snap-web.mjs: no .page sections found in ${sitePath} — is it a generated site?`);
            process.exit(1);
        }

        // Startup listening ends here; each page below collects into its own
        // buffer instead (finding: errors must not bleed across pages).
        removeListener(cdp, "Runtime.consoleAPICalled", onStartupConsole);
        removeListener(cdp, "Runtime.exceptionThrown", onStartupException);

        const report = [
            { page: null, width: null, phase: "startup", consoleErrors: startupErrors },
        ];
        for (const pageId of pageIds) {
            const consoleErrors = [];
            const onConsole = (p) => {
                if (p.type === "error") {
                    consoleErrors.push(p.args.map((a) => a.value ?? a.description ?? "").join(" "));
                }
            };
            const onException = (p) => {
                consoleErrors.push(p.exceptionDetails.text + ": " + (p.exceptionDetails.exception?.description ?? ""));
            };
            cdp.on("Runtime.consoleAPICalled", onConsole);
            cdp.on("Runtime.exceptionThrown", onException);

            await gotoHash(cdp, pageId);
            await settleFade();

            for (const { width, height } of WIDTHS) {
                // Reset per viewport: an error is only ever reported under
                // the width that actually produced it, not copied onto
                // every width after (finding: errors copied across widths).
                consoleErrors.length = 0;
                await cdp.send("Emulation.setDeviceMetricsOverride", {
                    width,
                    height,
                    deviceScaleFactor: 1,
                    mobile: width < 600,
                });
                await evalJs(cdp, `window.scrollTo(0, 0)`);

                const diag = await evalJs(
                    cdp,
                    overflowDiagnosticJs(),
                );

                for (const theme of THEMES) {
                    await setTheme(cdp, theme);
                    const base = `${pageId}-${width}-${theme}`;
                    await screenshot(cdp, join(outDir, `${base}.png`), null);
                    const fullHeight = Math.min(diag.pageScrollHeight, height * MAX_VIEWPORTS);
                    await screenshot(cdp, join(outDir, `${base}-full.png`), {
                        x: 0,
                        y: 0,
                        width,
                        height: fullHeight,
                        scale: 1,
                    });
                }

                report.push({
                    page: pageId,
                    width,
                    pageScrollOverflow: diag.pageScrollOverflow,
                    offenders: diag.offenders,
                    consoleErrors: consoleErrors.slice(),
                });
            }

            removeListener(cdp, "Runtime.consoleAPICalled", onConsole);
            removeListener(cdp, "Runtime.exceptionThrown", onException);
        }

        await writeFile(join(outDir, "report.json"), JSON.stringify(report, null, 2));
        console.log(`wrote ${pageIds.length} pages to ${outDir}`);

        cdp.close();
    } finally {
        await cleanup();
    }
}

async function shootAnchors(cdp) {
    for (const id of anchors) {
        await gotoHash(cdp, id);
        await settleFade();
        for (const { width, height } of WIDTHS) {
            await cdp.send("Emulation.setDeviceMetricsOverride", {
                width,
                height,
                deviceScaleFactor: 1,
                mobile: width < 600,
            });
            const box = await evalJs(
                cdp,
                `(function () {
                   var el = document.getElementById(${JSON.stringify(id)});
                   if (!el) { return null; }
                   el.scrollIntoView();
                   var r = el.getBoundingClientRect();
                   return { y: r.top + window.scrollY, h: r.height };
                 }())`,
            );
            if (!box) throw new Error(`no element with id ${id}`);
            for (const theme of THEMES) {
                await setTheme(cdp, theme);
                await screenshot(cdp, join(outDir, `anchor-${id}-${width}-${theme}.png`), {
                    x: 0,
                    y: box.y,
                    width,
                    height: Math.max(1, Math.min(box.h, height * 3)),
                    scale: 1,
                });
            }
        }
    }
    console.log(`wrote ${anchors.length} anchors to ${outDir}`);
}

function overflowDiagnosticJs() {
    return `(function () {
      function overflows(el) {
        for (var n = el.parentElement; n; n = n.parentElement) {
          var cs = getComputedStyle(n);
          if (cs.overflowX === 'auto' || cs.overflowX === 'scroll') { return null; }
        }
        var r = el.getBoundingClientRect();
        if (r.right > window.innerWidth + 1) { return r; }
        return null;
      }
      var offenders = [];
      Array.prototype.slice.call(document.querySelectorAll('*')).forEach(function (el) {
        var r = overflows(el);
        if (r) {
          offenders.push({
            tag: el.tagName.toLowerCase(),
            class: el.className && el.className.toString ? el.className.toString() : '',
            id: el.id || '',
            right: Math.round(r.right),
          });
        }
      });
      return {
        pageScrollOverflow: document.documentElement.scrollWidth > window.innerWidth,
        pageScrollHeight: document.documentElement.scrollHeight,
        offenders: offenders,
      };
    }())`;
}

async function navigate(cdp, url) {
    const loaded = new Promise((resolve) => {
        const fn = () => {
            const fns = cdp.listeners.get("Page.loadEventFired");
            fns.splice(fns.indexOf(fn), 1);
            resolve();
        };
        cdp.on("Page.loadEventFired", fn);
    });
    await cdp.send("Page.navigate", { url });
    await loaded;
}

async function evalJs(cdp, expression, { awaitPromise = false } = {}) {
    const { result, exceptionDetails } = await cdp.send("Runtime.evaluate", {
        expression,
        returnByValue: true,
        awaitPromise,
    });
    if (exceptionDetails) {
        throw new Error(`page script failed: ${exceptionDetails.text}`);
    }
    return result.value;
}

// Switches the visible page the same way a reader clicking a nav link does:
// set `location.hash`, which the site's own `hashchange` listener (SCRIPT's
// `go`/`show`) reacts to by toggling `.page.active`. No `Page.navigate` here —
// a same-document hash change does not fire `Page.loadEventFired` again, so
// waiting on that event would hang.
async function gotoHash(cdp, id) {
    await evalJs(
        cdp,
        `new Promise(function (resolve) {
           if (location.hash.slice(1) === ${JSON.stringify(id)}) { resolve(); return; }
           window.addEventListener('hashchange', function once() {
             window.removeEventListener('hashchange', once);
             resolve();
           });
           location.hash = ${JSON.stringify(id)};
           setTimeout(resolve, 300);
         })`,
        { awaitPromise: true },
    );
}

// Forces the theme the same way the site's own switch does: click the header
// button (crates/tasqx-cli/src/docs.rs::header, `.themebtn[data-theme-set]`)
// so the page runs its own click handler — the same `applyTheme` +
// `localStorage` write a reader triggers — rather than poking the
// `data-theme` attribute from outside.
async function setTheme(cdp, mode) {
    await evalJs(
        cdp,
        `document.querySelector('.themebtn[data-theme-set="${mode}"]').click()`,
    );
}

async function screenshot(cdp, path, clip) {
    const params = { format: "png" };
    if (clip) {
        params.clip = clip;
        params.captureBeyondViewport = true;
    }
    const { data } = await cdp.send("Page.captureScreenshot", params);
    await writeFile(path, Buffer.from(data, "base64"));
}

main().catch((err) => {
    console.error(`snap-web.mjs: ${err.message}`);
    process.exit(1);
});
