//! The one page `tasqx board` serves (#637): inline CSS, one inline script, a
//! system font stack, nothing fetched from anywhere but its own origin. The same
//! idiom as `report --html` ([`crate::html`]) and `tasqx docs`, and the same
//! palette (`html::palette`, light and dark, AA-contrast checked against the
//! active theme).
//!
//! The page owns no state of its own: it asks `/api` (`task.list`, `task.get`),
//! and repaints when `/events` says something changed. Phase 1 writes nothing.
//! Cards are built with `textContent`, never `innerHTML`, so a task title is
//! only ever text.

use crate::theme::Theme;

/// The script's ceiling in bytes, so growth is a red test rather than drift.
#[cfg(test)]
const SCRIPT_BUDGET: usize = 12 * 1024;

/// Where the per-run nonce goes; the listener's CSP allows only that script.
pub(crate) const NONCE_SLOT: &str = "__NONCE__";

/// The columns, as `(id, title, filter)`: the states the core already derives.
/// The page carries the same table in its script; a test keeps them equal.
#[cfg(test)]
const COLUMNS: [(&str, &str, &str); 5] = [
    ("backlog", "Backlog", "status:backlog"),
    ("blocked", "Blocked", "@blocked"),
    ("ready", "Ready", "@working"),
    ("active", "Active", "status:active"),
    ("done", "Done", "completed.after:-7d"),
];

/// The page, with [`NONCE_SLOT`] still in it.
pub(crate) fn page(theme: &Theme) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>tasqx board</title><style>{}{}</style></head><body>{}\
         <script nonce=\"{NONCE_SLOT}\">{}</script></body></html>\n",
        crate::html::palette(theme),
        CSS,
        BODY,
        SCRIPT
    )
}

const BODY: &str = r##"<a class="skip" href="#cols">Skip to the board</a>
<header class="bar">
<h1>tasqx <span>board</span></h1>
<label class="vh" for="q">Search</label>
<input id="q" type="search" placeholder="Search title, tag, id   ( / )" autocomplete="off">
<label class="sel">Lanes <select id="lanes"><option value="none">none</option><option value="project">by project</option><option value="priority">by priority</option></select></label>
<span id="conn" class="conn" role="status" aria-live="polite">daemon · connecting</span>
</header>
<p id="err" class="err" role="alert" hidden></p>
<main id="cols" class="cols" aria-label="Task board"></main>
<aside id="panel" class="panel" role="dialog" aria-labelledby="ptitle" hidden>
<button id="pclose" class="x" aria-label="Close details (Esc)">&times;</button>
<h2 id="ptitle"></h2><div id="pbody"></div>
</aside>
<p class="keys">j/k move &middot; h/l column &middot; Enter open &middot; / search &middot; Esc close</p>"##;

