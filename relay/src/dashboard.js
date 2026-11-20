// The private analytics dashboard: how many people download Temino and
// how many use it. Served by this Worker under /dashboard/ and shown at
// DASHBOARD_ORIGIN (dashboard.bucaastudio.com): a Vercel project in the
// bucaa team (relay/dashboard-site/vercel.json, deployed with the Vercel
// CLI) proxies every path there to /dashboard/<path> here.
//
// Sign-in reuses the relay's Google sign-in. /login sends you to Google
// with a request whose return is DASHBOARD_ORIGIN/done; the callback
// (index.js `finish`) hands a dashboardFinish, which checks the address
// against DASHBOARD_EMAILS (a secret, comma-separated) and sends back a
// two-minute ticket tied to a state cookie. /done trades it for a
// 30-day session cookie on the dashboard's own host.
//
// The page reads the usage pings straight from KV (analytics.js stores
// them) and the download counts from GitHub's releases API.

import { COUNTERS, lastDays, dayKey } from "./analytics.js";
import { random, sign, unsign } from "./sign.js";

const DAY_MS = 24 * 60 * 60 * 1000;
const SESSION_DAYS = 30;
const REPO = "bluevisor/temino";

const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const fmt = (n) => Math.round(n).toLocaleString("en-US");

/// Where the dashboard is seen, with no trailing slash: links and the
/// sign-in's return go there.
function origin(env, url) {
  return (env.DASHBOARD_ORIGIN || `${url.origin}/dashboard`).replace(/\/+$/, "");
}

function allowed(env) {
  return String(env.DASHBOARD_EMAILS || "").split(",").map((e) => e.trim().toLowerCase()).filter(Boolean);
}

// Cookie names: `__Host-` where the dashboard is on https (it needs
// Secure, Path=/ and no Domain), plain on a local http dev server.
function cookies(env, url) {
  const secure = origin(env, url).startsWith("https:");
  const attrs = `Path=/; HttpOnly; SameSite=Lax${secure ? "; Secure" : ""}`;
  return { session: secure ? "__Host-dash" : "dash", state: secure ? "__Host-dash_state" : "dash_state", attrs };
}

function readCookie(request, name) {
  for (const part of (request.headers.get("cookie") || "").split(";")) {
    const [key, ...rest] = part.trim().split("=");
    if (key === name) return rest.join("=");
  }
  return null;
}

const HEADERS = {
  "content-type": "text/html; charset=utf-8",
  "cache-control": "no-store",
  "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  "x-frame-options": "DENY",
  "referrer-policy": "no-referrer",
  "x-robots-tag": "noindex",
};

function respond(body, status = 200, extra = []) {
  const headers = new Headers(HEADERS);
  for (const [k, v] of extra) headers.append(k, v);
  return new Response(body, { status, headers });
}

function redirect(location, extra = []) {
  const headers = new Headers({ location, "cache-control": "no-store", "referrer-policy": "no-referrer" });
  for (const [k, v] of extra) headers.append(k, v);
  return new Response(null, { status: 302, headers });
}

export async function dashboard(request, env, url) {
  if (request.method !== "GET") return new Response("method not allowed", { status: 405 });
  const path = url.pathname.slice("/dashboard".length).replace(/^\/+/, "");
  const base = origin(env, url);
  const names = cookies(env, url);
  if (path === "login") {
    const state = random();
    const dev = url.searchParams.get("with") === "dev" && env.DEV_LOGIN === "1";
    const req = await sign(env, {
      kind: "dashboard", ret: `${base}/done`, state, nonce: random(), name: "Dashboard",
      hint: allowed(env)[0] || "", exp: Date.now() + 10 * 60 * 1000,
    });
    return redirect(`${url.origin}/auth/${dev ? "dev" : "google"}?req=${encodeURIComponent(req)}`, [
      ["set-cookie", `${names.state}=${state}; ${names.attrs}; Max-Age=600`],
    ]);
  }
  if (path === "done") {
    const ticket = await unsign(env, url.searchParams.get("ticket"));
    const state = readCookie(request, names.state);
    if (!ticket || !ticket.dash || !state || ticket.state !== state || !allowed(env).includes(ticket.dash)) {
      return respond(signInPage(base, env, "That sign-in didn't go through. Try again."), 400);
    }
    const session = await sign(env, { dash: ticket.dash, exp: Date.now() + SESSION_DAYS * DAY_MS });
    return redirect(`${base}/`, [
      ["set-cookie", `${names.session}=${session}; ${names.attrs}; Max-Age=${SESSION_DAYS * 86400}`],
      ["set-cookie", `${names.state}=; ${names.attrs}; Max-Age=0`],
    ]);
  }
  if (path === "logout") {
    return redirect(`${base}/`, [["set-cookie", `${names.session}=; ${names.attrs}; Max-Age=0`]]);
  }
  if (path !== "") return respond(signInPage(base, env, "Nothing here."), 404);
  const session = await unsign(env, readCookie(request, names.session));
  if (!session || !allowed(env).includes(session.dash)) return respond(signInPage(base, env));
  const [records, aggregates] = await readPings(env);
  const releases = await readReleases();
  const stats = buildStats(records, aggregates, releases, dayKey(new Date()));
  return respond(dashboardPage(base, session.dash, stats));
}

