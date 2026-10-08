//! The one page `tasqx board` serves (#637): inline CSS, one inline script, a
//! system font stack, nothing fetched from anywhere but its own origin. The same
//! idiom as `report --html` ([`crate::html`]) and `tasqx docs`, and the same
//! palette (`html::palette`, light and dark, AA-contrast checked against the
//! active theme).
//!
//! The page owns no state of its own: it asks `/api` (`task.list`, `task.get`),
//! and repaints when `/events` says something changed. A drag (#638) sends one
//! of the listener's [`tasqx_core::board::WRITE_METHODS`] with the card's
//! `_rev` as `expected_rev`; the detail panel's buttons and the `s`/`d` keys
//! send the same calls, so every drag has a keyboard and touch equivalent.
//! Under `--scope read` the page draws no drag, button or key that writes (and
//! the listener refuses them anyway). Cards are built with `textContent`, never
//! `innerHTML`, so a task title is only ever text.

use crate::theme::Theme;

/// The script's ceiling in bytes, so growth is a red test rather than drift.
#[cfg(test)]
const SCRIPT_BUDGET: usize = 16 * 1024;

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

/// The page, with [`NONCE_SLOT`] still in it. `writes` is `--scope write`.
pub(crate) fn page(theme: &Theme, writes: bool) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>tasqx board</title><style>{}{}</style></head><body data-writes=\"{}\">{}\
         <script nonce=\"{NONCE_SLOT}\">{}</script></body></html>\n",
        crate::html::palette(theme),
        CSS,
        u8::from(writes),
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
<div id="tray" class="tray" hidden aria-hidden="true"><b data-pri="H">H</b><b data-pri="M">M</b><b data-pri="L">L</b><b data-pri="">clear</b><b data-cancel="1" class="cx">cancel</b></div>
<div id="toast" class="toast" role="status" aria-live="polite" hidden><span id="tmsg"></span><button id="tundo" type="button" hidden>Undo</button></div>
<p class="keys">j/k move &middot; h/l column &middot; Enter open &middot; / search &middot; Esc close<span class="w"> &middot; s start &middot; d done &middot; drag a card to a column, or onto H/M/L to set priority</span></p>"##;

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
.col{background:var(--bg);border:1px solid var(--line);border-radius:8px;min-width:0}
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
body[data-writes="1"] .card{cursor:grab;-webkit-user-select:none;user-select:none}
body.dnd{-webkit-user-select:none;user-select:none;cursor:grabbing}
body[data-writes="0"] .w{display:none}
.col.over{outline:2px dashed var(--accent);outline-offset:-2px}.col.no{outline-color:var(--danger)}
.ghost{position:fixed;pointer-events:none;z-index:8;opacity:.92;box-shadow:0 8px 24px var(--shadow);margin:0}
.dragging{opacity:.4}
.tray{position:fixed;z-index:9;display:flex;gap:6px;padding:6px;background:var(--surface);border:1px solid var(--line-strong);border-radius:8px;box-shadow:0 4px 16px var(--shadow)}
.tray b{min-width:44px;min-height:44px;display:grid;place-items:center;padding:0 8px;border:1px solid var(--line-strong);border-radius:6px}
.tray b.over{background:var(--accent);color:var(--on-accent)}.tray .cx{color:var(--danger)}.tray .cx.over{background:var(--danger);color:var(--on-accent)}
.toast{position:fixed;left:50%;bottom:16px;transform:translateX(-50%);z-index:10;display:flex;gap:12px;align-items:center;max-width:calc(100% - 24px);padding:10px 14px;background:var(--fg);color:var(--bg);border-radius:8px;box-shadow:0 4px 16px var(--shadow)}
.toast button{background:none;border:1px solid var(--bg);border-radius:6px;padding:4px 10px;color:inherit;cursor:pointer}
.acts{display:flex;flex-wrap:wrap;gap:6px;margin:0 0 12px}
.acts button{min-height:36px;padding:4px 12px;background:var(--sunken);border:1px solid var(--line-strong);border-radius:6px;cursor:pointer}
@media (max-width:760px){.cols{grid-template-columns:1fr;overflow-x:visible}.keys{display:none}}
@media (prefers-reduced-motion:no-preference){.card{transition:border-color .15s}}
@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important;scroll-behavior:auto!important}}
"###;

