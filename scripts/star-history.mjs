#!/usr/bin/env node
/* Star curve: the one line of cumulative stars against the date, and nothing else.
 *
 * Why draw it ourselves. We used `lowlighter/metrics`, and it ties the "cumulative total" and
 * "new per day" charts **to one and the same switch** (`plugin_stargazers_charts`), with no way
 * in the template to keep only one of them. And the one we want is the traditional cumulative
 * line.
 *
 * Drawing it ourselves also got rid of one thing along the way: that was a third-party action
 * running under `contents: write`. What runs under that permission now is this script.
 *
 * **On 2026-06-30 GitHub restricted the stargazer timeline to a repository's own
 * admins/collaborators**, so a token with enough permission is mandatory -- an anonymous call
 * cannot get it any more, and sites like star-history.com have returned nothing but a placeholder
 * image ever since.
 */
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

const [owner, repo] = (process.env.REPO ?? "").split("/");
const token = process.env.GITHUB_TOKEN;
const out = process.env.OUT ?? "assets/star-history.svg";
if (!owner || !repo) throw new Error("REPO must be set as owner/name");
if (!token) throw new Error("GITHUB_TOKEN must be set");

/** 100 per page, paged to the end with a cursor. 1868 stars = 19 pages, not worth adding
 * concurrency for.
 *
 * **GraphQL, not REST.** REST's `/repos/{o}/{r}/stargazers` answers 404 with this same token:
 * that endpoint wants a token carrying repository-level scope, and this one only has `read:org`;
 * GitHub answers 404 instead of 403 for a resource you have no access to, so as not to leak
 * whether it exists. GraphQL's `stargazers` connection does accept this token -- verified in CI.
 * Changing this beats loosening the credentials for the sake of one endpoint. */
async function stargazerDates() {
  /* `viewerPermission` and `totalCount` are not decoration. **For restricted data GitHub returns
     an empty collection, not an error** -- looking at `edges` alone, "no permission" and
     "genuinely zero stars" look exactly alike. Fetching these two along with it means a failure
     can say which of the two it was */
  const query = `query($owner:String!,$name:String!,$cursor:String){
    repository(owner:$owner,name:$name){
      stargazerCount
      viewerPermission
      stargazers(first:100,after:$cursor,orderBy:{field:STARRED_AT,direction:ASC}){
        totalCount
        pageInfo{hasNextPage endCursor}
        edges{starredAt}
      }
    }
  }`;
  const dates = [];
  let cursor = null;
  for (let page = 1; page <= 400; page++) {
    const res = await fetch("https://api.github.com/graphql", {
      method: "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
        "user-agent": `${owner}-star-history`,
      },
      body: JSON.stringify({ query, variables: { owner, name: repo, cursor } }),
    });
    if (!res.ok) {
      throw new Error(`graphql page ${page}: ${res.status} ${await res.text()}`);
    }
    const body = await res.json();
    // **GraphQL answers 200 even on an error**, so we have to read `errors` ourselves --
    // otherwise it only shows up when something below reads undefined, and by then the original
    // error text is already gone
    if (body.errors) {
      throw new Error(`graphql page ${page}: ${JSON.stringify(body.errors)}`);
    }
    const node = body.data?.repository;
    const conn = node?.stargazers;
    if (!conn) throw new Error(`graphql page ${page}: no stargazers in response`);
    if (page === 1) {
      console.log(
        `repo sees ${node.stargazerCount} stars; connection reports ` +
          `${conn.totalCount}; token permission = ${node.viewerPermission}`,
      );
    }
    for (const e of conn.edges) if (e.starredAt) dates.push(new Date(e.starredAt));
    if (!conn.pageInfo.hasNextPage) return dates;
    cursor = conn.pageInfo.endCursor;
  }
  return dates;
}

const dates = (await stargazerDates()).sort((a, b) => a - b);
if (dates.length === 0) {
  // The log line above has already said how many stars the repo reports, how many the connection
  // reports, and what permission the token has.
  // **Do not silently emit an empty chart here** -- a curve drawn at zero is worse than no chart
  throw new Error(
    "no stargazer timestamps came back — see the line above for what the API " +
      "reported. An empty connection with a non-zero star count means the token " +
      "cannot read the stargazer timeline (restricted to admins and collaborators " +
      "since 2026-06-30).",
  );
}