/// The end of a dashboard sign-in (from index.js `finish`): a verified
/// address on the list gets a short ticket back to the dashboard.
export async function dashboardFinish(env, req, who) {
  const email = String(who.email || "").toLowerCase();
  const verified = who.verified || (who.provider === "dev" && env.DEV_LOGIN === "1");
  if (!verified || !allowed(env).includes(email)) {
    return respond(shell("Not allowed", `<main class="narrow card"><h1>Not allowed</h1>
<p>That account can't open this dashboard.</p></main>`), 403);
  }
  const ticket = await sign(env, { dash: email, state: req.state, exp: Date.now() + 2 * 60 * 1000 });
  return redirect(`${req.ret}?ticket=${encodeURIComponent(ticket)}`);
}

// ---- data

/// Every stored install-day ({ day, hash, os, arch, version, counters })
/// and the per-day counter totals.
async function readPings(env) {
  const keys = [];
  let cursor;
  do {
    const page = await env.ACCOUNTS.list({ prefix: "analytics:", cursor, limit: 1000 });
    keys.push(...page.keys.map((k) => k.name));
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  const records = [];
  const aggregates = {};
  for (let i = 0; i < keys.length; i += 50) {
    await Promise.all(keys.slice(i, i + 50).map(async (name) => {
      const [, day, hash] = name.split(":");
      let value = {};
      try {
        value = JSON.parse(await env.ACCOUNTS.get(name)) || {};
      } catch {}
      if (day === "agg") aggregates[hash] = value;
      else records.push({ day, hash, os: value.os, arch: value.arch, version: value.version, counters: value.counters || {} });
    }));
  }
  return [records, aggregates];
}

/// The releases, oldest first, with each asset's download count; null
/// when GitHub can't be reached. Cached ten minutes (GitHub allows 60
/// unauthenticated calls an hour).
async function readReleases() {
  const api = `https://api.github.com/repos/${REPO}/releases?per_page=100`;
  try {
    const cache = caches.default;
    let response = await cache.match(api);
    if (!response) {
      const fresh = await fetch(api, { headers: { "user-agent": "temino-relay", accept: "application/vnd.github+json" } });
      if (!fresh.ok) return null;
      response = new Response(await fresh.text(), { headers: { "content-type": "application/json", "cache-control": "max-age=600" } });
      await cache.put(api, response.clone());
    }
    const list = await response.json();
    return list
      .filter((r) => !r.draft)
      .map((r) => ({ tag: r.tag_name, date: r.published_at, assets: (r.assets || []).map((a) => ({ name: a.name, count: a.download_count })) }))
      .sort((a, b) => (a.date < b.date ? -1 : 1));
  } catch {
    return null;
  }
}

/// Which download an asset is, by its name.
export function platformOf(name) {
  const rules = [
    [/macos-arm64|macos-aarch64/, "macOS Apple silicon"],
    [/macos-x86_64/, "macOS Intel"],
    [/macos/, "macOS"],
    [/linux-aarch64/, "Linux arm64"],
    [/linux-x86_64/, "Linux x86_64"],
    [/arm64\.hpkg$/, "Haiku arm64"],
    [/x86_64\.hpkg$/, "Haiku x86_64"],
  ];
  for (const [rule, label] of rules) if (rule.test(name)) return label;
  return "Other";
}

const OS_NAMES = { macos: "macOS", linux: "Linux", haiku: "Haiku" };
const ARCH_NAMES = { aarch64: "arm64", x86_64: "x86_64", x86: "x86" };

/// An install's platform, named as its download is (platformOf).
function installPlatform(os, arch) {
  if (os === "macos") return arch === "aarch64" ? "macOS Apple silicon" : arch === "x86_64" ? "macOS Intel" : "macOS";
  return `${OS_NAMES[os] || os || "Unknown"} ${ARCH_NAMES[arch] || arch || ""}`.trim();
}

/// Everything the page shows, worked out from the stored pings and the
/// releases (null when GitHub was out of reach).
export function buildStats(records, aggregates, releases, today) {
  const days30 = lastDays(today, 30);
  const days7 = new Set(lastDays(today, 7));
  const in30 = new Set(days30);
  const daily = Object.fromEntries(days30.map((d) => [d, 0]));
  const firstSeen = {};
  const latest = {};
  const week = new Set();
  const month = new Set();
  for (const r of records) {
    if (r.day in daily) daily[r.day] += 1;
    if (!firstSeen[r.hash] || r.day < firstSeen[r.hash]) firstSeen[r.hash] = r.day;
    if (in30.has(r.day)) month.add(r.hash);
    if (days7.has(r.day)) {
      week.add(r.hash);
      if (!latest[r.hash] || r.day > latest[r.hash].day) latest[r.hash] = r;
    }
  }
  const platforms = {};
  const versions = {};
  for (const r of Object.values(latest)) {
    const platform = installPlatform(r.os, r.arch);
    platforms[platform] = (platforms[platform] || 0) + 1;
    const version = r.version || "Unknown";
    versions[version] = (versions[version] || 0) + 1;
  }
  const features = Object.fromEntries(COUNTERS.map((c) => [c, 0]));
  for (const day of days7) {
    for (const [name, n] of Object.entries(aggregates[day] || {})) if (name in features) features[name] += n;
  }
  let downloads = null;
  if (releases) {
    const perRelease = releases.map((r) => ({ tag: r.tag, date: r.date, total: r.assets.reduce((sum, a) => sum + a.count, 0) }));
    const byPlatform = {};
    for (const r of releases) for (const a of r.assets) byPlatform[platformOf(a.name)] = (byPlatform[platformOf(a.name)] || 0) + a.count;
    downloads = {
      total: perRelease.reduce((sum, r) => sum + r.total, 0),
      latest: perRelease.at(-1) || null,
      perRelease,
      byPlatform,
    };
  }
  return {
    today,
    daily,
    activeToday: daily[today] || 0,
    active7: week.size,
    active30: month.size,
    new7: Object.values(firstSeen).filter((d) => days7.has(d)).length,
    platforms,
    versions,
    features,
    downloads,
  };
}

// ---- pages

const STYLE = `
:root{color-scheme:light dark;
  --plane:#f9f9f7;--surface:#fcfcfb;--ink:#0b0b0b;--ink-2:#52514e;--muted:#6f6d68;
  --grid:#e1e0d9;--axis:#c3c2b7;--border:rgba(11,11,11,.10);--series:#2a78d6;--focus:#2a78d6}
@media (prefers-color-scheme:dark){:root{
  --plane:#0d0d0d;--surface:#1a1a19;--ink:#f0efec;--ink-2:#c3c2b7;--muted:#9b9993;
  --grid:#2c2c2a;--axis:#383835;--border:rgba(255,255,255,.10);--series:#3987e5;--focus:#3987e5}}
*{box-sizing:border-box}
html{background:var(--plane)}
body{margin:0;color:var(--ink);font:15px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;-webkit-text-size-adjust:100%}
a{color:inherit}
:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
main{max-width:1080px;margin:0 auto;padding:24px 16px 48px}
.narrow{max-width:380px;margin:12vh auto 0;text-align:center}
.card{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:20px}
h1{font-size:22px;margin:0}
h2{font-size:15px;margin:0 0 12px}
p{margin:0 0 12px;color:var(--ink-2)}
header{display:flex;flex-wrap:wrap;gap:8px 16px;align-items:baseline;justify-content:space-between;margin-bottom:20px}
header p{margin:0;font-size:13px}
.button{display:block;padding:11px 14px;border-radius:10px;border:1px solid var(--border);background:var(--ink);color:var(--plane);font-weight:600;text-decoration:none;margin-top:16px}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px;margin-bottom:12px}
.tile .value{font-size:28px;font-weight:600;font-variant-numeric:tabular-nums;line-height:1.1}
.tile .label{font-size:13px;color:var(--ink-2);margin-top:4px}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,440px),1fr));gap:12px;margin-top:12px}
.note{font-size:13px;color:var(--ink-2)}
figure{margin:0}
.plot{position:relative;height:160px;margin:8px 0 4px 32px}
.tick{position:absolute;left:0;right:0;border-top:1px solid var(--grid)}
.tick span{position:absolute;right:calc(100% + 6px);top:-8px;font-size:11px;color:var(--muted);font-variant-numeric:tabular-nums}
.tick.base{border-top-color:var(--axis)}
.bars{position:absolute;inset:0;display:grid;grid-auto-flow:column;grid-auto-columns:1fr;align-items:end;margin:0;padding:0;list-style:none}
.bars li{height:100%;display:flex;flex-direction:column;justify-content:flex-end;align-items:center;position:relative}
.bar{width:min(24px,calc(100% - 2px));height:var(--v);background:var(--series);border-radius:4px 4px 0 0}
.bars li:hover .bar{filter:brightness(1.12)}
.cap{position:absolute;font-size:12px;font-weight:600;font-variant-numeric:tabular-nums;color:var(--ink);transform:translateY(-100%);padding-bottom:2px}
.xlabels{display:flex;justify-content:space-between;margin-left:32px;font-size:11px;color:var(--muted)}
details{margin-top:10px;font-size:13px}
summary{cursor:pointer;color:var(--ink-2)}
.table-wrapper{overflow:auto;max-inline-size:100%;max-block-size:min(420px,70vh)}
table{border-collapse:separate;border-spacing:0;width:100%;font-size:14px}
caption{text-align:left;font-weight:600;padding-bottom:8px}
th,td{padding:6px 8px;border-bottom:1px solid var(--grid);text-align:left;white-space:nowrap}
thead th{position:sticky;inset-block-start:0;background:var(--surface);color:var(--ink-2);font-weight:500;font-size:13px}
td.n,th.n{text-align:right;font-variant-numeric:tabular-nums}
tbody th{font-weight:400}
`;

function shell(title, body) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark"><meta name="robots" content="noindex">
<title>${esc(title)}</title><style>${STYLE}</style></head><body>${body}</body></html>`;
}

function signInPage(base, env, problem = "") {
  const dev = env.DEV_LOGIN === "1" ? `<a class="button" href="${esc(base)}/login?with=dev">Dev sign-in (local only)</a>` : "";
  return shell("Temino dashboard", `<main class="narrow card"><h1>Temino dashboard</h1>
<p>${problem ? esc(problem) : "Downloads and usage. Private: sign in to see it."}</p>
<a class="button" href="${esc(base)}/login">Continue with Google</a>${dev}</main>`);
}

/// Clean axis ticks for values up to `max`: a 1, 2 or 5 step, at most
/// four steps.
export function niceTicks(max) {
  const top = Math.max(1, max);
  const raw = top / 4;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * power).find((s) => s >= raw && Number.isInteger(s)) || Math.ceil(raw);
  const ticks = [];
  for (let v = 0; v < top + step; v += step) {
    ticks.push(v);
    if (v >= top) break;
  }
  return ticks;
}

function table(caption, id, head, rows) {
  return `<div class="table-wrapper" tabindex="0" role="region" aria-labelledby="${id}"><table>
<caption id="${id}">${esc(caption)}</caption>
<thead><tr>${head.map((h, i) => `<th scope="col"${i ? ' class="n"' : ""}>${esc(h)}</th>`).join("")}</tr></thead>
<tbody>${rows.map((r) => `<tr><th scope="row">${esc(r[0])}</th>${r.slice(1).map((c) => `<td class="n">${esc(c)}</td>`).join("")}</tr>`).join("")}</tbody>
</table></div>`;
}

/// A one-series column chart: hairline grid with clean ticks, columns
/// from the baseline, the last column's value on its cap, a hover title
/// per column, and the same numbers as a table.
function columns(id, title, points, unit, first, last) {
  const ticks = niceTicks(Math.max(0, ...points.map((p) => p.value)));
  const top = ticks.at(-1);
  const grid = ticks
    .map((t) => `<div class="tick${t === 0 ? " base" : ""}" style="bottom:${(t / top) * 100}%"><span>${fmt(t)}</span></div>`)
    .join("");
  const bars = points
    .map((p, i) => {
      const cap = i === points.length - 1 && p.value > 0 ? `<span class="cap" style="bottom:${(p.value / top) * 100}%">${fmt(p.value)}</span>` : "";
      return `<li title="${esc(`${p.label}: ${fmt(p.value)} ${unit}`)}"><span class="bar" style="--v:${(p.value / top) * 100}%"></span>${cap}</li>`;
    })
    .join("");
  return `<figure class="card"><h2 id="${id}">${esc(title)}</h2>
<div class="plot" role="img" aria-labelledby="${id}">${grid}<ol class="bars" aria-hidden="true">${bars}</ol></div>
<div class="xlabels" aria-hidden="true"><span>${esc(first)}</span><span>${esc(last)}</span></div>
<details><summary>Show as a table</summary>${table(title, `${id}-table`, ["", unit], points.map((p) => [p.label, fmt(p.value)]))}</details>
</figure>`;
}

const shortDay = (day) => new Date(`${day}T00:00:00Z`).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });

const FEATURE_NAMES = {
  launches: "Launches",
  active_minutes: "Hours in use",
  commands_completed: "Commands run",
  blocks_created: "Command blocks",
  tabs_opened: "Tabs opened",
  panes_opened: "Panes opened",
  ai_queries: "AI requests",
  remote_sessions: "Remote Control sessions",
  viewer_opens: "Files and pages opened",
  find_uses: "Find uses",
  update_installs: "Updates installed",
  settings_changes: "Settings changes",
  crashes: "Crashes",
  update_failures: "Failed updates",
};

const sorted = (counts) => Object.entries(counts).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));

function tile(value, label) {
  return `<div class="card tile"><div class="value">${esc(value)}</div><div class="label">${esc(label)}</div></div>`;
}

export function dashboardPage(base, email, s) {
  const d = s.downloads;
  const tiles = [
    tile(fmt(s.activeToday), "Active installs today"),
    tile(fmt(s.active7), "Active in the last 7 days"),
    tile(fmt(s.active30), "Active in the last 30 days"),
    tile(fmt(s.new7), "New in the last 7 days"),
    tile(d ? fmt(d.total) : "—", "Downloads, all releases"),
    tile(d && d.latest ? fmt(d.latest.total) : "—", d && d.latest ? `Downloads of ${d.latest.tag}` : "Latest release"),
  ].join("");
  const days = Object.keys(s.daily);
  const usage = columns("active", "Active installs per day", days.map((day) => ({ label: shortDay(day), value: s.daily[day] })), "installs", shortDay(days[0]), shortDay(days.at(-1)));
  let downloads = `<section class="card"><h2>Downloads</h2><p class="note">GitHub couldn't be reached; try again in a minute.</p></section>`;
  if (d) {
    const recent = d.perRelease.slice(-15);
    downloads = columns("downloads", "Downloads per release", recent.map((r) => ({ label: r.tag, value: r.total })), "downloads", recent[0]?.tag || "", recent.at(-1)?.tag || "");
  }
  const features = sorted(s.features)
    .filter(([, n]) => n > 0)
    .map(([name, n]) => [FEATURE_NAMES[name] || name, name === "active_minutes" ? (n / 60).toFixed(1) : fmt(n)]);
  const tables = [
    table("Installs by platform, last 7 days", "by-platform", ["Platform", "Installs"], sorted(s.platforms).map(([k, n]) => [k, fmt(n)])),
    table("Installs by version, last 7 days", "by-version", ["Version", "Installs"], sorted(s.versions).map(([k, n]) => [k, fmt(n)])),
    table("Feature use, last 7 days", "features", ["Feature", "Total"], features.length ? features : [["No usage yet", "0"]]),
    d ? table("Downloads by platform, all releases", "dl-platform", ["Platform", "Downloads"], sorted(d.byPlatform).map(([k, n]) => [k, fmt(n)])) : "",
  ].filter(Boolean).map((t) => `<section class="card">${t}</section>`).join("");
  return shell("Temino dashboard", `<main>
<header><div><h1>Temino</h1><p>Downloads and usage · ${esc(shortDay(s.today))} (UTC)</p></div>
<p>${esc(email)} · <a href="${esc(base)}/logout">Sign out</a></p></header>
<div class="tiles">${tiles}</div>
<div class="grid">${usage}${downloads}</div>
<div class="grid">${tables}</div>
<p class="note" style="margin-top:16px">Usage counts only installs whose owner chose to share anonymous usage data; each sends one ping a day, kept 40 days. Downloads are GitHub's counts and include automatic updates, which fetch the same files.</p>
</main>`);
}