const CSS: &str = r###"*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;-webkit-text-size-adjust:100%}
.vh{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);white-space:nowrap}
.skip{position:absolute;left:-999px;top:0;background:var(--accent);color:var(--on-accent);padding:8px 12px;z-index:9}
.skip:focus{left:8px;top:8px}
:focus-visible{outline:3px solid var(--accent);outline-offset:2px;border-radius:3px}
[hidden]{display:none!important}
button,input,select{font:inherit;color:inherit}
.bar{display:flex;flex-wrap:wrap;gap:10px 14px;align-items:center;padding:12px clamp(12px,2vw,24px);border-bottom:1px solid var(--line);background:var(--surface)}
h1{font-size:18px;margin:0;font-weight:700}h1 span{font-weight:400;color:var(--muted)}
#q{flex:1 1 220px;min-width:0;padding:7px 10px;background:var(--sunken);border:1px solid var(--line-strong);border-radius:6px}
.sel{color:var(--muted);font-size:13px}
select{background:var(--sunken);border:1px solid var(--line-strong);border-radius:6px;padding:6px 8px}
.conn{font-size:13px;color:var(--muted)}.conn.on{color:var(--good)}.conn.off{color:var(--danger);font-weight:600}
.err{margin:0;padding:8px clamp(12px,2vw,24px);background:var(--danger);color:var(--on-accent)}
.cols{display:grid;grid-template-columns:repeat(5,minmax(210px,1fr));gap:12px;padding:12px clamp(12px,2vw,24px);overflow-x:auto;align-items:start}
.col{background:var(--sunken);border:1px solid var(--line);border-radius:8px;min-width:0}
.col>header{padding:10px 12px 6px;border-bottom:1px solid var(--line)}
.col h2{margin:0;font-size:14px;display:flex;justify-content:space-between;gap:8px}
.col .n{color:var(--muted);font-variant-numeric:tabular-nums}
.col code{font:12px ui-monospace,"SF Mono",Consolas,monospace;color:var(--muted)}
.lane{margin:0;padding:6px 12px 0;font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}
.list{list-style:none;margin:0;padding:8px;display:grid;gap:8px}
.empty{color:var(--muted);font-size:13px;padding:4px 4px 8px}
.card{background:var(--surface);border:1px solid var(--line-strong);border-radius:6px;padding:8px 10px;cursor:pointer}
.card:hover{border-color:var(--accent)}
.card[aria-current=true]{border-color:var(--accent);box-shadow:0 0 0 2px var(--accent)}
.top{display:flex;gap:8px;align-items:baseline;font-size:12px;color:var(--muted)}
.rail{width:1.1em;text-align:center;color:var(--good)}.rail.blk{color:var(--danger)}
.tid,.meta{font-variant-numeric:tabular-nums}
.pri{margin-left:auto;font-weight:700}
.ttl{margin:2px 0 4px;overflow-wrap:anywhere}
.meta{font-size:12px;color:var(--muted);display:flex;flex-wrap:wrap;gap:2px 10px}
.meta .late{color:var(--danger);font-weight:600}
.gauge{height:4px;border-radius:2px;background:var(--line);margin-top:6px;overflow:hidden}
.gauge i{display:block;height:100%;background:var(--muted)}
.gauge.warn i{background:var(--warn)}.gauge.danger i{background:var(--danger)}
.panel{position:fixed;top:0;right:0;bottom:0;width:min(420px,100%);overflow:auto;background:var(--surface);border-left:1px solid var(--line-strong);box-shadow:-8px 0 24px var(--shadow);padding:16px;z-index:5}
.panel h2{margin:0 36px 8px 0;font-size:17px;overflow-wrap:anywhere}
.x{position:absolute;top:8px;right:8px;width:36px;height:36px;background:none;border:1px solid var(--line-strong);border-radius:6px;font-size:22px;cursor:pointer}
.panel dl{display:grid;grid-template-columns:auto 1fr;gap:4px 12px;margin:0 0 12px}
.panel dt{color:var(--muted)}.panel dd{margin:0;overflow-wrap:anywhere}
.panel ul{margin:0 0 12px;padding-left:20px}
.panel h3{font-size:12px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted);margin:12px 0 4px}
.keys{margin:0;padding:8px clamp(12px,2vw,24px) 16px;color:var(--muted);font-size:12px}
@media (max-width:760px){.cols{grid-template-columns:1fr;overflow-x:visible}.keys{display:none}}
@media (prefers-reduced-motion:no-preference){.card{transition:border-color .15s}}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important;scroll-behavior:auto!important}}
"###;