const SCRIPT: &str = r###"(()=>{
"use strict";
const COLS=[["backlog","Backlog","status:backlog"],["blocked","Blocked","@blocked"],["ready","Ready","@working"],["active","Active","status:active"],["done","Done","completed.after:-7d"]];
const $=id=>document.getElementById(id);
const el=(t,c,x)=>{const e=document.createElement(t);if(c)e.className=c;if(x!=null)e.textContent=x;return e};
const W=document.body.dataset.writes==="1";
const state={data:{},byId:{},q:"",lanes:"none",sel:null,live:false,drag:null};
let timer=0,seq=0,ttimer=0;
async function api(method,params){
 const r=await fetch("/api",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({tasqx:"1",id:"1",method,params}),credentials:"same-origin"});
 let env;try{env=await r.json()}catch(e){throw new Error("HTTP "+r.status)}
 if(!env.ok){const er=new Error(env.error&&env.error.message||("HTTP "+r.status));er.code=env.error&&env.error.code;throw er}
 return env.result;
}
function showErr(m){const e=$("err");e.hidden=!m;e.textContent=m||""}
async function load(){
 if(state.drag){later();return}
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
 const li=el("li","card");li.tabIndex=0;li.dataset.id=t.short_id;li.dataset.col=col;state.byId[t.short_id]=t;
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
 li.addEventListener("click",()=>{if(!state.moved)open(t.short_id)});
 if(W)li.addEventListener("pointerdown",e=>grab(e,li));
 return li;
}
function paint(){
 const keep=state.sel,main=$("cols"),had=document.activeElement&&document.activeElement.dataset&&document.activeElement.dataset.id;
 main.replaceChildren();state.byId={};
 COLS.forEach(c=>{
  const rows=(state.data[c[0]]||[]).filter(t=>(c[0]!=="ready"||t.status!=="active")&&hit(t));
  const s=el("section","col");s.setAttribute("aria-labelledby","h-"+c[0]);s.dataset.col=c[0];
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
  if(W)b.append(acts(t));
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
 if(e.key==="Escape"){if(state.drag)end();else if(!$("panel").hidden)close();else if(typing)document.activeElement.blur();return}
 if(typing)return;
 if(e.key==="/"){e.preventDefault();$("q").focus()}
 else if(e.key==="j")move(0,1);else if(e.key==="k")move(0,-1);
 else if(e.key==="l")move(1,0);else if(e.key==="h")move(-1,0);
 else if(e.key==="Enter"){const k=document.activeElement;if(k&&k.classList.contains("card"))open(k.dataset.id)}
 else if(W&&(e.key==="s"||e.key==="d")){const k=document.activeElement;if(k&&k.classList.contains("card"))drop(k.dataset.id,k.dataset.col,e.key==="s"?"active":"done")}
});
const verbs={"task.start":"Started","task.stop":"Stopped","task.done":"Done","task.reopen":"Reopened","task.cancel":"Cancelled"};
const iso=d=>String(d||"").replace(/^P(T)?/,"").toLowerCase();
function plan(from,to){
 if(to==="blocked")return{no:"Blocked comes from dependencies; it is not a drop target."};
 if(to==="active")return{m:"task.start"};
 if(to==="done")return{m:"task.done"};
 if(to==="ready")return from==="active"?{m:"task.stop"}:from==="done"?{m:"task.reopen"}:from==="backlog"?{m:"task.modify",p:{set:{wait:null,scheduled:null}}}:{no:"It is blocked by its dependencies; finish those first."};
 return from==="ready"||from==="blocked"?{m:"task.modify",p:{set:{wait:"+1w"}}}:{no:"Only a ready task goes to Backlog; stop or reopen it first."};
}
function drop(id,from,to){
 if(from===to)return;
 const t=state.byId[id],pl=plan(from,to);
 if(pl.no)return toast("#"+id+": "+pl.no);
 act(t,pl.m,pl.p);
}
function said(m,p,r,id){
 const n="#"+id;
 if(m==="task.modify"){const s=p.set;
  if("priority" in s)return s.priority?"Priority "+s.priority+" on "+n:"Cleared the priority on "+n;
  return s.wait?n+" waits a week (Backlog)":n+" is ready: wait and scheduled cleared";}
 let x=verbs[m]+" "+n;
 if(m==="task.start"){if(r.already_running)x=n+" was already running";(r.auto_stopped||[]).forEach(a=>{x+=" · stopped #"+a.short_id+" after "+iso(a.tracked)})}
 if(m==="task.stop")x+=" after "+iso(r.interval);
 if((r.unblocked||[]).length)x+=" · unblocked "+r.unblocked.map(u=>"#"+u).join(" ");
 return x;
}
async function act(t,m,p){
 const id=t.short_id,rev=t._rev;
 try{
  const r=await api(m,Object.assign({ref:id,expected_rev:rev},p||{}));
  const x=said(m,p,r,id);
  // start and reopen are outside undo's exact set (D54): drag it back instead.
  if(m==="task.start"||m==="task.reopen")toast(x+". Drag it back to undo.");
  else toast(x,()=>undo(id,rev+1));
 }catch(e){
  if(e.code==="conflict"&&/expected_rev/.test(e.message)){
   try{const c=await api("task.get",{ref:id});toast("#"+id+": another session changed it (rev "+c._rev+"). Nothing was written; the card is up to date.")}catch(_){toast(e.message)}
  }else toast("#"+id+": "+e.message);
 }
 later();if(state.sel===String(id)||state.sel===id)open(id);
}
async function undo(id,rev){
 try{const r=await api("event.revert",{ref:id,expected_rev:rev});toast("Undid "+r.reverted.op+" on #"+id)}
 catch(e){toast(e.code==="conflict"?"#"+id+": another session changed it since, so Undo would take back theirs. Nothing was undone.":e.message)}
 later();
}
function toast(m,fn){
 const t=$("toast"),u=$("tundo");$("tmsg").textContent=m;t.hidden=false;u.hidden=!fn;
 u.onclick=fn?()=>{t.hidden=true;fn()}:null;
 clearTimeout(ttimer);ttimer=setTimeout(()=>{if(!t.contains(document.activeElement))t.hidden=true},fn?10000:6000);
}
function acts(t){
 const d=el("div","acts"),s=t.status,b=(x,m,p)=>{const k=el("button","",x);k.type="button";k.addEventListener("click",()=>act(t,m,p));d.append(k)};
 if(s==="pending"||s==="backlog")b("Start","task.start");
 if(s==="active")b("Stop","task.stop");
 if(s==="pending"||s==="active")b("Done","task.done");
 if(s==="done"||s==="cancelled")b("Reopen","task.reopen");
 if(s==="backlog")b("Ready","task.modify",{set:{wait:null,scheduled:null}});
 if(s==="pending")b("Backlog","task.modify",{set:{wait:"+1w"}});
 ["H","M","L"].forEach(x=>{if(t.priority!==x)b("Priority "+x,"task.modify",{set:{priority:x}})});
 if(t.priority)b("Clear priority","task.modify",{set:{priority:null}});
 if(s!=="done"&&s!=="cancelled")b("Cancel","task.cancel");
 return d;
}
// Drag: a mouse or pen moves a ghost; touch scrolls, and uses the panel.
function grab(e,li){
 if(e.button||e.pointerType==="touch")return;
 const x0=e.clientX,y0=e.clientY;state.moved=false;
 const mv=ev=>{
  if(!state.drag){if(Math.abs(ev.clientX-x0)+Math.abs(ev.clientY-y0)<6)return;start(li,x0,y0)}
  const d=state.drag;d.g.style.left=ev.clientX-d.dx+"px";d.g.style.top=ev.clientY-d.dy+"px";
  over(document.elementFromPoint(ev.clientX,ev.clientY));
 };
 const up=ev=>{
  removeEventListener("pointermove",mv);removeEventListener("pointerup",up);removeEventListener("pointercancel",up);
  const d=state.drag;if(!d)return;
  const hit=ev.type==="pointerup"&&document.elementFromPoint(ev.clientX,ev.clientY);
  end();
  if(!hit)return;
  const z=hit.closest(".tray b"),c=hit.closest(".col");
  if(z){const t=state.byId[d.id];z.dataset.cancel?act(t,"task.cancel"):act(t,"task.modify",{set:{priority:z.dataset.pri||null}})}
  else if(c)drop(d.id,d.from,c.dataset.col);
 };
 addEventListener("pointermove",mv);addEventListener("pointerup",up);addEventListener("pointercancel",up);
}
function start(li,x,y){
 const r=li.getBoundingClientRect(),g=li.cloneNode(true);
 g.className="card ghost";g.style.width=r.width+"px";g.removeAttribute("tabindex");document.body.append(g);
 li.classList.add("dragging");document.body.classList.add("dnd");state.moved=true;
 state.drag={id:li.dataset.id,from:li.dataset.col,li,g,dx:x-r.left,dy:y-r.top};
 const tr=$("tray");tr.hidden=false;
 tr.style.left=Math.max(8,Math.min(innerWidth-tr.offsetWidth-8,x-tr.offsetWidth/2))+"px";
 tr.style.top=Math.max(8,Math.min(innerHeight-tr.offsetHeight-8,y+48))+"px";
}
function over(t){
 document.querySelectorAll(".over,.no").forEach(n=>n.classList.remove("over","no"));
 if(!t)return;
 const z=t.closest(".tray b"),c=t.closest(".col");
 if(z)z.classList.add("over");
 else if(c&&c.dataset.col!==state.drag.from)c.classList.add("over",...(plan(state.drag.from,c.dataset.col).no?["no"]:[]));
}
function end(){
 const d=state.drag;state.drag=null;d.g.remove();d.li.classList.remove("dragging");document.body.classList.remove("dnd");$("tray").hidden=true;over(null);
 setTimeout(()=>{state.moved=false},0);
}
load();stream();
})();
"###;

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> String {
        page(&crate::theme::builtin("nord").expect("nord ships"), true)
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

    /// The script names no API method the listener would refuse: every
    /// method it spells is a listed read or write, and every write the
    /// listener lists is one the page can send.
    #[test]
    fn the_script_names_only_listed_methods() {
        use tasqx_core::board::{READ_METHODS, WRITE_METHODS};
        let listed =
            |m: &str| READ_METHODS.contains(&m) || WRITE_METHODS.iter().any(|(w, _)| *w == m);
        for (m, _, _) in tasqx_core::PARAMS {
            if SCRIPT.contains(&format!("\"{m}\"")) {
                assert!(
                    listed(m),
                    "the page sends `{m}`, which the board does not serve"
                );
            }
        }
        for (m, _) in WRITE_METHODS {
            assert!(
                SCRIPT.contains(&format!("\"{m}\"")),
                "nothing on the page sends `{m}`"
            );
        }
    }

    /// `--scope read` reaches the page as `data-writes="0"`, which the script
    /// reads before it wires a drag, a button or a key that writes.
    #[test]
    fn the_scope_reaches_the_page() {
        let theme = crate::theme::builtin("nord").expect("nord ships");
        assert!(page(&theme, false).contains("<body data-writes=\"0\">"));
        assert!(page(&theme, true).contains("<body data-writes=\"1\">"));
        assert!(SCRIPT.contains("const W=document.body.dataset.writes===\"1\""));
        for gate in [
            "if(W)li.addEventListener",
            "if(W)b.append(acts(t))",
            "else if(W&&",
        ] {
            assert!(SCRIPT.contains(gate), "the write path is not gated: {gate}");
        }
    }

    #[test]
    fn it_has_light_and_dark_tokens_and_honours_reduced_motion() {
        let doc = built();
        assert!(doc.contains("color-scheme:light") && doc.contains("color-scheme:dark"));
        assert!(doc.contains("prefers-reduced-motion:reduce"));
        assert!(doc.contains(":focus-visible"));
    }

    // ---- check 2: columns sorted by urgency, in D119's three bands ----

    /// The page asks the engine for urgency order (`-urgency`, the sort `list`
    /// uses), and its three bands and gauge sit on the scale the terminal's
    /// `urgency_scale` and `Theme::ramp_band` use: bands at half and all of
    /// `DUE_WEIGHT`, the gauge full at `DUE_WEIGHT`.
    #[test]
    fn urgency_is_sorted_and_banded_on_the_d119_scale() {
        use tasqx_core::urgency::DUE_WEIGHT;
        assert!(SCRIPT.contains(r#"sort:["-urgency"]"#));
        let (warn, danger) = (DUE_WEIGHT / 2.0, DUE_WEIGHT);
        let band =
            format!(r#"function band(u){{return u>={danger}?"danger":u>={warn}?"warn":""}}"#);
        assert!(
            SCRIPT.contains(&band),
            "band thresholds drifted from DUE_WEIGHT"
        );
        assert!(
            SCRIPT.contains(&format!("u/{DUE_WEIGHT}*100")),
            "gauge scale drifted"
        );
        // The same three bands as the terminal ramp, at every half point.
        let theme = crate::theme::builtin("nord").unwrap();
        for step in 0..=40 {
            let u = f64::from(step) * 0.5;
            let rust = theme.ramp_band(crate::render::urgency_scale(u));
            let quiet = theme.ramp_band(0.0);
            let js = if u >= danger {
                2
            } else if u >= warn {
                1
            } else {
                0
            };
            let want = [quiet, theme.ramp_band(0.5), theme.ramp_band(1.0)][js];
            assert_eq!(rust, want, "urgency {u}");
        }
    }

    // ---- check 5: keyboard, focus, contrast, reduced motion ----

    /// Every action has a keyboard path: cards are focusable and open on Enter,
    /// `j`/`k`/`h`/`l` move, `/` searches, Esc closes, and the search box, lane
    /// menu and close control are native elements a keyboard already reaches.
    #[test]
    fn every_action_has_a_keyboard_path() {
        for key in [
            r#"e.key==="j""#,
            r#"e.key==="k""#,
            r#"e.key==="h""#,
            r#"e.key==="l""#,
            r#"e.key==="Enter""#,
            r#"e.key==="Escape""#,
            r#"e.key==="/""#,
        ] {
            assert!(SCRIPT.contains(key), "no handler for {key}");
        }
        assert!(SCRIPT.contains("li.tabIndex=0"), "cards must be focusable");
        for native in [
            "<input id=\"q\"",
            "<select id=\"lanes\"",
            "<button id=\"pclose\"",
            "<a class=\"skip\"",
        ] {
            assert!(BODY.contains(native), "missing {native}");
        }
        assert!(
            SCRIPT.contains("$(\"pclose\").focus()") && SCRIPT.contains("k.focus()"),
            "focus moves into the panel and back to the card"
        );
    }

    /// Focus is drawn, never removed: a visible outline on `:focus-visible`,
    /// and no rule anywhere that turns outlines off.
    #[test]
    fn focus_is_visible_and_never_removed() {
        assert!(CSS.contains(":focus-visible{outline:3px solid var(--accent)"));
        assert!(!CSS.contains("outline:none") && !CSS.contains("outline:0"));
    }

    /// Reduced motion: transitions exist only under `no-preference`, and the
    /// `reduce` block switches animation, transition and smooth scrolling off.
    #[test]
    fn motion_is_off_when_the_user_asks() {
        assert!(CSS.contains("@media (prefers-reduced-motion:no-preference){.card{transition"));
        assert!(CSS.contains("@media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important;scroll-behavior:auto!important}}"));
        let outside = CSS.replace(
            "@media (prefers-reduced-motion:no-preference){.card{transition:border-color .15s}}",
            "",
        );
        assert!(
            !outside
                .replace("transition:none!important", "")
                .contains("transition:"),
            "a transition outside the no-preference block"
        );
    }

    /// The colour of a token in one scheme's block; a `color-mix` against
    /// white or black (the dark palette's neutrals) is resolved in sRGB.
    fn resolve(block: &str, name: &str) -> crate::theme::Rgb {
        use crate::theme::Rgb;
        let at = block.find(name).unwrap_or_else(|| panic!("{name} missing"));
        let rest = &block[at + name.len()..];
        let val = rest[..rest
            .find(";--")
            .or_else(|| rest.find(";color-scheme"))
            .unwrap_or(rest.len())]
            .trim();
        if let Some(mix) = val.strip_prefix("color-mix(in srgb,") {
            let mut parts = mix.trim_end_matches(')').split(',');
            let (a, pct) = parts.next().unwrap().split_once(' ').unwrap();
            let b = parts.next().unwrap();
            let p = pct.trim_end_matches('%').parse::<f64>().unwrap() / 100.0;
            let (a, b) = (Rgb::parse_hex(a).unwrap(), Rgb::parse_hex(b).unwrap());
            let m = |x: u8, y: u8| (f64::from(x) * p + f64::from(y) * (1.0 - p)).round() as u8;
            Rgb::new(m(a.r, b.r), m(a.g, b.g), m(a.b, b.b))
        } else {
            Rgb::parse_hex(val).unwrap_or_else(|| panic!("{name}: {val:?}"))
        }
    }

    /// AA for every text pair the board's CSS uses (4.5:1) and 3:1 for its non-text marks, in both
    /// schemes of every built-in theme. `html::palette` guarantees the role
    /// colours; this covers the neutrals and the on-accent pairs the board adds.
    #[test]
    fn every_text_pair_the_board_uses_clears_aa_in_both_schemes() {
        for name in crate::theme::BUILTINS {
            let doc = page(&crate::theme::builtin(name).unwrap(), true);
            let light = &doc[doc.find(":root{--bg:").unwrap()..];
            let light = &light[..light.find('}').unwrap()];
            let dark = &doc[doc.find("[data-theme=\"dark\"]{").unwrap()..];
            let dark = &dark[..dark.find('}').unwrap()];
            for (scheme, block) in [("light", light), ("dark", dark)] {
                let c = |n: &str| resolve(block, n);
                let grounds = [c("--bg:"), c("--surface:"), c("--sunken:")];
                // Text: body and secondary text on every ground; the status
                // colours only ever sit on the header bar and cards (surface).
                // (`--muted` never sits on `--sunken`: that ground is the search
                // box and lane menu, which carry body text only.)
                for text in ["--fg:", "--muted:"] {
                    let n = if text == "--fg:" { 3 } else { 2 };
                    for (g, ground) in ["bg", "surface", "sunken"].iter().zip(grounds).take(n) {
                        let r = crate::html::contrast_ratio(c(text), ground);
                        assert!(r >= 4.5, "{name} {scheme} {text} on {g}: {r:.2}:1");
                    }
                }
                for text in ["--good:", "--danger:"] {
                    let r = crate::html::contrast_ratio(c(text), grounds[1]);
                    assert!(r >= 4.5, "{name} {scheme} {text} on surface: {r:.2}:1");
                }
                // Non-text (focus ring, card edge, gauge fill): 3:1.
                for mark in ["--accent:", "--warn:", "--danger:"] {
                    for ground in [grounds[1], grounds[2]] {
                        let r = crate::html::contrast_ratio(c(mark), ground);
                        assert!(r >= 3.0, "{name} {scheme} {mark} mark: {r:.2}:1");
                    }
                }
                for ground in ["--accent:", "--danger:"] {
                    let r = crate::html::contrast_ratio(c("--on-accent:"), c(ground));
                    assert!(r >= 4.5, "{name} {scheme} on-accent on {ground}: {r:.2}:1");
                }
            }
        }
    }
}