/* Aggregated by day into a cumulative value. **Every day needs a point**, even a day with no
   new stars -- skip over the gaps and the spacing of the x axis no longer stands for time, which
   makes the slope of the curve a lie */
const DAY = 86400000;
const day0 = Date.UTC(
  dates[0].getUTCFullYear(),
  dates[0].getUTCMonth(),
  dates[0].getUTCDate(),
);
const today = Date.now();
const perDay = new Map();
for (const d of dates) {
  const k = Math.floor((Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate()) - day0) / DAY);
  perDay.set(k, (perDay.get(k) ?? 0) + 1);
}
const lastDay = Math.floor((today - day0) / DAY);
const series = [];
let total = 0;
for (let k = 0; k <= lastDay; k++) {
  total += perDay.get(k) ?? 0;
  series.push({ t: day0 + k * DAY, v: total });
}

// ---- Drawing
const W = 800, H = 400;
/* More room at the top than anywhere else: the final point's value is labelled above it, and
   **the final point is always at the top** -- a cumulative value is monotonically
   non-decreasing, so the last point is the maximum */
const PAD = { top: 40, right: 28, bottom: 40, left: 64 };
const plotW = W - PAD.left - PAD.right;
const plotH = H - PAD.top - PAD.bottom;
const maxV = series[series.length - 1].v;
const x = (i) => PAD.left + (plotW * i) / Math.max(1, series.length - 1);
const y = (v) => PAD.top + plotH - (plotH * v) / Math.max(1, maxV);

/** Axis ticks take round numbers, not the fractional sort that max/5 gives you */
function ticks(max, count = 5) {
  const raw = max / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? mag * 10;
  const out = [];
  for (let v = 0; v <= max; v += step) out.push(Math.round(v));
  return out;
}
const fmtDate = (t) =>
  new Date(t).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });

/** Smoothed into cubic Béziers with **monotone** interpolation (Fritsch–Carlson).
 *
 * Not plain Catmull-Rom: cumulative stars only ever rise, while a plain spline overshoots where
 * the slope changes abruptly and draws a dip -- **which is the chart saying stars were lost**.
 * Monotone interpolation clamps each segment's tangent into a range that creates no extrema, so
 * the curve can never run backwards.
 *
 * On the flat days (zero new stars that day) the tangent is zero, so joining them up does not
 * bulge either. */
function smoothPath(pts) {
  const n = pts.length;
  if (n < 2) return `M${pts[0].x.toFixed(1)},${pts[0].y.toFixed(1)}`;
  // Slope of each segment
  const dx = [], dy = [], slope = [];
  for (let i = 0; i < n - 1; i++) {
    dx.push(pts[i + 1].x - pts[i].x);
    dy.push(pts[i + 1].y - pts[i].y);
    slope.push(dy[i] / dx[i]);
  }
  // Tangent at each point: zero when the two neighbouring segments have opposite signs (or one
  // of them is flat), which is exactly the no-overshoot condition
  const m = [slope[0]];
  for (let i = 1; i < n - 1; i++) {
    m.push(slope[i - 1] * slope[i] <= 0 ? 0 : (slope[i - 1] + slope[i]) / 2);
  }
  m.push(slope[n - 2]);
  // Fritsch–Carlson: clamp the tangents to within three times the slope of each segment
  for (let i = 0; i < n - 1; i++) {
    if (slope[i] === 0) {
      m[i] = 0;
      m[i + 1] = 0;
      continue;
    }
    const a = m[i] / slope[i];
    const b = m[i + 1] / slope[i];
    const s = a * a + b * b;
    if (s > 9) {
      const t = (3 / Math.sqrt(s)) * slope[i];
      m[i] = t * a;
      m[i + 1] = t * b;
    }
  }
  let d = `M${pts[0].x.toFixed(1)},${pts[0].y.toFixed(1)}`;
  for (let i = 0; i < n - 1; i++) {
    const h = dx[i] / 3;
    d +=
      `C${(pts[i].x + h).toFixed(1)},${(pts[i].y + m[i] * h).toFixed(1)} ` +
      `${(pts[i + 1].x - h).toFixed(1)},${(pts[i + 1].y - m[i + 1] * h).toFixed(1)} ` +
      `${pts[i + 1].x.toFixed(1)},${pts[i + 1].y.toFixed(1)}`;
  }
  return d;
}