const SCRIPT: &str = r###"(()=>{
"use strict";
const COLS=[["backlog","Backlog","status:backlog"],["blocked","Blocked","@blocked"],["ready","Ready","@working"],["active","Active","status:active"],["done","Done","completed.after:-7d"]];
const $=id=>document.getElementById(id);
const el=(t,c,x)=>{const e=document.createElement(t);if(c)e.className=c;if(x!=null)e.textContent=x;return e};
const state={data:{},q:"",lanes:"none",sel:null,live:false};
let timer=0,seq=0;
async function api(method,params){
 const r=await fetch("/api",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({tasqx:"1",id:"1",method,params}),credentials:"same-origin"});
 let env;try{env=await r.json()}catch(e){throw new Error("HTTP "+r.status)}
 if(!env.ok)throw new Error(env.error&&env.error.message||("HTTP "+r.status));
 return env.result;
}
function showErr(m){const e=$("err");e.hidden=!m;e.textContent=m||""}
async function load(){
 const mine=++seq;
 try{
  const rs=await Promise.all(COLS.map(c=>api("task.list",{filter:c[2],sort:["-urgency"],limit:500})));
  if(mine!==seq)return;
  COLS.forEach((c,i)=>{state.data[c[0]]=rs[i].tasks});
  showErr("");paint();
 }catch(e){showErr("Could not read the store: "+e.message)}
}
function later(){clearTimeout(timer);timer=setTimeout(load,150)}
function day(s){return new Date(s).toLocaleDateString(undefined,{weekday:"short",day:"numeric",month:"short"})}
function band(u){return u>=12?"danger":u>=6?"warn":""}
function hit(t){
 const q=state.q;if(!q)return true;
 return (t.title+" "+(t.tags||[]).join(" ")+" #"+t.short_id+" "+t.project+" "+(t.priority||"")).toLowerCase().includes(q);
}
function lane(t){return state.lanes==="project"?t.project||"(none)":state.lanes==="priority"?({H:"High",M:"Medium",L:"Low"}[t.priority]||"No priority"):""}
function card(t,col){
 const li=el("li","card");li.tabIndex=0;li.dataset.id=t.short_id;
 const top=el("div","top");
 const blk=t.blocked&&t.status!=="done";
 const rail=el("span","rail"+(blk?" blk":""),t.status==="active"?"▶":blk?"⊘":"");
 rail.setAttribute("aria-label",t.status==="active"?"running":blk?"blocked":"");
 top.append(rail,el("span","tid","#"+t.short_id),el("span","pri",t.priority||""));
 li.append(top,el("div","ttl",t.title));
 const m=el("div","meta");
 if(t.project)m.append(el("span","",t.project));
 if(col==="done"&&t.completed)m.append(el("span","","done "+day(t.completed)));
 else if(t.due){const late=new Date(t.due)<new Date();m.append(el("span",late?"late":"",(late?"overdue ":"due ")+day(t.due)))}
 (t.tags||[]).forEach(g=>m.append(el("span","","+"+g)));
 li.append(m);
 if(col!=="done"){
  const u=t.urgency||0,g=el("div","gauge "+band(u));g.setAttribute("role","img");
  g.setAttribute("aria-label","urgency "+u);
  const i=el("i");i.style.width=Math.min(100,Math.max(0,u/12*100))+"%";g.append(i);li.append(g);
 }
 li.setAttribute("aria-label",t.title+", #"+t.short_id);
 li.addEventListener("click",()=>open(t.short_id));
 return li;
}
function paint(){
 const keep=state.sel,main=$("cols"),had=document.activeElement&&document.activeElement.dataset&&document.activeElement.dataset.id;
 main.replaceChildren();
 COLS.forEach(c=>{
  const rows=(state.data[c[0]]||[]).filter(t=>(c[0]!=="ready"||t.status!=="active")&&hit(t));
  const s=el("section","col");s.setAttribute("aria-labelledby","h-"+c[0]);
  const h=el("header"),h2=el("h2");const nm=el("span","",c[1]);nm.id="h-"+c[0];
  h2.append(nm,el("span","n",String(rows.length)));h.append(h2,el("code","",c[2]));s.append(h);
  if(!rows.length)s.append(el("p","empty","Nothing here."));
  let cur=null,ul=null;
  rows.forEach(t=>{
   const l=lane(t);
   if(!ul||l!==cur){cur=l;if(l)s.append(el("p","lane",l));ul=el("ul","list");s.append(ul)}
   const k=card(t,c[0]);if(keep===t.short_id)k.setAttribute("aria-current","true");ul.append(k);
  });
  main.append(s);
 });
 if(had){const k=main.querySelector('.card[data-id="'+had+'"]');if(k)k.focus()}
}
const cards=()=>Array.from(document.querySelectorAll(".card"));
function move(dx,dy){
 const all=cards(),i=all.indexOf(document.activeElement);
 if(i<0){if(all[0])all[0].focus();return}
 if(dy){const n=all[i+dy];if(n)n.focus();return}
 const col=document.activeElement.closest(".col"),cs=Array.from(document.querySelectorAll(".col"));
 for(let j=cs.indexOf(col)+dx;j>=0&&j<cs.length;j+=dx){const f=cs[j].querySelector(".card");if(f){f.focus();return}}
}
async function open(id){
 state.sel=id;const p=$("panel");p.hidden=false;$("ptitle").textContent="#"+id;$("pbody").replaceChildren(el("p","","Loading…"));
 $("pclose").focus();
 try{
  const t=await api("task.get",{ref:id,annotations_limit:1,max_body_bytes:600});
  if(state.sel!==id)return;
  $("ptitle").textContent="#"+t.short_id+" "+t.title;
  const dl=el("dl");
  const row=(k,v)=>{if(v==null||v==="")return;dl.append(el("dt","",k),el("dd","",v))};
  row("Status",t.status+(t.blocked?" (blocked)":""));row("Project",t.project);row("Priority",t.priority);
  row("Urgency",String(t.urgency));row("Due",t.due&&day(t.due));row("Estimate",t.estimate);
  row("Tags",(t.tags||[]).map(g=>"+"+g).join(" "));
  row("Blocked by",(t.depends_on||[]).map(d=>"#"+d).join(" "));
  row("Notes",String(t.annotations_total||0));row("Revision",String(t._rev));
  const b=$("pbody");b.replaceChildren(dl);
  if((t.checks||[]).length){
   const done=t.checks.filter(c=>c.state==="passed").length;
   b.append(el("h3","","Checks "+done+"/"+t.checks.length));
   const ul=el("ul");t.checks.forEach(c=>ul.append(el("li","",(c.state==="passed"?"[x] ":"[ ] ")+c.body)));b.append(ul);
  }
  if(t.first_annotation){b.append(el("h3","","Opening note"),el("p","",t.first_annotation.body))}
 }catch(e){$("pbody").replaceChildren(el("p","err",e.message))}
}
function close(){
 const id=state.sel;state.sel=null;$("panel").hidden=true;paint();
 const k=id&&document.querySelector('.card[data-id="'+id+'"]');if(k)k.focus();
}
function conn(on){
 state.live=on;const c=$("conn");c.textContent=on?"daemon · live":"daemon · offline";c.className="conn "+(on?"on":"off");
}
function stream(){
 const es=new EventSource("/events");
 es.addEventListener("daemon",e=>{const on=JSON.parse(e.data).online;conn(on);if(on)later()});
 es.addEventListener("task.changed",()=>{later();if(state.sel)open(state.sel)});
 es.onerror=()=>conn(false);
}
$("q").addEventListener("input",e=>{state.q=e.target.value.trim().toLowerCase();paint()});
$("lanes").addEventListener("change",e=>{state.lanes=e.target.value;paint()});
$("pclose").addEventListener("click",close);
document.addEventListener("keydown",e=>{
 if(e.ctrlKey||e.metaKey||e.altKey)return;
 const typing=/^(INPUT|SELECT|TEXTAREA)$/.test(document.activeElement.tagName);
 if(e.key==="Escape"){if(!$("panel").hidden)close();else if(typing)document.activeElement.blur();return}
 if(typing)return;
 if(e.key==="/"){e.preventDefault();$("q").focus()}
 else if(e.key==="j")move(0,1);else if(e.key==="k")move(0,-1);
 else if(e.key==="l")move(1,0);else if(e.key==="h")move(-1,0);
 else if(e.key==="Enter"){const k=document.activeElement;if(k&&k.classList.contains("card"))open(k.dataset.id)}
});
load();stream();
})();
"###;

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> String {
        page(&crate::theme::builtin("nord").expect("nord ships"))
    }

    #[test]
    fn the_script_stays_inside_its_budget() {
        assert!(SCRIPT.len() <= SCRIPT_BUDGET, "{} bytes", SCRIPT.len());
    }

    #[test]
    fn the_page_script_and_the_rust_table_name_the_same_columns() {
        for (id, title, filter) in COLUMNS {
            let want = format!("[\"{id}\",\"{title}\",\"{filter}\"]");
            assert!(SCRIPT.contains(&want), "script lacks column {want}");
        }
    }

    /// The page may ask its own origin for two things and nothing else: no
    /// remote URL, no `src`/`href` to anywhere, one script, never `innerHTML`.
    #[test]
    fn the_page_reaches_nothing_but_its_own_origin() {
        let doc = built();
        for banned in [
            "http://",
            "https://",
            "//cdn",
            "@import",
            "url(",
            "<link",
            "<iframe",
            "<img",
            " src=",
            "innerHTML",
            "eval(",
            "new Function",
            "WebSocket",
            "XMLHttpRequest",
            "sendBeacon",
            "importScripts",
        ] {
            assert!(!doc.contains(banned), "the page contains `{banned}`");
        }
        assert_eq!(doc.matches("<script").count(), 1, "one inline script");
        assert!(doc.contains(&format!("<script nonce=\"{NONCE_SLOT}\">")));
        assert_eq!(doc.matches("fetch(\"/api\"").count(), 1);
        assert_eq!(doc.matches("new EventSource(\"/events\")").count(), 1);
        // Every anchor is in-page.
        for part in doc.split("href=\"").skip(1) {
            assert!(
                part.starts_with('#'),
                "href leaves the page: {}",
                &part[..20.min(part.len())]
            );
        }
    }

    /// Phase 1 writes nothing: the script asks for reads and no other method.
    #[test]
    fn the_script_asks_only_read_methods() {
        for m in SCRIPT.split("api(\"").skip(1) {
            let name = m.split('"').next().unwrap();
            assert!(
                tasqx_core::board::READ_METHODS.contains(&name),
                "the page calls `{name}`, which the board does not serve"
            );
        }
    }

    #[test]
    fn it_has_light_and_dark_tokens_and_honours_reduced_motion() {
        let doc = built();
        assert!(doc.contains("color-scheme:light") && doc.contains("color-scheme:dark"));
        assert!(doc.contains("prefers-reduced-motion:reduce"));
        assert!(doc.contains(":focus-visible"));
    }
}