const pts = series.map((p, i) => ({ x: x(i), y: y(p.v) }));
const line = smoothPath(pts);
const area = `${line}L${x(series.length - 1).toFixed(1)},${(PAD.top + plotH).toFixed(1)}L${x(0).toFixed(1)},${(PAD.top + plotH).toFixed(1)}Z`;

/* The final point and its value. **Switch the text to right-aligned when it is up against the
   right edge**, otherwise a four-digit number runs off the canvas -- SVG will not clip it for
   you, it is simply gone */
const endX = x(series.length - 1);
const endY = y(maxV);
const endAnchor = endX > W - PAD.right - 40 ? "end" : "middle";

const xTickIdx = [...new Set(
  Array.from({ length: 6 }, (_, i) => Math.round((i * (series.length - 1)) / 5)),
)];

/* Two files out, one dark and one light, and the README picks by theme with `<picture>`.
 *
 * **A white line is invisible on the light theme** -- GitHub's light background is white itself.
 * Wanting white means it has to be two files: `prefers-color-scheme` written inside the SVG does
 * not count, because an SVG in a README is loaded as an image, and that media query asks the
 * operating system rather than GitHub's theme setting, so anyone whose two disagree sees a blank
 * chart. `<picture>` is the one that asks GitHub itself. */
const THEMES = {
  dark: { ink: "#8b949e", accent: "#ffffff", grid: "#8b949e33" },
  light: { ink: "#6e7781", accent: "#1f2328", grid: "#6e778133" },
};

function render({ ink, accent, grid }) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" font-family="-apple-system,BlinkMacSystemFont,Segoe UI,Helvetica,Arial,sans-serif">
<defs><linearGradient id="fill" x1="0" y1="0" x2="0" y2="1">
<stop offset="0%" stop-color="${accent}" stop-opacity="0.22"/>
<stop offset="100%" stop-color="${accent}" stop-opacity="0"/>
</linearGradient></defs>
<text x="${PAD.left}" y="24" fill="${ink}" font-size="13">${owner}/${repo}</text>
${ticks(maxV).map((v) => `<g><line x1="${PAD.left}" y1="${y(v).toFixed(1)}" x2="${W - PAD.right}" y2="${y(v).toFixed(1)}" stroke="${grid}"/><text x="${PAD.left - 10}" y="${(y(v) + 4).toFixed(1)}" fill="${ink}" font-size="11" text-anchor="end">${v.toLocaleString("en-US")}</text></g>`).join("")}
${xTickIdx.map((i) => `<text x="${x(i).toFixed(1)}" y="${H - 16}" fill="${ink}" font-size="11" text-anchor="middle">${fmtDate(series[i].t)}</text>`).join("")}
<path d="${area}" fill="url(#fill)"/>
<path d="${line}" fill="none" stroke="${accent}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
<circle cx="${endX.toFixed(1)}" cy="${endY.toFixed(1)}" r="3.5" fill="${accent}"/>
<text x="${endX.toFixed(1)}" y="${(endY - 12).toFixed(1)}" fill="${accent}" font-size="14" font-weight="600" text-anchor="${endAnchor}">${maxV.toLocaleString("en-US")}</text>
</svg>
`;
}

mkdirSync(dirname(out), { recursive: true });
const lightOut = out.replace(/\.svg$/, "-light.svg");
writeFileSync(out, render(THEMES.dark));
writeFileSync(lightOut, render(THEMES.light));
console.log(`${series.length} days, ${maxV} stars → ${out} + ${lightOut}`);
