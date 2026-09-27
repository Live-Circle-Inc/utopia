import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import Graphology from "graphology";
import { circular, circlepack } from "graphology-layout";
import forceAtlas2 from "graphology-layout-forceatlas2";
import FA2Layout from "graphology-layout-forceatlas2/worker";
import Sigma from "sigma";
import { createNodeBorderProgram } from "@sigma/node-border";
import EdgeCurveProgram from "@sigma/edge-curve";
import { NodeSquareShellProgram } from "./squareShellProgram";
import { EntityHistory } from "./EntityHistory";
import {
  ArrowLeft,
  ArrowRight,
  ChevronRight,
  CircleDashed,
  Grape,
  Loader2,
  Maximize2,
  Orbit,
  Pause,
  Pencil,
  Play,
  Waypoints,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import {
  api,
  type DerivedFact,
  type EntityFact,
  type Evidence,
  type GraphEdge,
  type GraphNode,
  type BlockedDerivation,
  type ProofStep,
} from "../api";
import { S } from "../i18n";
import { usePopoverFlip } from "../ui/popoverFlip";
import { useKb, useKbId } from "../kb";
import { toast } from "../toast";

/* Canvas palette -- the structure is taken from the Semantica GraphWorkspace source; the base
   colours have been neutralised: Semantica's original is steel-blue (#0B1320/#5A7A9E/#7A92AE),
   and per the established principle "zero colour cast in the chrome, colour belongs to the data
   only" they are swapped for pure greys of the same lightness; the type-colour mix ratios are
   unchanged */
const NODE_SHELL_BASE = "#121212"; // node shell dark base (neutralised #0B1320)
const NODE_CORE_BASE = "#767676"; // node core grey (neutralised #5A7A9E)
const NODE_BORDER_BASE = "#909090"; // node border (neutralised #7A92AE)
const NODE_TINT_MIX = 0.14; // type colour mixes into the shell at only 14% (the key to the refined look)
const NODE_CORE_MIX = 0.5; // how far the core mixes toward the type colour
/* The status ring takes **the node's own type colour**, not two hard-coded hues.

   The immediate reason for the change was a colour collision: the old selected gold ring
   `#E7C57C` is exactly `rgb(231,197,124)`, bit-for-bit identical to `EDGE_COLOR_DERIVED` --
   "this node is selected" and "this edge was derived" were speaking in the same colour, and the
   two have nothing to do with each other. Gold now belongs exclusively to "derived".

   Mixed toward white rather than used raw: the ring is drawn on the node itself, so at the same
   colour and the same lightness you cannot tell there is a ring at all.
   **Hover mixes whiter, selection mixes less** -- on hover nothing else is dimmed, so the ring
   has to jump straight out of a tangle of lines; on selection everything else is already dimmed
   and the node stands alone anyway, so what the ring should say then is "who it is", hence
   closer to its own colour. */
const RING_HOVER_MIX = 0.7; // hover: toward white, so that it jumps out
const RING_SELECT_MIX = 0.35; // selected: toward its own colour, so that it is recognisable
const EDGE_COLOR = "rgba(163,163,163,0.2)"; // pure grey (per user request, no steel blue)
// Relations the ontology never admitted: same colour, only fainter. The name comes from the
// source text, so it should not look as heavy as a relation out of the vocabulary
const EDGE_COLOR_INFERRED = "rgba(163,163,163,0.1)";
// Derived edges (R1). **This says something different from the two above**: those two say
// "where this edge's name came from", this one says "nobody stated this edge at all, the engine
// derived it". So it gets a hue of its own rather than one more step of faint grey -- the user
// has to tell "written in a document" from "derived" out of the corner of their eye
const EDGE_COLOR_DERIVED = "rgba(231,197,124,0.42)";
const EDGE_COLOR_DERIVED_DIM = "rgba(231,197,124,0.14)";
/* Contested (0017 §3): coral orange `--u-contest`. Gold is derived, amber is warning, pink is
   danger, so it has to pull away from all three. **The whole edge changes colour** -- a ring on
   the node with the edge still grey is indistinguishable in peripheral vision */
const EDGE_COLOR_CONTEST = "rgba(255,106,61,0.55)";
const EDGE_FOCUS_CONTEST = "rgba(255,106,61,1)";

/** The curvature difference between two adjacent arcs. Too small and they still smear
 *  together; too large and on a long edge the arc swings far away from the nodes */
const EDGE_CURVATURE_STEP = 0.18;

/** Which arc an edge is drawn on; `curvature === 0` = a straight line. */
interface PlacedEdge {
  edge: GraphEdge;
  curvature: number;
  /** The other phrasings folded into this edge (inverse relations), shown along with it on hover */
  alsoLabels: string[];
}

/** Edges between the same pair of nodes, each drawn on its own arc; the ones derived from
 *  inverse relations are folded away first.
 *
 *  **Two things, and the order matters: subtract first, then fan out.**
 *
 *  One. `A works_at B` and the `B employs A` derived from it are **two phrasings of the same
 *  thing**, not two pieces of knowledge. Drawing them as two arcs only makes the redundancy look
 *  prettier. So an edge derived from an inverse relation is folded into its source edge, and the
 *  phrasing is hung on that edge. `sub_property` (`ceo_of ⊑ works_at`) is not folded -- those are
 *  two facts at different granularities, each standing on its own.
 *
 *  Two. The rest are grouped by **undirected pair** and fanned out. Undirected is the point: an
 *  edge and its reverse have source and target swapped, so grouping by directed pair puts each
 *  in a group of its own, each thinking itself an only child, and they stack back onto the same
 *  straight line. Grouping uses min/max, and when landing on an arc the sign is flipped by the
 *  edge's own direction -- sigma's curvature is relative to source→target, and without the flip
 *  the reverse edge's arc would bend to the same side. */
function layOutParallelEdges(edges: GraphEdge[]): {
  edges: PlacedEdge[];
  folded: number;
} {
  const pairKey = (a: string, b: string) => (a < b ? `${a} ${b}` : `${b} ${a}`);
  const push = (m: Map<string, GraphEdge[]>, k: string, e: GraphEdge) => {
    const list = m.get(k);
    if (list) list.push(e);
    else m.set(k, [e]);
  };

  // ---- One. Fold away the edges derived from inverse relations
  const survivors: GraphEdge[] = [];
  const inverses: GraphEdge[] = [];
  for (const e of edges) {
    if (e.derived && e.rule === "inverse") inverses.push(e);
    else survivors.push(e);
  }
  /* **Find the source edge by premise, not by node pair.**
     This was once written as "take the first edge on that pair of nodes", and so `contains`
     ended up hung on an `allied_with` that happened to connect the same two points -- while
     `contains` belongs to `part_of`. Once it is hung on the wrong edge the UI looks completely
     normal, which is the hardest kind to spot. The premises are computed on the server; use
     them. */
  const onScreen = new Map<string, GraphEdge>();
  for (const e of survivors) onScreen.set(e.id, e);

  const also = new Map<string, string[]>();
  let folded = 0;
  for (const e of inverses) {
    const host = (e.premises ?? []).map((p) => onScreen.get(p)).find(Boolean);
    if (!host) {
      // The source edge is not on this screen (the timeline filtered it out, or it is itself
      // derived and got filtered away).
      // **Then keep it** -- folding into an edge that does not exist is the same as deleting
      // this piece of knowledge
      survivors.push(e);
      continue;
    }
    const list = also.get(host.id) ?? [];
    // Dedupe: several transitively derived `part_of` edges each have their own inverse, and
    // their premise chains all lead back to the same edge, so the same phrasing gets hung three
    // times over. **A phrasing is a name, not a count**
    const name = e.label ?? e.predicate ?? "";
    if (!list.includes(name)) list.push(name);
    also.set(host.id, list);
    folded++;
  }

  // ---- Two. Fan the rest out by undirected pair
  const groups = new Map<string, GraphEdge[]>();
  for (const e of survivors) push(groups, pairKey(e.source, e.target), e);

  const placed: PlacedEdge[] = [];
  for (const group of groups.values()) {
    const n = group.length;
    group.forEach((e, i) => {
      // Spread out symmetrically around the line: n=1 → [0]; n=2 → [-0.5, 0.5]; n=3 → [-1, 0, 1]
      const offset = n === 1 ? 0 : i - (n - 1) / 2;
      const sign = e.source < e.target ? 1 : -1;
      placed.push({
        edge: e,
        curvature: offset === 0 ? 0 : sign * offset * EDGE_CURVATURE_STEP,
        alsoLabels: also.get(e.id) ?? [],
      });
    });
  }
  return { edges: placed, folded };
}
// The breathing period. The animation is not there to look good -- a static colour difference
// simply goes unnoticed among several hundred edges
const DERIVED_PULSE_MS = 2200;
// Past this count, colour only, no animation. **Stated outright rather than degraded quietly**:
// recomputing the colour of thousands of edges every frame buys you a graph you cannot drag, and
// at that point what the user wants is to be able to drag it
const DERIVED_ANIMATE_MAX = 400;
// Fade duration for the toggle. **Slightly longer than FADE_MS(320)**: the playback fade is a
// batch of edges arriving one after another, this is a whole batch entering or leaving at once,
// and only at a slower pace can you see that "that batch of gold lines left together"
const DERIVED_TOGGLE_MS = 420;
/* Derived edges **arrive a little later than the facts**, then fade in as a whole.

   Tried animating the derivation itself: light up the premises in order, then light up the
   conclusion last. Two versions were built and neither was readable -- in the first the premises
   flashed and died, so by the time the conclusion appeared the premises were long dark; the
   second lit and released each whole group together, and it was still dozens of groups rising
   and falling all over the graph with no way to tell which belonged to which.
   **A graph of several hundred edges is not the place to tell a causal chain** -- the sidebar's
   Derived tab writes them out one by one, and far more legibly. All that is needed here is one
   thing: these edges came later, and they are not the same kind of thing as what somebody wrote
   down. Arriving a little later + a colour of their own already says it all. */
const DERIVE_SETTLE_MS = 500; // how long after the facts land before the derived ones follow
const DERIVE_FADE_MS = 620; // overall fade-in, slower than the toggle: an "entrance", not a "switch"
// How many chips the legend lays out at most; the rest go into "+N types". **This row is laid
// out horizontally, so with many types it wraps and pushes the canvas down**; and with a dozen
// identical chips in a row nobody can read which one matters. The collapsed ones can still be
// found from "+N"
const LEGEND_MAX = 6;
/* The selectable steps for how many nodes to draw. **Steps rather than a text field**: there is
   no such thing as "precise" for this number -- it only affects whether you can see clearly or
   can still drag, and what the user wants is "more/fewer", not the number 237.
   The maximum matches the backend's GRAPH_NODE_CAP_MAX; above that what breaks first is
   dragging, not clarity */
const NODE_BUDGETS: number[] = [150, 300, 600, 1000];
// Note: sigma's edge shader does not premultiply RGB under premultiplied blending
// (ONE, ONE_MINUS_SRC_ALPHA), so alpha cannot dim an edge -- the dimness has to be encoded into
// the RGB (an opaque near-background colour)
const EDGE_DIM = "#141414";
/* Ghost edges: derivations that never landed. The same hue mixed toward EDGE_DIM (under
   premultiplied blending alpha cannot dim an edge), and thinner. Sigma's default edge program
   cannot draw dashes, and it is not worth writing another one just for this */
const EDGE_GHOST = lerpColor("rgba(255,106,61,1)", EDGE_DIM, 0.55);
const EDGE_GHOST_FOCUS = lerpColor("rgba(255,106,61,1)", EDGE_DIM, 0.2);
const EDGE_FOCUS = "rgba(255,255,255,0.55)";
// Derived edges when selected/hovered. **They must not go white along with everything else**:
// selection is exactly the moment of closest inspection, and that is when "this edge was
// derived, nobody ever wrote it" needs saying more than at any other time.
// This used to be EDGE_FOCUS across the board, so selecting turned the gold lines white, which
// erased where they came from.
// Brighter and more solid than the normal gold -- it still has to express "selected"
const EDGE_FOCUS_DERIVED = "rgba(255,214,140,0.95)";
const MUTED_SHELL = "#151515";
/* How far everything else is dimmed on hover. **Lighter than selection** (selection dims to the
   floor): hover follows the mouse and changes every time you sweep across a node, so dimming to
   the floor would make the whole canvas flicker without end; and if the two dimmed equally hard,
   "I am just passing by" and "I selected it" would be the same picture.
   So a step is left between them: dimmed deeply, but not to the floor -- you can see the focus,
   and you can also see that this is only passing by.
   (Tried 0.55: too shallow, the focus did not stand out enough)*/
const HOVER_MUTE = 0.78;
const PILL_BG = "rgba(12,12,12,0.9)";
const PILL_BORDER = "rgba(255,255,255,0.14)";
const PILL_TEXT = "#ededed";
const TRANSPARENT = "rgba(0,0,0,0)";
const DAY_MS = 24 * 3600 * 1000;

function hexToRgb(hex: string): [number, number, number] {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex);
  if (!m) return [128, 128, 128];
  const v = parseInt(m[1], 16);
  return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
}

/** Mix c1 toward c2 by the ratio t */
function mix(c1: string, c2: string, t: number): string {
  const [r1, g1, b1] = hexToRgb(c1);
  const [r2, g2, b2] = hexToRgb(c2);
  const f = (a: number, b: number) => Math.round(a + (b - a) * t);
  return `rgb(${f(r1, r2)},${f(g1, g2)},${f(b1, b2)})`;
}

/* Playback fade: parse hex / rgb / rgba (alpha included) and interpolate linearly */
function parseRgba(c: string): [number, number, number, number] {
  if (c.startsWith("#")) {
    const [r, g, b] = hexToRgb(c);
    return [r, g, b, 1];
  }
  const m = c.match(
    /rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:[,\s/]+([\d.]+))?/,
  );
  if (!m) return [128, 128, 128, 1];
  return [+m[1], +m[2], +m[3], m[4] !== undefined ? +m[4] : 1];
}
function lerpColor(from: string, to: string, t: number): string {
  const a = parseRgba(from);
  const b = parseRgba(to);
  const f = (i: number) => a[i] + (b[i] - a[i]) * t;
  return `rgba(${Math.round(f(0))},${Math.round(f(1))},${Math.round(f(2))},${f(3).toFixed(3)})`;
}
/** Fade-in duration for elements that are new during playback */
const FADE_MS = 320;

/* World-coordinate grid: pans/zooms with the camera (the Figma/tldraw infinite-canvas
   convention). 4x subdivision LOD: each level's alpha fades in continuously with its on-screen
   spacing (entering at 13px → full 5.5% at 52px), and where the coarse and fine lines coincide
   they stack brighter of their own accord, forming a "major/minor cell" hierarchy; no jumps
   anywhere. */
const GRID_BASE_WORLD = 24; // base world cell spacing (matches the ~300-scale layout)
const GRID_FADE_IN_PX = 13;
const GRID_FULL_PX = 52;
const GRID_MAX_LEVEL_PX = 480;
const GRID_MAX_ALPHA = 0.055;

function drawWorldGrid(canvas: HTMLCanvasElement, sigma: Sigma): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const { width, height } = sigma.getDimensions();
  const dpr = window.devicePixelRatio || 1;
  const pw = Math.round(width * dpr);
  const ph = Math.round(height * dpr);
  if (canvas.width !== pw || canvas.height !== ph) {
    canvas.width = pw;
    canvas.height = ph;
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);
  if (width <= 0 || height <= 0) return;

  // World→screen: two probe points give the pixels per world unit and the origin's position
  // (no camera rotation in this scene)
  const p0 = sigma.graphToViewport({ x: 0, y: 0 });
  const p1 = sigma.graphToViewport({ x: 1, y: 0 });
  const ppw = p1.x - p0.x;
  if (!Number.isFinite(ppw) || ppw <= 0) return;

  // Finest visible level: the smallest power-of-4 spacing whose screen gap ≥ the fade-in threshold
  let spacing = GRID_BASE_WORLD;
  while (spacing * ppw < GRID_FADE_IN_PX) spacing *= 4;
  while (spacing * ppw >= GRID_FADE_IN_PX * 4) spacing /= 4;

  for (let sp = spacing; sp * ppw < GRID_MAX_LEVEL_PX; sp *= 4) {
    const ss = sp * ppw;
    const t = Math.min(
      1,
      (ss - GRID_FADE_IN_PX) / (GRID_FULL_PX - GRID_FADE_IN_PX),
    );
    if (t <= 0) continue;
    ctx.strokeStyle = `rgba(255,255,255,${(GRID_MAX_ALPHA * t).toFixed(4)})`;
    ctx.lineWidth = 1;
    ctx.beginPath();
    const startX = ((p0.x % ss) + ss) % ss;
    for (let x = startX; x <= width; x += ss) {
      const px = Math.round(x) + 0.5;
      ctx.moveTo(px, 0);
      ctx.lineTo(px, height);
    }
    const startY = ((p0.y % ss) + ss) % ss;
    for (let y = startY; y <= height; y += ss) {
      const py = Math.round(y) + 0.5;
      ctx.moveTo(0, py);
      ctx.lineTo(width, py);
    }
    ctx.stroke();
  }
}

/* Chip label: dark rounded backing + soft text (borrowing Semantica's floating-tag style) */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function drawPillLabel(
  ctx: CanvasRenderingContext2D,
  data: any,
  settings: any,
): void {
  if (!data.label) return;
  // On hover the hover card (drawHoverCard) takes over the display and the underlying pill is
  // hidden, to avoid two layers of label
  if (data.hideBaseLabel) return;
  // Semantica chip: fontSize=clamp(10, size*0.25, 11), pad 6/3, radius 6, above the node,
  // shadow blur 12
  const size = Math.max(10, Math.min(11, data.size * 0.25));
  ctx.font = `500 ${size}px Geist, Inter, "Noto Sans SC", sans-serif`;
  ctx.textBaseline = "middle";
  const padX = 6;
  const padY = 3;
  const w = ctx.measureText(data.label).width + padX * 2;
  const h = size + padY * 2;
  const x = data.x + Math.max(data.size * 0.7, 12);
  const y = data.y - Math.max(data.size * 0.9, 10) - h;
  ctx.save();
  ctx.shadowColor = "rgba(0,0,0,0.6)";
  ctx.shadowBlur = 12;
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, 6);
  ctx.fillStyle = PILL_BG;
  ctx.fill();
  ctx.shadowBlur = 0;
  ctx.strokeStyle = PILL_BORDER;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.fillStyle = PILL_TEXT;
  ctx.fillText(data.label, x + padX, y + h / 2);
  ctx.restore();
}

/* Hover card (the Semantica hoverCard spec): radial glow + name + type line */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function drawHoverCard(
  ctx: CanvasRenderingContext2D,
  data: any,
  _settings: any,
): void {
  if (!data.label) return;
  ctx.save();

  // Glow: radius max(size*4.8, 16), type colour alpha 0.18 → 0
  const glowR = Math.max(data.size * 4.8, 16);
  const [r, g, b] = hexToRgb((data.typeColor as string) ?? "#888888");
  const grad = ctx.createRadialGradient(
    data.x,
    data.y,
    0,
    data.x,
    data.y,
    glowR,
  );
  grad.addColorStop(0, `rgba(${r},${g},${b},0.18)`);
  grad.addColorStop(1, `rgba(${r},${g},${b},0)`);
  ctx.fillStyle = grad;
  ctx.beginPath();
  ctx.arc(data.x, data.y, glowR, 0, Math.PI * 2);
  ctx.fill();

  // Card: title 700/13 + type line 500/10 uppercase
  const titleSize = 13;
  const metaSize = 10;
  const padX = 10;
  const padY = 7;
  const metaGap = 5;
  const meta = String(data.typeLabel ?? "NODE").toUpperCase();
  ctx.textBaseline = "top";
  ctx.font = `700 ${titleSize}px Geist, Inter, "Noto Sans SC", sans-serif`;
  const titleW = ctx.measureText(data.label).width;
  ctx.font = `500 ${metaSize}px Geist, Inter, sans-serif`;
  const metaW = ctx.measureText(meta).width;
  const w = Math.max(titleW, metaW) + padX * 2;
  const h = padY * 2 + titleSize + metaGap + metaSize;
  const x = data.x + Math.max(data.size * 0.9, 16);
  const y = data.y - Math.max(data.size * 1.1, 16) - h;

  ctx.shadowColor = "rgba(0,0,0,0.62)";
  ctx.shadowBlur = 15;
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, 8);
  ctx.fillStyle = "rgba(12,12,12,0.94)";
  ctx.fill();
  ctx.shadowBlur = 0;
  ctx.strokeStyle = "rgba(255,255,255,0.16)";
  ctx.lineWidth = 1;
  ctx.stroke();

  ctx.fillStyle = "#f5f5f5";
  ctx.font = `700 ${titleSize}px Geist, Inter, "Noto Sans SC", sans-serif`;
  ctx.fillText(data.label, x + padX, y + padY);
  ctx.fillStyle = "rgba(255,255,255,0.5)";
  ctx.font = `500 ${metaSize}px Geist, Inter, sans-serif`;
  ctx.fillText(meta, x + padX, y + padY + titleSize + metaGap);
  ctx.restore();
}

export function Graph() {
  const kbId = useKbId();
  const { kb } = useKb();
  /* The address bar and the view stay in sync **both ways**.
     There used to be only the "in" half: `?entity=` was read once on mount and then ignored --
     a link somebody else gave you worked, but what you were looking at yourself could not be
     shared, because the address bar stayed parked on a bare /graph. */
  const search = useSearch({ from: "/app/kb/$kbId/graph" });
  const navigate = useNavigate();
  const entityParam = search.entity;
  const [focusEntity, setFocusEntity] = useState<string | null>(
    search.focus ?? entityParam ?? null,
  );
  const [selected, setSelected] = useState<string | null>(entityParam ?? null);
  const [searchInput, setSearchInput] = useState("");
  const [searchQ, setSearchQ] = useState("");
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  // Whether derived edges are shown. Shown by default -- inference is off by default, so having
  // any derivations at all means the user turned the switch on at some point
  const [showDerived, setShowDerived] = useState(true);
  // The info window is collapsed by default: it answers "when was this derived", and that is a
  // question only asked occasionally
  /* Inference expands in place too, the same mechanism as "+N types", the notifications and the
     user menu. **Anchored to the bottom left corner**: the tower sits at the canvas's bottom
     left, so the panel has to grow up and to the right out of that ⋯ button */
  const derivedPop = usePopoverFlip<HTMLButtonElement, HTMLDivElement>(
    "bottom left",
  );
  /* "+N types" uses the same in-place expansion as the notification/user cards: the panel
     collapses onto the chip's real bounds (999px radius) and then grows into a card.
     **It hugs the left edge, so the anchor corner is top left** */
  const legendPop = usePopoverFlip<HTMLButtonElement, HTMLDivElement>(
    "top left",
  );
  const [legendQ, setLegendQ] = useState("");
  /* The entity currently leaving. **The panel must not unmount the moment selection is
     cleared** -- that way it simply vanishes. It stays where it is until the exit animation has
     played out, and only then is it really removed. The current value is read through
     selectedRef rather than by writing setState as an updater with side effects: that form runs
     twice under StrictMode */
  const [exiting, setExiting] = useState<string | null>(null);
  // Which tab to land on and which row to expand when the panel opens -- used when arriving by
  // clicking a ghost edge
  const panelIntentRef = useRef<{ view: "derived"; open: string } | null>(null);
  const deselect = useCallback(() => {
    const cur = selectedRef.current;
    if (!cur) return;
    setExiting(cur);
    setSelected(null);
    window.setTimeout(() => setExiting(null), 170);
  }, []);
  /** null = all time; a number = the as-of instant (ms).
      As-of today by default: a temporal platform's graph presents "the world as it is now" by
      default, and closed facts should not sit indistinguishably alongside current ones
      (All time is an explicit choice) */
  /* The timeline. If the URL carries one, use it: `all` = all time, otherwise parsed as
     YYYY-MM-DD (matching the data's day-level precision, and easier to read than a run of
     milliseconds) */
  const [timeT, setTimeT] = useState<number | null>(() => {
    if (search.at === "all") return null;
    if (search.at) {
      const t = Date.parse(search.at);
      if (!Number.isNaN(t)) return t;
    }
    return Date.now();
  });
  const [activeCount, setActiveCount] = useState(0);
  const [stabilizing, setStabilizing] = useState(false);
  /* Playback state is lifted to this level: the reducer has to distinguish "playback advancing"
     (fade in) from "manual dragging" (instant switch) */
  const [playing, setPlaying] = useState(false);

  /* View → address bar. **replace, not push**: clicking a node is browsing, not navigating, and
     piling it into history would turn "back" into undoing clicks one at a time.
     Skipped wholesale during playback -- writing the URL once per frame is a disaster */
  useEffect(() => {
    if (playing) return;
    const at =
      timeT === null
        ? "all"
        : // If it is parked at "now", write nothing. Otherwise every visit drags a run of
          // today's date through the address bar, and that was the default value anyway
          Math.abs(timeT - Date.now()) < DAY_MS
          ? undefined
          : new Date(timeT).toISOString().slice(0, 10);
    const next = {
      entity: selected ?? undefined,
      // **Omitted when it is identical to entity**: clicking a search result sets both of them,
      // and written out literally the address bar shows the same UUID twice over.
      // It only carries information when "focused on A's neighbourhood but B is selected"
      focus:
        focusEntity && focusEntity !== selected ? focusEntity : undefined,
      at,
    };
    if (
      next.entity === search.entity &&
      next.focus === search.focus &&
      next.at === search.at
    )
      return;
    navigate({
      to: "/kb/$kbId/graph",
      params: { kbId },
      search: next,
      replace: true,
    });
  }, [
    selected,
    focusEntity,
    timeT,
    playing,
    search.entity,
    search.focus,
    search.at,
    navigate,
  ]);

  /* Address bar → view. **This half is what back/forward run on**: without it the browser's
     back button changes the address but not the view, which looks like back is broken. Both
     directions compare before acting, so they do not fight each other */
  useEffect(() => {
    const e = search.entity ?? null;
    const f = search.focus ?? null;
    setSelected((cur) => (cur === e ? cur : e));
    setFocusEntity((cur) => (cur === f ? cur : f));
  }, [search.entity, search.focus]);
  /* Layout modes: force = FA2 repulsion; circular = ring; pack = circle-packed clusters by type */
  type LayoutMode = "force" | "circular" | "pack";
  const [layoutMode, setLayoutMode] = useState<LayoutMode>("force");
  const layoutModeRef = useRef<LayoutMode>("force");
  const layoutCtlRef = useRef<{ apply: (m: LayoutMode) => void } | null>(null);

  /* How many to draw. **It goes into the queryKey** -- without that, changing the step does not
     refetch, and the UI looks changed while the data underneath is still the old data */
  const [nodeBudget, setNodeBudget] = useState<number>(NODE_BUDGETS[0]);

  const data = useQuery({
    queryKey: ["graph", kb?.id, focusEntity, nodeBudget],
    queryFn: () =>
      focusEntity
        ? api.graphNeighborhood(kb!.id, focusEntity)
        : api.graphOverview(kb!.id, nodeBudget),
    enabled: !!kb,
  });

  // Overview mode searches entities across the whole KB; subgraph mode filters client-side
  // within the already-loaded subgraph
  const inSubgraph = !!focusEntity;
  // The cap on how many hits come back. **"Load more" rather than pagination**: this is a
  // suggestion dropdown, the user is hunting for one specific entity, and paging would make them
  // lose the few rows they just scanned
  const [searchLimit, setSearchLimit] = useState(10);
  useEffect(() => setSearchLimit(10), [searchQ]);
  const candidates = useQuery({
    queryKey: ["entitySearch", kb?.id, searchQ, searchLimit],
    queryFn: () => api.searchEntities(kb!.id, searchQ, searchLimit),
    enabled: !!kb && searchQ.length > 0 && !inSubgraph,
    placeholderData: (prev) => prev,
  });
  const subgraphHits = useMemo(() => {
    if (!inSubgraph || !searchQ || !data.data) return [];
    const q = searchQ.toLowerCase();
    return data.data.nodes
      .filter(
        (n) =>
          n.name.toLowerCase().includes(q) ||
          n.disambiguator?.toLowerCase().includes(q),
      )
      .slice(0, 10);
  }, [inSubgraph, searchQ, data.data]);
  const searchHits = inSubgraph
    ? subgraphHits
    : (candidates.data?.entities ?? []);

  const containerRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLCanvasElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  /* Focus = hover takes precedence over selection; styling is handled uniformly in the reducer */
  const selectedRef = useRef<string | null>(null);
  const hoverRef = useRef<string | null>(null);
  /** Which edge the mouse is resting on. Used to surface the inverse phrasings folded into it */
  const hoverEdgeRef = useRef<string | null>(null);
  const filterRef = useRef<{
    hiddenTypes: Set<string>;
    activeNodes: Set<string> | null;
    activeEdges: Set<string> | null;
    /** Whether derived edges are shown. **Shown by default** -- inference is off by default, so
     *  having derived edges at all means the user turned the switch on themselves; but it has to
     *  be hideable in one click, to see what "the graph of only what people said" looks like */
    showDerived: boolean;
  }>({
    hiddenTypes: new Set(),
    activeNodes: null,
    activeEdges: null,
    showDerived: true,
  });
  const playingRef = useRef(false);
  /* Playback fade table: the ids of nodes/edges newly activated this round → the instant of
     activation (an rAF loop drives them to completion) */
  const fadeRef = useRef<Map<string, number>>(new Map());
  const fadeRafRef = useRef(0);

  const kickFade = useCallback(() => {
    if (fadeRafRef.current) return;
    const step = () => {
      const now = performance.now();
      for (const [id, start] of fadeRef.current)
        if (now - start >= FADE_MS) fadeRef.current.delete(id);
      sigmaRef.current?.refresh();
      fadeRafRef.current = fadeRef.current.size
        ? requestAnimationFrame(step)
        : 0;
    };
    fadeRafRef.current = requestAnimationFrame(step);
  }, []);

  useEffect(() => {
    playingRef.current = playing;
    if (!playing) {
      // Playback stopped: unfinished fades snap straight to completion
      fadeRef.current.clear();
      sigmaRef.current?.refresh();
    }
  }, [playing]);

  useEffect(() => () => cancelAnimationFrame(fadeRafRef.current), []);

  const types = useMemo(() => {
    const map = new Map<
      string,
      { label: string; color: string; shape: string; count: number }
    >();
    for (const n of data.data?.nodes ?? []) {
      // Nodes whose type was never determined go under the empty key (0009). Real keys are
      // derived from IRIs and can never be empty, so it cannot collide with any type; the label
      // goes through i18n -- do not draw a null onto the legend
      const key = n.type_key ?? "";
      const cur = map.get(key);
      if (cur) cur.count++;
      else
        map.set(key, {
          label: n.type_label ?? S.graph.untyped,
          color: n.color,
          shape: n.shape,
          count: 1,
        });
    }
    // **Sorted by occurrence count, not by the order they were met in**. The legend only fits a
    // few, and those slots should go to the types that dominate the picture; it used to be node
    // arrival order, which is effectively random. Ties break on the label -- otherwise the same
    // data jitters into a different order on every refresh
    return [...map.entries()].sort(
      (a, b) => b[1].count - a[1].count || a[1].label.localeCompare(b[1].label),
    );
  }, [data.data]);

  /* The ones that fit / the ones collapsed away. The collapsed ones can still be found and
     toggled from "+N" */
  const legendShown = types.slice(0, LEGEND_MAX);
  const legendRest = types.slice(LEGEND_MAX);
  // Whether any of the collapsed types is currently hidden. **Without this marker it is silent
  // filtering** -- turn a type off inside the panel, collapse the panel, and nothing in the UI
  // says any longer that it is off
  const hiddenInRest = legendRest.filter(([k]) => hiddenTypes.has(k)).length;

  // How many derived edges there are. **At zero the switch does not appear at all** -- a KB
  // with inference off should not be shown a button that never toggles anything visible
  const derivedCount = useMemo(
    () => (data.data?.edges ?? []).filter((e) => e.derived).length,
    [data.data],
  );

  /* Time filtering: compute the set of active edges/nodes at the instant T */
  const recomputeActive = useCallback(
    (t: number | null) => {
      const d = data.data;
      if (!d) return;
      if (t === null) {
        filterRef.current.activeNodes = null;
        filterRef.current.activeEdges = null;
        setActiveCount(d.edges.length);
      } else {
        const prevNodes = filterRef.current.activeNodes;
        const prevEdges = filterRef.current.activeEdges;
        const edges = new Set<string>();
        const nodes = new Set<string>();
        const touched = new Set<string>();
        for (const e of d.edges) {
          const vf = e.valid_from ? Date.parse(e.valid_from) : null;
          const vt = e.valid_to ? Date.parse(e.valid_to) : null;
          touched.add(e.source);
          touched.add(e.target);
          const active =
            vf === null ? true : vf <= t && (vt === null || vt > t);
          if (active) {
            edges.add(e.id);
            nodes.add(e.source);
            nodes.add(e.target);
          }
        }
        // Isolated nodes with no edges at all stay visible
        for (const n of d.nodes) if (!touched.has(n.id)) nodes.add(n.id);
        // Elements newly appearing as playback advances fade in; manual dragging keeps the
        // instant switch
        if (playingRef.current) {
          const now = performance.now();
          for (const id of edges)
            if (prevEdges && !prevEdges.has(id)) fadeRef.current.set(id, now);
          for (const id of nodes)
            if (prevNodes && !prevNodes.has(id)) fadeRef.current.set(id, now);
          if (fadeRef.current.size) kickFade();
        }
        filterRef.current.activeNodes = nodes;
        filterRef.current.activeEdges = edges;
        setActiveCount(edges.size);
      }
      sigmaRef.current?.refresh();
    },
    [data.data, kickFade],
  );

  useEffect(() => {
    filterRef.current.hiddenTypes = hiddenTypes;
    filterRef.current.showDerived = showDerived;
    sigmaRef.current?.refresh();
  }, [hiddenTypes, showDerived]);

  const deriveRafRef = useRef(0);
  /* Derived edges do not appear until the entrance has played. **The switch is "should they be
     shown", this is "has the entrance played yet"** -- two different things, and merging them
     means one entrance is skipped when you switch off and on again */
  const [derivedRevealed, setDerivedRevealed] = useState(false);
  /* The reducer is a closure that runs every frame, and reading state there reads a stale
     value -- it only understands refs */
  const derivedRevealedRef = useRef(false);
  useEffect(() => {
    derivedRevealedRef.current = derivedRevealed;
    sigmaRef.current?.refresh();
  }, [derivedRevealed]);

  const revealDerived = useCallback(() => {
    setDerivedRevealed(true);
    // Reuse the toggle's fade: direction "on", brightening from the near-background colour up to
    // normal
    derivedToggleRef.current = { at: performance.now(), on: true };
    const step = () => {
      const tr = derivedToggleRef.current;
      const done = !tr || performance.now() - tr.at >= DERIVE_FADE_MS;
      if (done) derivedToggleRef.current = null;
      sigmaRef.current?.refresh();
      deriveRafRef.current = done ? 0 : requestAnimationFrame(step);
    };
    cancelAnimationFrame(deriveRafRef.current);
    deriveRafRef.current = requestAnimationFrame(step);
  }, []);

  useEffect(() => () => cancelAnimationFrame(deriveRafRef.current), []);

  /* The toggle's fade: { start instant, which direction }; null = no transition in flight */
  const derivedToggleRef = useRef<{ at: number; on: boolean } | null>(null);
  const derivedRafRef = useRef(0);
  /* The previous value of the switch. **It is the only thing that can tell "did it really
     toggle"** -- the effect's deps also contain derivedCount, and "Run now derives new edges"
     changes the count without touching the switch; fading once for every effect firing would be
     an animation nobody asked for */
  const prevShowDerived = useRef(showDerived);

  // Toggling runs a transition rather than vanishing outright. **It has to drive the repaint
  // itself** -- once switched off the breathing timer below stops running, nobody pushes sigma to
  // redraw, and the fade-out sticks on its first frame
  useEffect(() => {
    const changed = prevShowDerived.current !== showDerived;
    prevShowDerived.current = showDerived;
    // Neither the first mount nor "only the count changed" is a toggle:
    // on entering the page, and when inference finishes and refreshes the count, you should not
    // see some inexplicable fade
    if (!changed) return;
    // No fade when there are too many of them: the same line as the breathing -- recomputing the
    // colour of thousands of edges every frame buys you stutter.
    // **Stated outright rather than degraded quietly**
    if (derivedCount > DERIVED_ANIMATE_MAX) return;

    const now = performance.now();
    const prev = derivedToggleRef.current;
    // Reversing midway (the user clicks twice in a row): carry on from the current progress
    // rather than starting over -- otherwise you see a jump in brightness
    const at =
      prev && prev.on !== showDerived
        ? now - Math.max(0, DERIVED_TOGGLE_MS - (now - prev.at))
        : now;
    derivedToggleRef.current = { at, on: showDerived };

    const step = () => {
      const tr = derivedToggleRef.current;
      const done = !tr || performance.now() - tr.at >= DERIVED_TOGGLE_MS;
      if (done) derivedToggleRef.current = null;
      sigmaRef.current?.refresh();
      derivedRafRef.current = done ? 0 : requestAnimationFrame(step);
    };
    cancelAnimationFrame(derivedRafRef.current);
    derivedRafRef.current = requestAnimationFrame(step);
    // **No cleanup is hung here**: cleanup would also run whenever a dep changes, and
    // derivedCount is one of the deps -- if inference happens to finish in the middle of these
    // 420ms the animation gets pinched off halfway (the picture stops at half brightness and
    // only settles on the next arbitrary repaint). The loop terminates on its own; cancelling
    // should only happen on unmount
  }, [showDerived, derivedCount]);

  // On unmount, collect the frame that may still be in flight
  useEffect(() => () => cancelAnimationFrame(derivedRafRef.current), []);

  /* When the entrance happens. Two entry points share one delay: entering the page, and turning
     the switch on by hand. **It does not wait for the layout to converge** -- converging takes
     2.5 seconds, and by the time it is done the person is long since looking elsewhere.

     **The order is itself content**: what settles first is the edges people wrote down, and only
     then do the derived ones get their turn. Appearing together and you cannot tell which came
     first */
  useEffect(() => {
    if (!showDerived || !data.data) {
      if (!showDerived) setDerivedRevealed(false);
      return;
    }
    if (derivedRevealed) return;
    const t = window.setTimeout(revealDerived, DERIVE_SETTLE_MS);
    return () => window.clearTimeout(t);
  }, [showDerived, data.data, derivedRevealed, revealDerived]);

  // The derived edges' breathing. **It only runs when there are derived edges, they are being
  // shown, and there are not too many of them** -- a KB with inference off should not repaint
  // every two seconds for this
  useEffect(() => {
    const n = derivedCount;
    if (!showDerived || n === 0 || n > DERIVED_ANIMATE_MAX) return;
    // Matching sigma's repaint rate is enough, no need for every frame: breathing is slow
    // motion, and at 30 fps you cannot see the difference
    const timer = setInterval(() => sigmaRef.current?.refresh(), 1000 / 30);
    return () => clearInterval(timer);
  }, [showDerived, derivedCount]);

  useEffect(() => {
    selectedRef.current = selected;
    sigmaRef.current?.refresh();
  }, [selected]);

  useEffect(() => {
    recomputeActive(timeT);
  }, [timeT, recomputeActive]);

  useEffect(() => {
    if (!containerRef.current || !data.data) return;
    const g = new Graphology({ multi: true });
    for (const n of data.data.nodes) {
      if (!g.hasNode(n.id)) {
        g.addNode(n.id, {
          label: n.name,
          // The Semantica recipe: dark shell + 14% type tint, core at 50% tint, steel-grey
          // border with a slight tint
          color: mix(NODE_CORE_BASE, n.color, NODE_CORE_MIX),
          shellColor: mix(NODE_SHELL_BASE, n.color, NODE_TINT_MIX),
          borderColor: mix(NODE_BORDER_BASE, n.color, 0.3),
          ringColor: TRANSPARENT,
          typeColor: n.color,
          typeLabel: n.type_label ?? S.graph.untyped,
          typeKey: n.type_key ?? "",
          type: n.shape === "square" ? "square" : "shell",
          size: 5 + Math.min(8, Math.sqrt(Number(n.degree)) * 1.6),
        });
      }
    }
    const placed = layOutParallelEdges(
      data.data.edges.filter((e) => g.hasNode(e.source) && g.hasNode(e.target)),
    );
    for (const { edge: e, curvature, alsoLabels } of placed.edges) {
      g.addEdgeWithKey(e.id, e.source, e.target, {
        // Contested edge labels are prefixed with ⚠: on top of the colour, a mark that does not
        // rely on colour vision
        label: (e.contested ? "⚠ " : "") + (e.label?.toUpperCase() ?? ""),
        size: e.blocked ? 0.7 : 1,
        color: e.blocked
          ? EDGE_GHOST
          : e.contested
            ? EDGE_COLOR_CONTEST
            : e.derived
              ? EDGE_COLOR_DERIVED
              : e.inferred
                ? EDGE_COLOR_INFERRED
                : EDGE_COLOR,
        contested: e.contested,
        blocked: e.blocked,
        // A lone edge goes straight: curves exist to pull overlaps apart, and with no overlap
        // there is nothing to bend
        type: curvature === 0 ? "line" : "curved",
        curvature,
        // The inverse phrasings folded in, shown together with its own name on hover
        alsoLabels,
        // The reducer reads this every frame: whether to hide it, whether to breathe
        derived: e.derived,
      });
    }
    // Layout: lay it out statically first, then let the worker animate to stability over ~2.5s
    // (Semantica-style stabilizing)
    let fa2: InstanceType<typeof FA2Layout> | null = null;
    let stabilizeTimer: ReturnType<typeof setTimeout> | null = null;
    // The drag state is declared ahead of fa2: the outputReducer closure references it
    let dragged: string | null = null;
    let dragPos: { x: number; y: number } | null = null;
    let fa2Settings: ReturnType<typeof forceAtlas2.inferSettings> | null = null;
    if (g.order > 0) {
      circular.assign(g, { scale: 300 });
      /* Tried scaling with graph size (gravity 0.12-0.22 / scalingRatio 11-16 + heavier damping),
         and one look at a real graph killed it: it did spread out, but that tension of "being
         pushed apart" was gone and the whole graph looked limp. **This set is the existing one
         and is deliberately on the large side** -- what we want is the feeling of nodes pressing
         against each other, not the least-effort arrangement */
      const settings = {
        ...forceAtlas2.inferSettings(g),
        gravity: 0.35,
        scalingRatio: 22,
        outboundAttractionDistribution: true,
      };
      fa2Settings = settings;
      forceAtlas2.assign(g, { iterations: 60, settings });
      fa2 = new FA2Layout(g, {
        settings,
        // The key: on write-back, pin the dragged node back onto the cursor (no flicker); and
        // once an outputReducer is supplied the supervisor calls readGraphPositions every frame
        // -- the cursor position keeps feeding into the force simulation
        outputReducer: (node, attr) => {
          if (dragged && node === dragged && dragPos) {
            attr.x = dragPos.x;
            attr.y = dragPos.y;
          }
          return attr;
        },
      });
      fa2.start();
      setStabilizing(true);
      stabilizeTimer = setTimeout(() => {
        fa2?.stop();
        setStabilizing(false);
      }, 2500);
    }

    // After a data rebuild the layout returns to force (the world grows out again)
    setLayoutMode("force");
    layoutModeRef.current = "force";

    // Any layout's result is rescaled to a world of the same magnitude as FA2's (±target), so a
    // camera reset feels the same either way
    const rescaleWorld = (target = 300) => {
      let minX = Infinity,
        maxX = -Infinity,
        minY = Infinity,
        maxY = -Infinity;
      g.forEachNode((_n, a) => {
        minX = Math.min(minX, a.x as number);
        maxX = Math.max(maxX, a.x as number);
        minY = Math.min(minY, a.y as number);
        maxY = Math.max(maxY, a.y as number);
      });
      const span = Math.max(maxX - minX, maxY - minY) || 1;
      const k = (target * 2) / span;
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      g.updateEachNodeAttributes((_n, a) => ({
        ...a,
        x: (a.x - cx) * k,
        y: (a.y - cy) * k,
      }));
    };

    // Layout switching control (hung on a ref for the component layer's buttons to call; inside
    // the closure it holds g / fa2 directly)
    layoutCtlRef.current = {
      apply: (mode) => {
        if (g.order === 0) return;
        if (stabilizeTimer) clearTimeout(stabilizeTimer);
        fa2?.stop();
        setStabilizing(false);
        if (mode === "force") {
          forceAtlas2.assign(g, {
            iterations: 60,
            settings: fa2Settings ?? undefined,
          });
          fa2?.start();
          setStabilizing(true);
          stabilizeTimer = setTimeout(() => {
            fa2?.stop();
            setStabilizing(false);
          }, 2500);
        } else if (mode === "circular") {
          circular.assign(g, { scale: 300 });
        } else {
          // Cluster by entity type: nodes of the same type crowd into the same circle
          circlepack.assign(g, { hierarchyAttributes: ["typeKey"] });
          rescaleWorld(300);
        }
        sigma.setCustomBBox(null);
        sigma.refresh();
        sigma.getCamera().animatedReset({ duration: 300 });
      },
    };

    sigmaRef.current?.kill();
    const sigma = new Sigma(g, containerRef.current, {
      allowInvalidContainer: true,
      defaultNodeType: "shell",
      nodeProgramClasses: {
        // Semantica node anatomy: status ring → border → dark shell → faintly coloured core
        shell: createNodeBorderProgram({
          borders: [
            { size: { value: 0.1 }, color: { attribute: "ringColor" } },
            { size: { value: 0.07 }, color: { attribute: "borderColor" } },
            { size: { value: 0.3 }, color: { attribute: "shellColor" } },
            { size: { fill: true }, color: { attribute: "color" } },
          ],
        }),
        square: NodeSquareShellProgram,
      },
      renderEdgeLabels: true,
      defaultEdgeType: "line",
      /* Parallel edges are fanned into arcs (see `layOutParallelEdges`). The straight-line
         version drew every edge between the same pair of nodes onto the same segment, so several
         labels stacked character over character into gibberish -- measured at up to six edges
         piled between one pair of nodes */
      edgeProgramClasses: { curved: EdgeCurveProgram },
      // Edge hover events are off by default. This turns them on for `enterEdge`: the phrasings
      // folded in need somewhere they can be seen (see edgeReducer)
      enableEdgeEvents: true,
      labelFont: '"Geist", "Inter", "Noto Sans SC", sans-serif',
      labelSize: 11,
      labelColor: { color: "#e5e5e5" },
      labelRenderedSizeThreshold: 6,
      labelDensity: 0.7,
      labelGridCellSize: 140,
      minCameraRatio: 0.04,
      maxCameraRatio: 8,
      edgeLabelSize: 9,
      edgeLabelColor: { color: "#a1a1a1" },
      edgeLabelFont: '"Geist", "Inter", sans-serif',
      defaultDrawNodeLabel: drawPillLabel,
      defaultDrawNodeHover: drawHoverCard,
      nodeReducer: (node, attrs) => {
        const f = filterRef.current;
        const res = { ...attrs };
        const base = attrs.size as number;
        // The status ring takes the node's own type colour (see the reasoning at RING_*_MIX)
        const ownColor = (attrs.typeColor as string) ?? NODE_CORE_BASE;
        if (f.hiddenTypes.has(attrs.typeKey as string)) {
          res.hidden = true;
          return res;
        }
        // Semantica state table: muted { ×0.52, every layer dimmed }
        const muteNode = () => {
          res.size = base * 0.52;
          res.color = mix(MUTED_SHELL, NODE_CORE_BASE, 0.3);
          res.shellColor = MUTED_SHELL;
          res.borderColor = TRANSPARENT;
          res.ringColor = TRANSPARENT;
          res.label = "";
          res.zIndex = 0;
        };
        /* On hover everything else is dimmed one step by HOVER_MUTE (selection dims to the
           floor). The neighbours are not dimmed -- what hover has to answer is "what is it
           connected to", and dimming the neighbours too is the same as not answering */
        const softMute = () => {
          res.size = base * (1 - 0.48 * HOVER_MUTE);
          res.color = lerpColor(
            String(attrs.color ?? NODE_CORE_BASE),
            mix(MUTED_SHELL, NODE_CORE_BASE, 0.3),
            HOVER_MUTE,
          );
          res.shellColor = lerpColor(
            String(attrs.shellColor ?? NODE_SHELL_BASE),
            MUTED_SHELL,
            HOVER_MUTE,
          );
          res.borderColor = TRANSPARENT;
          res.ringColor = TRANSPARENT;
          res.label = "";
          res.zIndex = 0;
        };
        if (hoverRef.current === node) {
          res.size = Math.max(base * 1.08, 10.4);
          res.ringColor = mix(ownColor, "#ffffff", RING_HOVER_MIX);
          // The hover card takes over label display; label itself is kept (the hover card uses
          // it to render the title)
          res.hideBaseLabel = true;
          res.zIndex = 4;
          return res;
        }
        const hov = hoverRef.current;
        // The selected entity may not be on the current canvas (sidebar navigation / the gap
        // during a neighbourhood reload) -- if it is not, skip the focus-dimming logic
        const sel =
          selectedRef.current && g.hasNode(selectedRef.current)
            ? selectedRef.current
            : null;
        if (sel) {
          if (node === sel) {
            res.size = Math.max(base * 1.02, 9.2);
            res.ringColor = mix(ownColor, "#ffffff", RING_SELECT_MIX);
            res.forceLabel = true;
            res.zIndex = 3;
            return res;
          }
          if (g.areNeighbors(sel, node)) {
            // neighbor {×0.76, min 4, zIndex 2}
            res.size = Math.max(base * 0.76, 4);
            res.zIndex = 2;
          } else {
            muteNode();
            return res;
          }
        } else if (hov && hov !== node && !g.areNeighbors(hov, node)) {
          // **Hover dims everything else too**, just one step lighter than selection (see
          // HOVER_MUTE). The neighbours are kept: what hover has to answer is precisely "what is
          // it connected to".
          // **Nodes that do not exist at this instant are dimmed to the floor**: this branch
          // returns early and bypasses the time filter below, so dimming them only halfway would
          // leave them brighter than when nothing is hovered at all
          if (f.activeNodes && !f.activeNodes.has(node)) muteNode();
          else softMute();
          return res;
        } else {
          // default {×0.7}
          res.size = base * 0.7;
        }
        if (f.activeNodes && !f.activeNodes.has(node)) {
          muteNode();
          return res;
        }
        // Playback fade: transition from the muted form to the normal form computed for this
        // frame
        const fs = fadeRef.current.get(node);
        if (fs !== undefined) {
          const t = Math.min(1, (performance.now() - fs) / FADE_MS);
          res.size = (res.size as number) * (0.55 + 0.45 * t);
          res.color = lerpColor(
            MUTED_SHELL,
            String(res.color ?? NODE_CORE_BASE),
            t,
          );
          res.shellColor = lerpColor(
            MUTED_SHELL,
            String(res.shellColor ?? NODE_SHELL_BASE),
            t,
          );
          res.borderColor = lerpColor(
            "rgba(0,0,0,0)",
            String(res.borderColor ?? NODE_BORDER_BASE),
            t,
          );
          if (t < 0.7) res.label = "";
        }
        return res;
      },
      edgeReducer: (edge, attrs) => {
        const f = filterRef.current;
        const res = { ...attrs };
        const [s, t] = g.extremities(edge);
        /* The inverse phrasings folded into this edge, appended after its own name:
           `PART OF ⁻¹ CONTAINS`.
           **Only shown while it has attention** -- showing it permanently doubles the label's
           length, and over-long labels are exactly the ailment being treated here.
           Two triggers, because **an edge is only one pixel wide and precise hovering is hard
           for a human to hit**: the mouse is on this edge, or on either of the nodes at its
           ends. The latter is the one that actually gets used; the former stays because
           sometimes a person really does mean to point at that one edge.
           Placed before the hide/dim logic: a phrasing is a matter of display, not of
           visibility */
        const also = attrs.alsoLabels as string[] | undefined;
        if (also && also.length > 0) {
          const focused =
            edge === hoverEdgeRef.current ||
            hoverRef.current === s ||
            hoverRef.current === t ||
            selectedRef.current === s ||
            selectedRef.current === t;
          if (focused) {
            res.label = `${attrs.label} ⁻¹ ${also
              .map((l) => l.toUpperCase())
              .join(" / ")}`;
          }
        }
        const sk = g.getNodeAttribute(s, "typeKey") as string;
        const tk = g.getNodeAttribute(t, "typeKey") as string;
        if (f.hiddenTypes.has(sk) || f.hiddenTypes.has(tk)) {
          res.hidden = true;
          return res;
        }
        // Derived edges: first whether they are hidden, then where the breathing sits.
        // **Placed right at the front** -- a hidden edge does not need the brightening/dimming
        // below computed at all
        const isDerived = attrs.derived === true;

        if (isDerived) {
          // Its turn to enter has not come yet: do not draw it. **Facts settle first, the
          // derived ones arrive after**
          if (!derivedRevealedRef.current) {
            res.hidden = true;
            return res;
          }
          const tr = derivedToggleRef.current;
          const k = tr
            ? Math.min(1, (performance.now() - tr.at) / DERIVED_TOGGLE_MS)
            : 1;
          // Switched off: the only case still left unhidden is "the fade-out has not finished
          // running"
          if (!f.showDerived) {
            if (!tr || tr.on || k >= 1) {
              res.hidden = true;
              return res;
            }
            /* Fade out from the current colour to the near-background colour. **The dimness has
               to be encoded into the RGB** (see the comment at EDGE_DIM: under premultiplied
               blending alpha cannot dim an edge), so this mixes toward EDGE_DIM rather than
               lowering alpha.

               **The starting point must not be hard-coded to full-brightness gold across the
               board**: this branch returns before the hover/selection dimming logic, so an
               unrelated derived edge that was dimmed dark would jump back to full brightness
               first and then fade out -- that jump is the "unrelated edges flash when you turn
               derivations off". The start has to be taken from how it actually looks right
               now */
            const selNow =
              selectedRef.current && g.hasNode(selectedRef.current)
                ? selectedRef.current
                : null;
            const hovNow = hoverRef.current;
            const focused = selNow ?? hovNow;
            const ghost = attrs.blocked === true;
            const from = !focused
              ? ghost
                ? EDGE_GHOST
                : EDGE_COLOR_DERIVED
              : s === focused || t === focused
                ? ghost
                  ? EDGE_GHOST_FOCUS
                  : EDGE_FOCUS_DERIVED
                : EDGE_DIM;
            res.color = lerpColor(from, EDGE_DIM, k);
            res.label = "";
            return res;
          }
          // Ghost edges do not breathe: they are not knowledge, they are a path that did not
          // go through
          const pulse = attrs.blocked === true
            ? EDGE_GHOST
            : lerpColor(
            EDGE_COLOR_DERIVED_DIM,
            EDGE_COLOR_DERIVED,
            // A triangle wave rather than a sine: it pauses for an instant at each end, so it
            // reads as "breathing" and not as "blinking"
            Math.abs(
              ((performance.now() % DERIVED_PULSE_MS) / DERIVED_PULSE_MS) * 2 -
                1,
            ),
          );
          // Switched on: brighten up from the near-background colour and join the breathing
          res.color =
            tr && tr.on && k < 1 ? lerpColor(EDGE_DIM, pulse, k) : pulse;
        }
        // hover: only brighten the incident edges; selected: brighten the incident edges + dim
        // everything else
        const hov = hoverRef.current;
        const sel =
          selectedRef.current && g.hasNode(selectedRef.current)
            ? selectedRef.current
            : null;
        const boost = () => {
          res.color =
            attrs.blocked === true
              ? EDGE_GHOST_FOCUS
              : attrs.contested === true
                ? EDGE_FOCUS_CONTEST
                : isDerived
                  ? EDGE_FOCUS_DERIVED
                  : EDGE_FOCUS;
          res.size = Math.max((attrs.size as number) * 1.42, 1.85);
          res.zIndex = 5;
        };
        /* **Whether this edge exists at the instant the timeline is parked on.**
           Both hover branches return early and bypass the time filter below -- without carrying
           this along, one hover makes every edge that "has not grown yet" jump from the
           near-background colour to 45% of its normal colour, which looks like it was lit up.
           That is exactly how brightly it lit up in practice */
        const liveNow = !f.activeEdges || f.activeEdges.has(edge);
        if (hov && (s === hov || t === hov) && liveNow) {
          boost();
        } else if (hov && !sel) {
          // On hover the other edges recede too, but **only halfway** -- the same HOVER_MUTE as
          // on the node side. Dimming to the floor is a privilege of selection. Edges that do
          // not exist at this instant **should be dark anyway**, and mixing from EDGE_DIM leaves
          // them exactly where they were
          const from = liveNow ? String(res.color) : EDGE_DIM;
          res.color = lerpColor(from, EDGE_DIM, HOVER_MUTE);
          res.size = (attrs.size as number) * (1 - 0.4 * HOVER_MUTE);
          res.label = "";
          return res;
        } else if (sel) {
          if (s === sel || t === sel) {
            boost();
          } else {
            res.color = EDGE_DIM;
            res.size = (attrs.size as number) * 0.6;
            res.label = "";
            return res;
          }
        }
        if (f.activeEdges && !f.activeEdges.has(edge)) {
          res.color = EDGE_DIM;
          res.label = "";
          return res;
        }
        // Playback fade: the edge brightens from the near-background colour to its normal colour
        // (alpha interpolated along with it)
        const fs = fadeRef.current.get(edge);
        if (fs !== undefined) {
          const t = Math.min(1, (performance.now() - fs) / FADE_MS);
          res.color = lerpColor(EDGE_DIM, String(res.color), t);
          if (t < 0.8) res.label = "";
        }
        return res;
      },
    });
    sigma.on("clickNode", ({ node }) => setSelected(node));
    // Clicking a ghost edge: open its subject's panel, land on the "derived" tab and expand
    // that row (0017 §3)
    sigma.on("clickEdge", ({ edge }) => {
      if (g.getEdgeAttribute(edge, "blocked") !== true) return;
      const [s] = g.extremities(edge);
      panelIntentRef.current = { view: "derived", open: edge };
      setSelected(s);
    });
    sigma.on("doubleClickNode", ({ node, event }) => {
      event.preventSigmaDefault();
      setFocusEntity(node);
      setSelected(node);
    });
    sigma.on("clickStage", () => deselect());
    sigma.on("enterNode", ({ node }) => {
      hoverRef.current = node;
      sigma.refresh();
    });
    sigma.on("leaveNode", () => {
      hoverRef.current = null;
      sigma.refresh();
    });
    /* Hovering an edge surfaces the phrasings folded into it.
       The inverse edge was folded away (see `layOutParallelEdges`), and drawing one fewer edge
       is right, but that name should not disappear along with it: that `part_of` reversed is
       called `contains` is something written down in the ontology, and people have a right to
       see it. **Only shown on hover** -- showing it permanently doubles the label's length, and
       over-long labels are exactly the ailment being treated here */
    sigma.on("enterEdge", ({ edge }) => {
      hoverEdgeRef.current = edge;
      sigma.refresh();
    });
    sigma.on("leaveEdge", () => {
      hoverEdgeRef.current = null;
      sigma.refresh();
    });
    // Edge labels only appear once zoomed in (at the default viewing distance they are too
    // dense; Semantica is equally restrained)
    const updateEdgeLabels = () =>
      sigma.setSetting("renderEdgeLabels", sigma.getCamera().ratio < 0.7);
    sigma.getCamera().on("updated", updateEdgeLabels);
    updateEdgeLabels();

    // World-coordinate grid: redrawn when the camera moves or the container is resized
    const renderGrid = () => {
      if (gridRef.current) drawWorldGrid(gridRef.current, sigma);
    };
    sigma.getCamera().on("updated", renderGrid);
    sigma.on("resize", renderGrid);
    renderGrid();

    // Node dragging + live force feedback. Pressing down only records a candidate: only a
    // viewport displacement of >4px promotes it to a drag
    // (otherwise a plain click would start FA2 by mistake); the dragged node is pinned onto the
    // cursor by fa2's outputReducer (see above), and after release it settles for ~1.2s and stops
    let settleTimer: ReturnType<typeof setTimeout> | null = null;
    let dragCandidate: string | null = null;
    let downPoint: { x: number; y: number } | null = null;
    sigma.on("downNode", (e) => {
      dragCandidate = e.node;
      downPoint = { x: e.event.x, y: e.event.y };
    });
    sigma.getMouseCaptor().on("mousemovebody", (e) => {
      if (!dragCandidate) return;
      if (!dragged) {
        if (!downPoint || Math.hypot(e.x - downPoint.x, e.y - downPoint.y) < 4)
          return;
        // Promoted to a drag
        dragged = dragCandidate;
        if (settleTimer) clearTimeout(settleTimer);
        // Under a static layout (circular/pack) dragging does not wake the force simulation --
        // otherwise one touch and it falls apart
        if (layoutModeRef.current === "force" && fa2 && !fa2.isRunning())
          fa2.start();
        // Freeze the current bounding box so the camera does not auto-zoom along with the drag
        if (!sigma.getCustomBBox()) sigma.setCustomBBox(sigma.getBBox());
      }
      const pos = sigma.viewportToGraph(e);
      dragPos = pos;
      g.setNodeAttribute(dragged, "x", pos.x);
      g.setNodeAttribute(dragged, "y", pos.y);
      // Prevent the camera from panning
      e.preventSigmaDefault();
      e.original.preventDefault();
      e.original.stopPropagation();
    });
    const endDrag = () => {
      dragCandidate = null;
      downPoint = null;
      if (!dragged) return;
      dragged = null;
      dragPos = null;
      settleTimer = setTimeout(() => fa2?.stop(), 1200);
    };
    sigma.getMouseCaptor().on("mouseup", endDrag);
    sigmaRef.current = sigma;
    if (import.meta.env.DEV) {
      // Debug handles (dev only): inspect the reducer's output in a headless environment
      (window as unknown as Record<string, unknown>).__g = g;
      (window as unknown as Record<string, unknown>).__sigma = sigma;
      (window as unknown as Record<string, unknown>).__sel = selectedRef;
    }
    recomputeActive(timeT);
    return () => {
      if (stabilizeTimer) clearTimeout(stabilizeTimer);
      if (settleTimer) clearTimeout(settleTimer);
      fa2?.kill();
      setStabilizing(false);
      sigma.kill();
      sigmaRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data.data]);

  if (!kb)
    return <div className="p-8 text-sm text-neutral-500">{S.nav.loading}</div>;

  const empty = data.isSuccess && data.data.nodes.length === 0;
  const nodeCount = data.data?.nodes.length ?? 0;
  const edgeCount = data.data?.edges.length ?? 0;
  // How many there are in the KB altogether. **Not the same thing as how many were drawn** --
  // the neighbourhood view has no total (it is only ever a small slice), so it falls back to the
  // drawn count and will not read "0 in total"
  const totalNodes = data.data?.total_nodes ?? nodeCount;
  const totalEdges = data.data?.total_edges ?? edgeCount;
  const capped = totalNodes > nodeCount;

  return (
    <div className="h-full relative">
      {/* Top floating bar: search + legend + status */}
      <div className="absolute top-3 left-3 right-3 z-10 flex items-start gap-2 pointer-events-none">
        <div className="relative pointer-events-auto">
          <input
            className="input-dark w-60 px-3 py-1.5 text-sm shadow-lg"
            placeholder={
              inSubgraph ? S.graph.searchInSubgraph : S.graph.searchEntity
            }
            value={searchInput}
            onChange={(e) => {
              setSearchInput(e.target.value);
              setSearchQ(e.target.value.trim());
            }}
          />
          {searchQ && searchHits.length > 0 && (
            <div className="glass-strong absolute mt-1 w-full rounded-lg shadow-xl overflow-hidden">
              {searchHits.map((c) => (
                <button
                  key={c.id}
                  onClick={() => {
                    // A hit inside the subgraph: just select it (it is already in view); a
                    // whole-graph search: jump to that entity's neighbourhood
                    if (!inSubgraph) setFocusEntity(c.id);
                    setSelected(c.id);
                    setSearchInput("");
                    setSearchQ("");
                  }}
                  className="w-full px-3 py-1.5 text-left text-sm text-neutral-200 hover:bg-white/5 flex items-center gap-2"
                >
                  <span
                    className="h-2.5 w-2.5 rounded-full shrink-0"
                    style={{ background: c.color }}
                  />
                  <span className="truncate">{c.name}</span>
                  {c.disambiguator && (
                    <span className="text-xs text-neutral-500 truncate">
                      · {c.disambiguator}
                    </span>
                  )}
                  <span className="ml-auto text-xs text-neutral-500">
                    {c.type_label}
                  </span>
                </button>
              ))}
              {/* There are more that are not shown. **Say how many are left** -- it used to be
                  a fixed ten, and when the one you were after was not among those ten the UI
                  gave no hint whatsoever. Searching inside a subgraph is client-side filtering,
                  where there is no such thing as "more" */}
              {!inSubgraph &&
                (candidates.data?.total ?? 0) > searchHits.length && (
                  <button
                    onClick={() => setSearchLimit((n) => n + 20)}
                    className="w-full border-t border-white/10 px-3 py-1.5 text-left text-xs text-neutral-400 hover:bg-white/5 hover:text-neutral-200"
                  >
                    {S.graph.searchMore(
                      candidates.data!.total - searchHits.length,
                    )}
                  </button>
                )}
            </div>
          )}
        </div>
        {focusEntity && (
          <button
            onClick={() => setFocusEntity(null)}
            className="u-btn u-btn-ghost glass-strong pointer-events-auto px-3 py-1.5 text-sm shadow-lg"
          >
            {S.graph.backToOverview}
          </button>
        )}

        {/* Legend (click to toggle a type's visibility). **Only the first LEGEND_MAX are laid
            out**, the rest go into "+N types" -- that row grows sideways, so with many types it
            wraps and pushes the canvas down; and with a dozen identical chips lined up you
            cannot read which one matters */}
        <div className="pointer-events-auto flex flex-wrap gap-1.5 pt-0.5">
          {legendShown.map(([key, t]) => (
            <button
              key={key}
              onClick={() =>
                setHiddenTypes((prev) => {
                  const next = new Set(prev);
                  if (next.has(key)) next.delete(key);
                  else next.add(key);
                  return next;
                })
              }
              className={`glass rounded-full px-2.5 py-1 text-[11px] flex items-center gap-1.5 transition-opacity ${
                hiddenTypes.has(key) ? "opacity-35" : ""
              }`}
            >
              <span
                className={`h-2 w-2 ${t.shape === "square" ? "" : "rounded-full"}`}
                style={{ background: t.color }}
              />
              <span className="text-neutral-300">{t.label}</span>
            </button>
          ))}

          {/* The number on the chip is **all types**, not just the collapsed few --
              what you see on opening it is all of them (any one is searchable), and writing
              "+3" would be promising something else */}
          {/* Reset. **Whenever anything is hidden, offer a one-step way out** -- "only this"
              very easily narrows the picture right down, and without this you would have to
              click each one back by hand */}
          {hiddenTypes.size > 0 && (
            <button
              onClick={() => setHiddenTypes(new Set())}
              className="glass rounded-full px-2.5 py-1 text-[11px] text-neutral-400 transition-colors hover:text-neutral-100"
            >
              {S.graph.legendShowAll(hiddenTypes.size)}
            </button>
          )}

          {legendRest.length > 0 && (
            <div className="relative" ref={legendPop.rootRef}>
              <button
                ref={legendPop.anchorRef}
                onClick={() =>
                  legendPop.open ? legendPop.close() : legendPop.setOpen(true)
                }
                title={S.graph.legendAllHint}
                aria-expanded={legendPop.open}
                className={`glass rounded-full px-2.5 py-1 text-[11px] flex items-center gap-1.5 transition-colors ${
                  legendPop.open ? "text-neutral-100" : "text-neutral-400"
                } hover:text-neutral-100`}
              >
                {S.graph.legendMore(types.length)}
                {/* A dot when one of the collapsed types is hidden. **Without the dot it is
                    silent filtering**: turn a type off inside the panel, collapse the panel, and
                    nothing in the UI says any longer that it is off */}
                {hiddenInRest > 0 && (
                  <span className="h-1.5 w-1.5 rounded-full bg-neutral-300" />
                )}
              </button>
              {legendPop.open && (
                <div
                  ref={legendPop.panelRef}
                  className="u-menu-glass absolute left-0 top-0 z-50 w-64 overflow-hidden rounded-xl p-2 shadow-2xl"
                >
                  {/* The panel covers the chip's original position, so **the first row is
                      shaped exactly like that chip** and clicking it collapses again -- "it
                      folds back to wherever it unfolded from", the same reasoning that puts the
                      notification/user cards' close key exactly where the trigger was */}
                  <button
                    onClick={() => legendPop.close()}
                    className="mb-1.5 flex w-full items-center gap-1.5 rounded-full px-1.5 py-0.5 text-[11px] text-neutral-300 transition-colors hover:text-neutral-100"
                  >
                    {S.graph.legendMore(types.length)}
                    <X size={11} className="ml-auto text-neutral-500" />
                  </button>
                  <input
                    autoFocus
                    value={legendQ}
                    onChange={(e) => setLegendQ(e.target.value)}
                    placeholder={S.graph.legendSearch}
                    className="input-dark mb-1.5 w-full px-2 py-1 text-[12px]"
                  />
                  {/* **This lists all the types, not just the collapsed ones**: when you are
                      looking for a type, nobody remembers whether it happened to make the first
                      few */}
                  <div className="flex max-h-64 flex-col overflow-y-auto">
                    {types
                      .filter(([, t]) =>
                        t.label.toLowerCase().includes(legendQ.toLowerCase()),
                      )
                      .map(([key, t]) => (
                        /* **Two buttons per row, not one button cycling three states.**
                           The price of a single cycling button is that you cannot know what the
                           next click will do without reading the current state, and getting from
                           "only this" back to normal has to pass through "excluded"
                           -- you want to clear the filter and first have to make the picture
                           wrong in another way. Split apart, each gesture has one fixed meaning */
                        <div
                          key={key}
                          className="group flex items-center gap-2 rounded px-1.5 py-1 hover:bg-white/5"
                        >
                          <button
                            onClick={() =>
                              setHiddenTypes((prev) => {
                                const next = new Set(prev);
                                if (next.has(key)) next.delete(key);
                                else next.add(key);
                                return next;
                              })
                            }
                            className="flex min-w-0 flex-1 items-center gap-2 text-left"
                          >
                            <span
                              className={`h-2 w-2 shrink-0 ${t.shape === "square" ? "" : "rounded-full"}`}
                              style={{
                                background: t.color,
                                opacity: hiddenTypes.has(key) ? 0.35 : 1,
                              }}
                            />
                            <span
                              className={`truncate text-[12px] ${
                                hiddenTypes.has(key)
                                  ? "text-neutral-500 line-through"
                                  : "text-neutral-200"
                              }`}
                            >
                              {t.label}
                            </span>
                          </button>
                          {/* "Only this": the action you want most once there are many types.
                              **An explicit button rather than a modifier key** -- nobody guesses
                              alt+click, and there is room here horizontally */}
                          <button
                            onClick={() =>
                              setHiddenTypes(
                                new Set(
                                  types.map(([k]) => k).filter((k) => k !== key),
                                ),
                              )
                            }
                            className="shrink-0 rounded px-1 text-[10px] text-neutral-500 opacity-0 transition-opacity hover:text-white focus:opacity-100 group-hover:opacity-100"
                          >
                            {S.graph.legendOnly}
                          </button>
                          <span className="u-num shrink-0 text-[11px] text-neutral-500">
                            {t.count}
                          </span>
                        </div>
                      ))}
                    {types.every(
                      ([, t]) =>
                        !t.label.toLowerCase().includes(legendQ.toLowerCase()),
                    ) && (
                      <div className="px-1.5 py-2 text-[12px] text-neutral-500">
                        {S.graph.legendNone}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Top right: the "how many to draw" control + the stats. **The stats are about exactly
            this number** ("150 drawn, 548 in total"), so putting the control next to them makes
            it obvious at a glance what is being changed.
            The shell stays neutral -- this area is chrome, and colour belongs to the data only */}
        <div className="ml-auto flex flex-col items-end gap-1">
          <div className="flex items-start gap-2">
            <div className="pointer-events-auto flex items-center overflow-hidden rounded-md border border-white/10">
            <button
              title={S.graph.nodeBudgetLess}
              disabled={nodeBudget <= NODE_BUDGETS[0]}
              onClick={() =>
                setNodeBudget(
                  (b) => NODE_BUDGETS[Math.max(0, NODE_BUDGETS.indexOf(b) - 1)],
                )
              }
              className="px-1.5 py-[3px] text-[11px] leading-none text-neutral-400 transition-colors hover:bg-white/[0.06] hover:text-white disabled:opacity-25 disabled:hover:bg-transparent disabled:hover:text-neutral-400"
            >
              −
            </button>
            {/* **Once everything is drawn, stop offering "draw more"**: that is all the KB has,
                turning it up higher changes nothing, and a button that does nothing when clicked
                is worse than no button at all */}
            <button
              title={S.graph.nodeBudgetMore}
              disabled={
                !capped || nodeBudget >= NODE_BUDGETS[NODE_BUDGETS.length - 1]
              }
              onClick={() =>
                setNodeBudget(
                  (b) =>
                    NODE_BUDGETS[
                      Math.min(NODE_BUDGETS.length - 1, NODE_BUDGETS.indexOf(b) + 1)
                    ],
                )
              }
              className="px-1.5 py-[3px] text-[11px] leading-none text-neutral-400 transition-colors hover:bg-white/[0.06] hover:text-white disabled:opacity-25 disabled:hover:bg-transparent disabled:hover:text-neutral-400"
            >
              +
            </button>
          </div>
          <div className="pointer-events-none pt-0.5 u-num text-[11px] text-neutral-500">
          {/* When the cap is hit, spell out "how many drawn / how many in total". **This number
              used to be the cap masquerading as the size** -- a KB with tens of thousands of
              entities forever read 150 in the top right corner */}
          {capped ? (
            <span title={S.graph.cappedHint(nodeCount, totalNodes)}>
              {/* **Facts use the "drawn / total" framing too**: this used to give the KB's
                  total while the entities gave "how many drawn / how many in total" -- two
                  framings in one sentence, so changing the step moved the entity count while the
                  fact count did not budge, and it looked broken.
                  With no time filter active is always equal to the drawn count, so it is left
                  unsaid */}
              {S.graph.statsCapped(
                nodeCount,
                totalNodes,
                edgeCount,
                totalEdges,
                timeT === null ? null : activeCount,
              )}
            </span>
          ) : (
            S.graph.stats(
              nodeCount,
              edgeCount,
              timeT === null ? null : activeCount,
            )
          )}
            </div>
          </div>
          {/* **A row of its own, not a prefix to the stats text.**
              As a prefix, the moment it appears it widens the whole block, and this block is
              right-aligned -- so on every relayout the step buttons on the left get shoved and
              jump. On a row of its own, the first row's width no longer varies with it */}
          {stabilizing && (
            <div className="flex items-center gap-1.5 text-[11px] text-neutral-400">
              <Loader2 size={11} className="animate-spin" />
              {S.graph.stabilizing}
            </div>
          )}
        </div>
      </div>

      {/* Canvas: the world-coordinate grid layer (moving with the camera) is laid under sigma's
          WebGL layer (full bleed, with the time island floating above it) */}
      <div className="absolute inset-0">
        <canvas ref={gridRef} className="absolute inset-0 h-full w-full" />
        <div ref={containerRef} className="absolute inset-0" />
      </div>

      {/* Bottom-left control tower: derived edges + layout switching + camera (bottom right
          belongs to the entity sidebar, bottom centre to the time island) */}
      {/* **items-start**: items in a column stretch by default, so one group expanding drags all
          the others out to the same width -- and their text is still collapsed, so they read as
          a few inexplicable blank bars. Each sizing to its own content is what makes it "one
          group expands at a time, without dragging the others along" */}
      <div className="absolute bottom-4 left-3 z-10 flex flex-col items-start gap-2">
        {/* Derived edges: **a group of their own, and not part of the type legend either.**
            The legend answers "which types are shown", a row made entirely of ontology types;
            this answers "are derived edges shown" -- not the same question. At zero the whole
            group does not appear.

            **Putting it on this tower is a way around a pair of conflicting constraints**: next
            to the legend in the top bar it looks like a 10th type; distinguishing it by colour
            runs straight into the principle established at the top of this file
            -- "zero colour cast in the chrome, colour belongs to the data only" (see the palette
            comment). Wedging a block of highly saturated gold into the frame would be the only
            patch of colour in the entire UI: glaring, and part of no system at all.

            This tower was always the territory of "how to look at the view" (layout, zoom), and
            "are derived edges shown" is precisely the same family of question. The shell stays
            neutral and the gold appears only on the icon itself -- the same approach as the
            colour dots on the type chips. */}
        {derivedCount > 0 && (
          /* **Two layers**: the outer one only handles positioning, and only the inner one has
             overflow-hidden. That class is there to clip the button stack's rounded corners, but
             the panel is a child of the same box -- collapsed into one layer the panel gets
             clipped along with it, measured down to just the tower's own 32px of width */
          <div className="relative" ref={derivedPop.rootRef}>
            <div className="u-tower group glass-strong rounded-xl shadow-xl flex flex-col overflow-hidden">
            <button
              onClick={() => setShowDerived((v) => !v)}
              role="switch"
              aria-checked={showDerived}
              title={`${S.graph.derivedEdges(derivedCount)} · ${S.graph.derivedHint}`}
              className={`flex items-center p-2 transition-colors ${
                showDerived
                  ? "bg-white/[0.1]"
                  : "text-neutral-500 hover:bg-white/[0.06]"
              }`}
              style={
                showDerived ? { color: "rgba(231,197,124,0.95)" } : undefined
              }
            >
              <Waypoints size={15} />
              <span className="u-tower-label">{S.graph.viewDerived}</span>
            </button>
            <div className="h-px bg-white/10 mx-1.5" />
            {/* Expands into a small window: when this batch of edges was derived, whether it is
                still being derived, and one more manual run.
                **Split off from the switch into two buttons** -- "hide them" is clicked every
                day, "when were they derived" is only asked occasionally, and merging the two
                would add a step to the common action */}
            <button
              ref={derivedPop.anchorRef}
              onClick={() =>
                derivedPop.open ? derivedPop.close() : derivedPop.setOpen(true)
              }
              title={S.graph.derivedPanel}
              aria-expanded={derivedPop.open}
              className={`flex items-center p-2 text-[11px] leading-none transition-colors ${
                derivedPop.open
                  ? "text-white bg-white/[0.1]"
                  : "text-neutral-400 hover:text-white hover:bg-white/[0.06]"
              }`}
            >
              <span className="grid h-[15px] w-[15px] shrink-0 place-items-center leading-none">
                ⋯
              </span>
              <span className="u-tower-label">{S.graph.derivedPanel}</span>
            </button>
            </div>
            {derivedPop.open && kb && (
              <DerivedPanel
                panelRef={derivedPop.panelRef}
                kbId={kb.id}
                count={derivedCount}
                onClose={() => derivedPop.close()}
              />
            )}
          </div>
        )}
        <div className="u-tower group glass-strong rounded-xl shadow-xl flex flex-col overflow-hidden">
          {(
            [
              { key: "force", Icon: Orbit, label: S.graph.layoutForce },
              {
                key: "circular",
                Icon: CircleDashed,
                label: S.graph.layoutCircular,
              },
              { key: "pack", Icon: Grape, label: S.graph.layoutPack },
            ] as const
          ).map(({ key, Icon, label }) => (
            <button
              key={key}
              title={label}
              onClick={() => {
                setLayoutMode(key);
                layoutModeRef.current = key;
                layoutCtlRef.current?.apply(key);
              }}
              className={`flex items-center p-2 transition-colors ${
                layoutMode === key
                  ? "text-white bg-white/[0.1]"
                  : "text-neutral-400 hover:text-white hover:bg-white/[0.06]"
              }`}
            >
              <Icon size={15} />
              <span className="u-tower-label">{label}</span>
            </button>
          ))}
        </div>
        <div className="u-tower group glass-strong rounded-xl shadow-xl flex flex-col overflow-hidden">
          <button
            title={S.graph.zoomIn}
            onClick={() =>
              sigmaRef.current?.getCamera().animatedZoom({ duration: 220 })
            }
            className="flex items-center p-2 text-neutral-400 hover:text-white hover:bg-white/[0.06] transition-colors"
          >
            <ZoomIn size={15} />
            <span className="u-tower-label">{S.graph.zoomIn}</span>
          </button>
          <button
            title={S.graph.zoomOut}
            onClick={() =>
              sigmaRef.current?.getCamera().animatedUnzoom({ duration: 220 })
            }
            className="flex items-center p-2 text-neutral-400 hover:text-white hover:bg-white/[0.06] transition-colors"
          >
            <ZoomOut size={15} />
            <span className="u-tower-label">{S.graph.zoomOut}</span>
          </button>
          <div className="h-px bg-white/10 mx-1.5" />
          <button
            title={S.graph.fitView}
            onClick={() =>
              sigmaRef.current?.getCamera().animatedReset({ duration: 300 })
            }
            className="flex items-center p-2 text-neutral-400 hover:text-white hover:bg-white/[0.06] transition-colors"
          >
            <Maximize2 size={15} />
            <span className="u-tower-label">{S.graph.fitView}</span>
          </button>
        </div>
      </div>

      {empty && (
        <div className="absolute inset-0 grid place-items-center pointer-events-none">
          {/* No title block: the page itself is the graph page and the tab bar says so too, so
              writing "Graph" a third time carries no information at all. What an empty state
              should say is what to do next */}
          <div className="text-center text-sm text-neutral-500 max-w-xs">
            {S.graph.emptyBody}
          </div>
        </div>
      )}

      {/* Bottom-centre floating time island */}
      {edgeCount > 0 && (
        <TimeScrubber
          edges={data.data!.edges}
          value={timeT}
          onChange={setTimeT}
          playing={playing}
          onPlayingChange={setPlaying}
        />
      )}

      {/* Entity sidebar. **It stays for another 170ms after selection is cleared**: that is the
          exit animation playing */}
      {(selected || exiting) && kb && (
        <EntityPanel
          kbId={kb.id}
          entityId={(selected ?? exiting)!}
          exiting={!selected}
          intent={panelIntentRef}
          onClose={deselect}
          onNavigate={(id) => {
            // The navigation target may not be on the current canvas: refocus the graph on its
            // neighbourhood at the same time (consistent with picking a search result)
            setFocusEntity(id);
            setSelected(id);
          }}
        />
      )}
    </div>
  );
}

/* ====== Timeline (bottom-centre floating island: play + density band + drag) ====== */

/** Track clientX → a time value snapped to day steps (the data's precision is day and dragging
 *  wants fineness; playback still advances by month, for the rhythm). */
function scrubValueAt(
  clientX: number,
  track: HTMLDivElement | null,
  minTs: number,
  maxTs: number,
): number {
  if (!track) return maxTs;
  const rect = track.getBoundingClientRect();
  // Before the layout has taken shape (width 0), avoid dividing by zero and producing NaN
  if (rect.width < 1) return maxTs;
  const frac = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
  const raw = minTs + frac * (maxTs - minTs);
  return Math.min(maxTs, minTs + Math.round((raw - minTs) / DAY_MS) * DAY_MS);
}

/** The step for playback/the bars. **These two were always meant to be the same unit** -- the
 *  bars used to go by year and playback by day, and nowhere in the UI could you tell "how long
 *  is one cell". */
type ScrubUnit = "year" | "month" | "day";

/** How many bars are drawn at most. Beyond that, adjacent buckets are merged into one bar --
 *  **this affects the drawing only, not the playback step**: at day granularity 15 years is over
 *  five thousand buckets, which will not fit even at one pixel each, yet playback still advances
 *  one day at a time. How many were merged is stated in the tooltip, not swallowed */
const SCRUB_MAX_BARS = 220;
/** The target duration for traversing the whole track. **Independent of the unit** -- the unit
 *  changes granularity and density, and it should not change "how long you wait" along with
 *  them: at day granularity, "one beat per day" would take twenty minutes to play 15 years */
const SCRUB_PLAY_MS = 18000;

function bucketStart(ts: number, unit: ScrubUnit): number {
  const d = new Date(ts);
  if (unit === "year") return Date.UTC(d.getUTCFullYear(), 0, 1);
  if (unit === "month")
    return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate());
}
function bucketNext(ts: number, unit: ScrubUnit): number {
  const d = new Date(ts);
  if (unit === "year") return Date.UTC(d.getUTCFullYear() + 1, 0, 1);
  if (unit === "month")
    return Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
  return ts + DAY_MS;
}

function TimeScrubber({
  edges,
  value,
  onChange,
  playing,
  onPlayingChange,
}: {
  edges: GraphEdge[];
  value: number | null;
  onChange: (v: number | null) => void;
  /* The playback state is owned by Graph: the render layer has to distinguish playback
     advancing from manual dragging */
  playing: boolean;
  onPlayingChange: (v: boolean) => void;
}) {
  const setPlaying = onPlayingChange;
  /* Year by default: **most KBs span years**, so what you get on arrival is the granularity
     you can take in at a glance */
  const [unit, setUnit] = useState<ScrubUnit>("year");
  /* How many times the track has been traversed. **Used as a key** -- retriggering the same
     animation on the same element does not replay it; changing the key so it remounts does */
  const [sweep, setSweep] = useState(0);
  /* While the pointer is over the track, the stretch already traversed is brightened. **It
     answers "how far have I got"** -- when it is not playing the whole track is one shade of
     grey and you cannot see where progress stopped; and that is exactly what somebody who moves
     the pointer onto it wants to know */
  const [trackHover, setTrackHover] = useState(false);
  const trackRef = useRef<HTMLDivElement>(null);
  const draggingRef = useRef(false);
  /* The drag's landing point. **The playback loop has its own floating-point accumulator** and
     does not read value -- otherwise each frame's rounding error would pile up. So changing
     value alone achieves nothing; the next frame overwrites it right back.
     Dragging puts the landing point in here, the loop picks it up on its next frame and carries
     on from the new position */
  const seekRef = useRef<number | null>(null);
  const seek = (v: number) => {
    seekRef.current = v;
    onChange(v);
  };

  const { minTs, maxTs, bars, merged, trackW } = useMemo(() => {
    const now = Date.now();
    const froms = edges
      .map((e) => (e.valid_from ? Date.parse(e.valid_from) : NaN))
      .filter((t) => !Number.isNaN(t));
    const min = froms.length
      ? Math.min(...froms)
      : now - 5 * 365 * 24 * 3600 * 1000;
    // The start is aligned to a unit boundary: otherwise the first bar is half a cell, which
    // reads as a chunk of missing data
    const start = bucketStart(min, unit);

    const counts = new Map<number, number>();
    for (const t of froms) {
      const k = bucketStart(t, unit);
      counts.set(k, (counts.get(k) ?? 0) + 1);
    }
    const raw: { ts: number; n: number }[] = [];
    for (let t = start; t <= now; t = bucketNext(t, unit))
      raw.push({ ts: t, n: counts.get(t) ?? 0 });

    // Merge buckets when they will not fit. **The merge is about the drawing, not the step**
    const group = Math.max(1, Math.ceil(raw.length / SCRUB_MAX_BARS));
    const cells: { ts: number; n: number }[] = [];
    for (let i = 0; i < raw.length; i += group) {
      const slice = raw.slice(i, i + group);
      cells.push({
        ts: slice[0].ts,
        n: slice.reduce((a, b) => a + b.n, 0),
      });
    }
    const peak = Math.max(1, ...cells.map((c) => c.n));

    // Bigger unit → fewer buckets → shorter island; smaller → longer. **But the lower bound has
    // to be raised high enough**: the row of fixed controls in the island (play button + unit
    // selector + two years + date + All time/Now) needs four hundred-odd pixels on its own, and
    // at an island of only 320 the flex-1 track gets squeezed to 0 --
    // measured: not a single bar visible, the whole thing empty.
    //
    // Once raised, what the unit mainly changes is **the thickness of each bar**: on the same
    // track, year is a dozen-odd thick blocks and day is two hundred-odd thin lines. That says
    // more than stretching the whole island
    const w = Math.min(780, Math.max(660, 380 + cells.length * 2));

    return {
      minTs: start,
      maxTs: now,
      bars: cells.map((c) => ({ ts: c.ts, h: c.n / peak, n: c.n })),
      merged: group,
      trackW: w,
    };
  }, [edges, unit]);

  // Playback advances by day (the data is day-precision) and the days flip past quickly; the
  // overall rhythm is still ≈ one month per 260ms.
  // Driven by rAF time: frame-rate independent, accumulating internally in floating point to
  // avoid rounding drift, and the value is only pushed out when a day boundary is crossed
  useEffect(() => {
    if (!playing) return;
    // The whole track takes about SCRUB_PLAY_MS regardless of the unit; the unit only decides
    // which cell the landing point rounds to
    const SPEED = (maxTs - minTs) / SCRUB_PLAY_MS;
    let raf = 0;
    let last = performance.now();
    let acc = value ?? minTs;
    let lastPushed = 0;
    const step = (now: number) => {
      // Somebody has dragged: carry on from the landing point, not along the old trajectory
      if (seekRef.current !== null) {
        acc = seekRef.current;
        seekRef.current = null;
      }
      acc += (now - last) * SPEED;
      last = now;
      if (acc >= maxTs) {
        setPlaying(false);
        onChange(null);
        // A sweep of light on reaching the end. **This is a closing gesture** -- playback stops
        // and time jumps back to all-time, which with nothing to explain it looks like it broke
        // off midway; a sweep of light says "this one ran all the way through"
        setSweep((n) => n + 1);
        return;
      }
      // **Continuous advance, no hopping from bucket to bucket.** It used to round through
      // `bucketStart` before pushing, which at year granularity meant a whole year at a time --
      // the playhead hopped cell by cell and looked like stutter rather than motion.
      // The unit now governs **display** only (label precision, bar span), no longer the step.
      //
      // The price is denser pushes (one per frame), and every push has to recompute the current
      // edges for the whole graph, so it is limited to ~30fps: the eye cannot tell it from
      // 60fps, and it halves the recomputation
      if (now - lastPushed >= 33) {
        lastPushed = now;
        onChange(Math.round(acc));
      }
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
    // Only restarts with the play switch: acc is self-sustaining inside the loop, and value
    // changing every frame should not rebuild the loop
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playing, minTs, maxTs, unit]);

  // Displayed down to the day: matching the data's day-level valid_precision
  const label = (() => {
    if (value === null) return S.graph.allTime;
    const d = new Date(value);
    const mm = String(d.getUTCMonth() + 1).padStart(2, "0");
    const dd = String(d.getUTCDate()).padStart(2, "0");
    // Precision follows the unit: writing out "2019-01-01" at year granularity is false precision
    if (unit === "year") return `${d.getUTCFullYear()}`;
    if (unit === "month") return `${d.getUTCFullYear()}-${mm}`;
    return `${d.getUTCFullYear()}-${mm}-${dd}`;
  })();

  const minYear = bars.length
    ? new Date(bars[0].ts).getUTCFullYear()
    : undefined;
  const maxYear = bars.length
    ? new Date(bars[bars.length - 1].ts).getUTCFullYear()
    : undefined;

  return (
    /* The width varies with the unit: big unit → few buckets → short; small unit → many buckets
       → long and dense.
       Still clamped inside the viewport (that calc term), so it will not overflow a narrow
       screen.
       Measured widths: year 320 / month 648 / day 760. */
    <div
      className={`glass-strong absolute bottom-4 left-1/2 -translate-x-1/2 z-10 rounded-2xl px-3 py-2 flex items-center gap-2.5 shadow-[0_12px_40px_rgba(0,0,0,0.5)] u-scrub-island${playing ? " u-solid" : ""}`}
      style={{ width: `min(${trackW}px, calc(100vw - 4rem))` }}
    >
      <button
        onClick={() => {
          // Pressing play while already at the end (`Now`) has to start over. **Otherwise the
          // first press amounts to nothing happening**: acc starts where it ends, the loop
          // declares playback finished on its very first frame and only clears the position to
          // All time
          if (
            !playing &&
            (value === null || value >= maxTs - (maxTs - minTs) * 0.02)
          )
            onChange(minTs);
          setPlaying(!playing);
        }}
        title={playing ? S.graph.pause : S.graph.play}
        className="u-btn u-btn-ghost h-8 w-8 shrink-0 grid place-items-center rounded-lg"
      >
        {playing ? <Pause size={13} /> : <Play size={13} />}
      </button>

      {/* The step. **Playback and the bars share it** -- the bars used to go by year and
          playback by day, and nowhere in the UI could you tell "how long is one cell" */}
      <div
        title={S.graph.scrubUnitHint}
        /* **Same height and corner radius as the play button**: that button is h-8 / rounded-lg,
           while this used to be 20px tall, padded out by py-[3px], and rounded-md -- two things
           side by side with different sizes and different radii do not look like one set */
        className="flex h-8 shrink-0 items-center overflow-hidden rounded-lg border border-white/10"
      >
        {(["year", "month", "day"] as const).map((u) => (
          <button
            key={u}
            onClick={() => setUnit(u)}
            className={`grid h-full place-items-center px-2 text-[10px] leading-none transition-colors ${
              unit === u
                ? "bg-white/[0.08] text-neutral-100"
                : "text-neutral-500 hover:bg-white/[0.04] hover:text-neutral-300"
            }`}
          >
            {u === "year"
              ? S.graph.scrubUnitYear
              : u === "month"
                ? S.graph.scrubUnitMonth
                : S.graph.scrubUnitDay}
          </button>
        ))}
      </div>

      <span className="shrink-0 u-num text-[10px] text-neutral-600">
        {minYear}
      </span>

      {/* Density-band track: an inset light well + one bar per year of fact volume */}
      <div
        ref={trackRef}
        onMouseEnter={() => setTrackHover(true)}
        onMouseLeave={() => setTrackHover(false)}
        className="relative h-9 min-w-[150px] flex-1 overflow-hidden rounded-lg bg-white/[0.04]"
      >
        {/* When the animation ends **React** unmounts it, **do not call `remove()` yourself**.
            This used to be `onAnimationEnd={(e) => e.currentTarget.remove()}` --
            ripping a node React manages out of the DOM, without React knowing. On the next
            sweep the key changed, React went to remove the "old node", and that node was no
            longer inside its parent; removeChild threw NotFoundError, and the uncaught error
            unmounted and remounted the whole tree: the symptom being **the UI looks like it
            refreshed after two rounds of playback** */}
        {sweep > 0 && (
          <span
            key={sweep}
            className="u-sweep"
            onAnimationEnd={() => setSweep(0)}
          />
        )}
        {/* **The gap has to shrink with the density**: hard-coded at 2px, day granularity's 216
            bars have 215 gaps ≈ 430px while the track's inner width is only ~455px -- the bars
            get squeezed to 0.1px and the whole track looks empty. That is exactly how it got
            lost. When the bars are sparse, 2px makes them easy to count; when dense they sit
            flush together and read as a density band */}
        <div
          className="absolute inset-x-1.5 top-1.5 bottom-1.5 flex items-end"
          style={{ gap: bars.length > 120 ? 0 : bars.length > 40 ? 1 : 2 }}
        >
          {bars.map((b) => {
            // Lit as soon as it is entered (judged on the bucket's start): the bar under the
            // playhead already counts as covered -- the usual progress-bar semantics
            const past = value !== null && b.ts <= value;
            const d = new Date(b.ts);
            const stamp =
              unit === "year"
                ? `${d.getUTCFullYear()}`
                : unit === "month"
                  ? `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}`
                  : d.toISOString().slice(0, 10);
            return (
              <div
                key={b.ts}
                className="flex-1 flex items-end h-full"
                title={`${stamp} · ${b.n}${merged > 1 ? ` · ${S.graph.scrubBarMerged(merged)}` : ""}`}
              >
                <div
                  className="w-full rounded-[1px] transition-colors"
                  style={{
                    height: `${Math.max(10, b.h * 100)}%`,
                    // Bars already swept past brighten during playback and return to normal
                    // brightness once it stops.
                    // **The not-yet-reached ones are dimmed to nearly invisible**: they used to
                    // be 0.09, which is still clearly visible against this backing, so the
                    // right of the playhead was "lit" just like the left and you could not see
                    // how far it had got. A trace is left rather than zero -- zero would pretend
                    // that stretch has no data, when it simply has not been reached yet
                    background:
                      value !== null && past && (playing || trackHover)
                        ? "rgba(255,255,255,0.62)"
                        : value === null || past
                          ? "rgba(255,255,255,0.32)"
                          : "rgba(255,255,255,0.04)",
                  }}
                />
              </div>
            );
          })}
        </div>
        <input
          type="range"
          className="scrubber-range"
          min={minTs}
          max={maxTs}
          step={DAY_MS}
          value={value ?? maxTs}
          /* **Dragging does not stop playback**: dragging is "I want to see that stretch", not
             "I want to stop" -- after release it should carry on from the new position to the
             end.
             (The `All time` / `Now` buttons do still stop: those are explicit jumps, not
             scrubbing) */
          onChange={(e) => seek(Number(e.target.value))}
          // The native range's drag gesture gets disturbed by page-level mouse listeners (such as
          // dragging nodes on the graph) -- so the drag is driven by our own pointer capture,
          // and clicks and drags both go down the same computation path
          onPointerDown={(e) => {
            draggingRef.current = true;
            try {
              e.currentTarget.setPointerCapture(e.pointerId);
            } catch {
              /* A synthetic event's pointerId may be invalid; ignore it */
            }
            seek(scrubValueAt(e.clientX, trackRef.current, minTs, maxTs));
          }}
          onPointerMove={(e) => {
            if (draggingRef.current)
              seek(scrubValueAt(e.clientX, trackRef.current, minTs, maxTs));
          }}
          onPointerUp={() => {
            draggingRef.current = false;
          }}
          onPointerCancel={() => {
            draggingRef.current = false;
          }}
        />
      </div>

      <span className="shrink-0 u-num text-[10px] text-neutral-600">
        {maxYear}
      </span>

      <div className="w-[5.6rem] shrink-0 text-center u-num text-xs text-neutral-200">
        {label}
      </div>

      <div className="h-5 w-px shrink-0 bg-white/10" />

      {/* Two-anchor segmented control: whichever anchor you are on is highlighted and a click
          jumps there; dragged to some day in between, neither is lit */}
      <div className="flex shrink-0 rounded-lg overflow-hidden border border-white/10">
        {(
          [
            {
              key: "all",
              label: S.graph.allTime,
              active: value === null,
              to: null,
            },
            {
              key: "now",
              label: S.graph.nowBtn,
              active: value !== null && maxTs - value < DAY_MS,
              to: maxTs,
            },
          ] as const
        ).map((a) => (
          <button
            key={a.key}
            onClick={() => {
              setPlaying(false);
              onChange(a.to);
            }}
            className={`px-2.5 py-1.5 text-xs transition-colors ${
              a.active
                ? "bg-white/10 text-neutral-100"
                : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300"
            }`}
          >
            {a.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/* ============ Entity sidebar ============ */

/** World time (when this thing held) → text. **Always read as UTC, never converted to local.**
 *
 *  `valid_from` / `valid_to` come from statements in documents ("took office on 2 May 2019"),
 *  which are **calendar dates, not instants**, and have no time zone to begin with; what is
 *  stored is UTC midnight of that day.
 *  Rendering in local time would show a reader at UTC-5 2019-05-01 -- a day off out of nowhere,
 *  and the direction of the error varies with where the reader is. Record time (when we came to
 *  believe it) is a different matter and should be local; see the comment on ymd in
 *  EntityHistory. */
function fmtTime(iso: string | null, precision: string | null): string | null {
  if (!iso) return null;
  const d = new Date(iso);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, "0");
  const day = String(d.getUTCDate()).padStart(2, "0");
  if (precision === "year") return `${y}`;
  if (precision === "month") return `${y}-${m}`;
  return `${y}-${m}-${day}`;
}

function fmtInterval(f: EntityFact): string {
  if (f.temporal === "eternal") return "";
  const from = fmtTime(f.valid_from, f.valid_from_precision);
  const to = fmtTime(f.valid_to, f.valid_to_precision);
  // **"Ended, but which day is unknown" must never be displayed as "to date".** That is the
  // full face of what this change fixes: the source says plainly "former CEO of Weta Digital"
  // while the UI told the reader he was still in post
  const endedUnknown = !f.valid_to && f.valid_to_precision === "unknown";
  if (!from && !to && !endedUnknown) return "";
  const end = to ?? (endedUnknown ? S.graph.endedUnknown : S.graph.ongoing);
  return from ? `${from} ~ ${end}` : `~ ${end}`;
}

/** One derived fact, **with the proof laid out underneath**.
 *
 * No collapsing: the entire reason this tab exists is "nobody stated this edge, this is how it
 * came about", and hiding the premises behind a click hides the reason. The longest chain is
 * twelve steps, so laid out it is not long either. */
/** The small window beside the derived switch: **when and by what this batch of edges was
 *  derived, and whether it is still accurate**.
 *
 * The reason it exists is that "freshness is invisible". Derivation re-runs every hour, while
 * the facts shift with every document that arrives -- a derived edge looks exactly as it did the
 * moment it was derived, yet the premise it rests on may have been retracted three minutes ago.
 * The switch on its own cannot answer "as of when is what I am looking at".
 *
 * The manual button stays here rather than somewhere else: the person who wants to re-run is
 * precisely the one who has just read these three lines and found the numbers too old.
 */
function DerivedPanel({
  panelRef,
  kbId,
  count,
  onClose,
}: {
  panelRef: React.Ref<HTMLDivElement>;
  kbId: string;
  count: number;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const kb = useQuery({
    queryKey: ["kbOne", kbId],
    queryFn: () => api.kbDetail(kbId),
  });
  /* Re-running needs confirming, but **the second press has to land on a different button**.
     This product's gesture convention is "two clicks on the same control = collapse it" -- the
     switch, the ⋯, the legend chips are all used that way. Putting "click again to execute" on
     the same button would mean the same gesture unexpectedly turns into "execute" here, while
     everywhere else it has always been "cancel".
     So one click only **asks a question**, and beneath the question sit two targets: cancel / run.

     The site-wide DangerConfirm is not used either: that is the danger tier, with a red title
     and the option to demand the name typed out, reserved for irreversible things like deleting
     a KB. Re-running inference is heavy but repeatable, which does not reach that tier */
  const [armed, setArmed] = useState(false);
  const run = useMutation({
    mutationFn: () => api.runInference(kbId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["graph"] });
      qc.invalidateQueries({ queryKey: ["kbOne", kbId] });
    },
    onError: (e: Error) => toast.error(e.message),
  });

  const on = kb.data?.materialize_inferences ?? false;
  const last = kb.data?.last_inference_at;
  // "How long ago" reads better than a timestamp -- the question is "is it fresh", not "what
  // time was it"
  const age = last
    ? Math.round((Date.now() - new Date(last).getTime()) / 60000)
    : null;

  // **It covers the trigger's own position and grows up and to the right** (bottom-0 left-0),
  // rather than hanging a window off to one side.
  // Surface and corner radius line up with the notification/user cards: u-menu-glass + rounded-xl
  return (
    <div
      ref={panelRef}
      className="u-menu-glass pointer-events-auto absolute bottom-0 left-0 z-50 w-72 overflow-hidden rounded-xl px-3 pb-3 pt-2.5 shadow-2xl"
    >
      {/* items-center rather than baseline: a button and a close key stand beside the title, and
          aligning on the baseline makes those two look like they drift upward */}
      <div className="flex items-center gap-2">
        <span className="text-[13px] text-neutral-100">
          {S.graph.derivedPanel}
        </span>
        {!armed && (
          <button
            /* **It has to look like a button**: it used to be a stretch of grey ghost text
               wedged between the title and the ×, which read as a third title rather than as an
               action. A border + padding puts it in the same tier of secondary control as the
               step stepper in the top right corner */
            className="ml-auto rounded-md border border-white/10 px-2 py-0.5 text-[11px] text-neutral-400 transition-colors hover:border-white/20 hover:text-neutral-100"
            disabled={!on || run.isPending}
            title={on ? undefined : S.err.inference_off}
            onClick={() => setArmed(true)}
          >
            {run.isPending ? S.graph.derivedRunning : S.graph.derivedRun}
          </button>
        )}
        {/* A fixed 18px square: **do not let the close key set the height of the title row** --
            once it does, the shortest thing in the row, the title, gets centred with gaps above
            and below, which looks like too much top padding */}
        <button
          className={`${armed ? "ml-auto " : ""}grid h-[18px] w-[18px] place-items-center rounded text-neutral-500 transition-colors hover:bg-white/[0.06] hover:text-neutral-200`}
          onClick={onClose}
          aria-label={S.graph.close}
        >
          ×
        </button>
      </div>

      {/* The question + two targets. **Cancel comes first**: moving over from that "run" press,
          the first thing you meet is cancel, and whichever has the smaller cost when hit by
          mistake should be the nearer one */}
      {armed && (
        <div className="mt-2 rounded-lg bg-white/[0.04] p-2">
          <p className="text-[11px] leading-relaxed text-neutral-300">
            {S.graph.derivedRunAsk}
          </p>
          <div className="mt-1.5 flex gap-1.5">
            <button
              className="rounded px-2 py-0.5 text-[11px] text-neutral-400 transition-colors hover:bg-white/[0.06] hover:text-neutral-100"
              onClick={() => setArmed(false)}
            >
              {S.graph.derivedRunCancel}
            </button>
            <button
              className="rounded bg-white/10 px-2 py-0.5 text-[11px] text-neutral-100 transition-colors hover:bg-white/[0.16]"
              disabled={run.isPending}
              onClick={() => {
                setArmed(false);
                run.mutate();
              }}
            >
              {S.graph.derivedRunGo}
            </button>
          </div>
        </div>
      )}

      <dl className="mt-2 space-y-1 text-[11px]">
        <div className="flex justify-between gap-3">
          <dt className="text-neutral-500">{S.graph.derivedCountLabel}</dt>
          <dd className="u-num text-neutral-200">{count}</dd>
        </div>
        <div className="flex justify-between gap-3">
          <dt className="text-neutral-500">{S.graph.derivedStateLabel}</dt>
          <dd className={on ? "text-neutral-200" : "text-[var(--u-warn)]"}>
            {on
              ? S.graph.derivedOn(kb.data!.inference_interval_minutes)
              : S.graph.derivedOff}
          </dd>
        </div>
        <div className="flex justify-between gap-3">
          <dt className="text-neutral-500">{S.graph.derivedLastLabel}</dt>
          <dd className="u-num text-neutral-200">
            {age === null ? S.graph.derivedNever : S.graph.derivedAgo(age)}
          </dd>
        </div>
      </dl>

      {/* The result of the last manual run stays here. **How many were derived and how many
          were invalidated have to be said separately** -- "nothing changed" and "thirty got
          replaced" are two very different things */}
      {run.data && (
        <p className="mt-2 text-[11px] text-neutral-400">
          {run.data.inserted === 0 && run.data.invalidated === 0
            ? S.graph.derivedNoChange
            : S.graph.derivedChanged(run.data.inserted, run.data.invalidated)}
          {run.data.capped > 0 &&
            ` · ${S.graph.derivedCapped(run.data.capped)}`}
        </p>
      )}

    </div>
  );
}

/** One derived edge. **The row styling lines up with FactRow**: the same rounded row, the same
 *  chevron expansion, the same role="link" navigation (avoiding a button inside a button).
 *
 *  This used to be a `glass rounded-xl p-3` card with the proof permanently expanded -- among a
 *  column of compact Relations/Timeline/History rows it looked like something out of a different
 *  product, and a dozen derivations stacked up were a wall. A proof is something you look at
 *  only once you ask for it, so tucking it into an expansion area fits exactly. */
function DerivedRow({
  kbId,
  d,
  otherId,
  otherName,
  open,
  onToggle,
  onNavigate,
}: {
  kbId: string;
  d: DerivedFact;
  otherId: string;
  otherName: string;
  open: boolean;
  onToggle: () => void;
  onNavigate: (entityId: string) => void;
}) {
  return (
    <div
      className={`rounded-lg transition-colors ${open ? "bg-white/[0.05]" : "hover:bg-white/[0.04]"}`}
    >
      <button
        onClick={onToggle}
        className="w-full text-left px-2 py-1.5 flex items-center gap-1.5"
      >
        <ChevronRight
          size={11}
          className={`shrink-0 text-neutral-600 transition-transform ${open ? "rotate-90" : ""}`}
        />
        <span
          role="link"
          tabIndex={0}
          onClick={(ev) => {
            ev.stopPropagation();
            onNavigate(otherId);
          }}
          onKeyDown={(ev) => {
            if (ev.key === "Enter") {
              ev.stopPropagation();
              onNavigate(otherId);
            }
          }}
          className="truncate text-[13px] text-neutral-200 hover:text-white hover:underline underline-offset-2 decoration-white/30"
        >
          {otherName}
        </span>
        <span className="ml-auto shrink-0 pl-2 text-[10.5px] text-neutral-600">
          {d.premises.length}
        </span>
      </button>
      {/* Proof: the premises in derivation order, each expanded down to the original sentence
          (0002 R2). **The border is of the same tier as EvidenceList** -- the two are two forms
          of the same thing: one gives the source, the other gives the chain of reasoning */}
      {open && <ProofChain kbId={kbId} d={d} />}
    </div>
  );
}

/** One derivation's proof chain. Fetched only on expansion -- a proof is something you look at
 *  only once you ask for it.
 *
 *  Each step is one asserted premise: the triple on top, its original sentence below, and the
 *  sentence clicks through into the document.
 *  Premises that have been retracted are marked but not hidden: the derivation lapses with them,
 *  and "what it rested on at the time" is exactly what the record-time axis has to answer.
 *  If it cannot be fetched (the derivation has already lapsed), fall back to the few lines of
 *  text the list brought along, rather than leaving it blank. */
function ProofChain({ kbId, d }: { kbId: string; d: DerivedFact }) {
  const proof = useQuery({
    queryKey: ["proof", d.id],
    queryFn: () => api.derivedProof(kbId, d.id),
  });
  const steps = proof.data?.proof?.steps;
  return (
    <div className="mx-2 mb-2 mt-0.5 border-l border-white/15 pl-2.5">
      {proof.isPending && (
        <p className="text-[11px] text-neutral-600">{S.graph.proofLoading}</p>
      )}
      {steps && <ProofSteps kbId={kbId} steps={steps} />}
      {/* The derivation has lapsed and the proof cannot be fetched: fall back to the few lines
          of text the list brought along */}
      {!proof.isPending && !steps && (
        <ol className="space-y-0.5">
          {d.premises.map((p, i) => (
            <li key={i} className="text-[11px] text-neutral-400">
              {p}
            </li>
          ))}
          {d.premises.length === 0 && (
            <li className="text-[11px] text-neutral-600">{S.graph.derivedNoProof}</li>
          )}
        </ol>
      )}
    </div>
  );
}

/** Proof steps, shared by derivations that landed and those that did not: a premise is the same
 *  kind of thing either way */
function ProofSteps({ kbId, steps }: { kbId: string; steps: ProofStep[] }) {
  return (
    <ol className="space-y-2">
      {steps.map((st) => (
        <li key={st.fact_id} className="text-[11px]">
          <div className="flex items-baseline gap-1.5 flex-wrap">
            <span className="u-num text-[10px] text-neutral-600 shrink-0">
              {S.graph.proofStep(st.seq + 1)}
            </span>
            <span className={st.retracted ? "text-neutral-600 line-through" : "text-neutral-300"}>
              {st.subject}
              <span className="text-neutral-500"> — {st.predicate ?? "?"} → </span>
              {st.object ?? "?"}
            </span>
            {st.retracted && (
              <span className="u-chip u-chip-warn text-[10px]">{S.graph.proofRetracted}</span>
            )}
          </div>
          <div className="mt-0.5 space-y-1 pl-2">
            {st.evidence.map((ev) => (
              <Link
                key={ev.chunk_id}
                to="/kb/$kbId/doc/$docId"
                params={{ kbId, docId: ev.document_id }}
                search={{ chunk: ev.chunk_id }}
                className="block text-neutral-500 hover:text-neutral-300"
              >
                <div className="line-clamp-2 italic">
                  {ev.quote ? `“${ev.quote}”` : S.graph.noQuote}
                </div>
                <div className="mt-0.5 text-neutral-400">
                  {S.graph.sectionRef(ev.filename, ev.seq + 1)}
                  {ev.stale && (
                    <span
                      className="ml-1.5 u-num text-[10px] text-neutral-600"
                      title={S.graph.staleEvidenceHint}
                    >
                      {S.graph.fromVersion(ev.doc_version)}
                    </span>
                  )}
                </div>
              </Link>
            ))}
            {st.evidence.length === 0 && (
              <p className="text-neutral-600">{S.graph.noEvidence}</p>
            )}
          </div>
        </li>
      ))}
    </ol>
  );
}

/** A derivation that did not land (0017 §3): a row like DerivedRow plus one line of "who is
 *  blocking it", expanding into its proof chain -- this is where a person sees "the engine could
 *  have drawn this edge; here is what stopped it" */
function BlockedRow({
  kbId,
  b,
  entityId,
  open,
  onToggle,
  onNavigate,
}: {
  kbId: string;
  b: BlockedDerivation;
  entityId: string;
  open: boolean;
  onToggle: () => void;
  onNavigate: (entityId: string) => void;
}) {
  const navigate = useNavigate();
  const out = b.subject_id === entityId;
  const otherId = out ? b.object_id : b.subject_id;
  const otherName = out ? b.object : b.subject;
  const proof = useQuery({
    queryKey: ["blocked-proof", b.violation_id],
    queryFn: () => api.blockedProof(kbId, b.violation_id),
    enabled: open,
  });
  return (
    <div
      className={`rounded-lg transition-colors ${open ? "bg-white/[0.05]" : "hover:bg-white/[0.04]"}`}
    >
      <button
        onClick={onToggle}
        className="w-full text-left px-2 py-1.5 flex items-center gap-1.5"
      >
        <ChevronRight
          size={11}
          className={`shrink-0 text-neutral-600 transition-transform ${open ? "rotate-90" : ""}`}
        />
        {out ? <ArrowRight size={10} className="shrink-0 text-neutral-600" /> : <ArrowLeft size={10} className="shrink-0 text-neutral-600" />}
        <span className="shrink-0 text-[11px] text-neutral-500">{b.predicate}</span>
        <span
          role="link"
          tabIndex={0}
          onClick={(ev) => {
            ev.stopPropagation();
            onNavigate(otherId);
          }}
          onKeyDown={(ev) => {
            if (ev.key === "Enter") {
              ev.stopPropagation();
              onNavigate(otherId);
            }
          }}
          className="truncate text-[13px] text-neutral-200 hover:text-white hover:underline underline-offset-2 decoration-white/30"
        >
          {otherName}
        </span>
        <span className="ml-auto shrink-0 pl-2 text-[10.5px] text-neutral-600">
          {S.graph.ruleNames[b.rule] ?? b.rule}
        </span>
      </button>
      <div className="flex items-center gap-1.5 px-2 pb-1.5 pl-[26px] text-[11px]">
        <span className="truncate text-[var(--u-contest)]">
          {S.graph.blockedBy(b.against_text)}
        </span>
        <span
          role="link"
          tabIndex={0}
          onClick={() =>
            navigate({
              to: "/kb/$kbId/review",
              params: { kbId },
              search: { queue: "violations", item: b.violation_id },
            })
          }
          className="ml-auto shrink-0 text-neutral-500 hover:text-neutral-300 hover:underline underline-offset-2"
        >
          {S.graph.blockedReview} →
        </span>
      </div>
      {open && (
        <div className="mx-2 mb-2 mt-0.5 border-l border-white/15 pl-2.5">
          {proof.isPending && (
            <p className="text-[11px] text-neutral-600">{S.graph.proofLoading}</p>
          )}
          {proof.data?.steps && (
            <ProofSteps kbId={kbId} steps={proof.data.steps} />
          )}
        </div>
      )}
    </div>
  );
}

/** Contested chip (0017 §3): an open violation or conflict is pointing at this assertion. The
 *  row is not dimmed -- it is still alive. Clicking it goes to the matching Review tab and
 *  lights that card up */
function ContestedChip({
  kbId,
  c,
}: {
  kbId: string;
  c: NonNullable<EntityFact["contested"]>;
}) {
  const navigate = useNavigate();
  const queue = c.kind === "temporal_conflict" ? "conflicts" : "violations";
  return (
    <span
      role="link"
      tabIndex={0}
      onClick={(ev) => {
        ev.stopPropagation();
        navigate({
          to: "/kb/$kbId/review",
          params: { kbId },
          search: { queue, item: c.ref_id },
        });
      }}
      onKeyDown={(ev) => {
        if (ev.key === "Enter") {
          ev.stopPropagation();
          navigate({
            to: "/kb/$kbId/review",
            params: { kbId },
            search: { queue, item: c.ref_id },
          });
        }
      }}
      className="u-chip u-chip-contest shrink-0 !text-[10px] !px-1.5 cursor-pointer"
      title={S.graph.contestedHint(c.kind, c.derived ?? null)}
    >
      {S.graph.contestedChip}
    </span>
  );
}

function EntityPanel({
  kbId,
  entityId,
  exiting,
  intent,
  onClose,
  onNavigate,
}: {
  kbId: string;
  entityId: string;
  /** Playing the exit animation: still attached to the DOM, but no longer accepting clicks */
  exiting: boolean;
  /** Which tab to land on and which row to expand when opening; cleared once it has been read */
  intent?: MutableRefObject<{ view: "derived"; open: string } | null>;
  onClose: () => void;
  onNavigate: (entityId: string) => void;
}) {
  const detail = useQuery({
    queryKey: ["entity", kbId, entityId],
    queryFn: () => api.entityDetail(kbId, entityId),
  });
  const [openFact, setOpenFact] = useState<string | null>(null);
  // The derived ones. **A key of their own, not mixed into facts** -- in one single list the
  // user cannot see the difference between "written in a document" and "derived by the engine"
  const derived = detail.data?.derived ?? [];
  // The ones that did not land (0017 §3): derived, and then ran into an assertion
  const blocked = detail.data?.blocked ?? [];
  /* Grouped by "direction + predicate + rule", with the same skeleton as Relations' groups.
     The rule hangs on the group rather than on every row: it holds for the whole group, so
     repeating it row by row is redundant, and that little amber label would also compete for
     hue with the derived edges */
  const derivedGroups = useMemo(() => {
    const map = new Map<
      string,
      {
        key: string;
        direction: "in" | "out";
        predicate: string;
        rule: string;
        rows: DerivedFact[];
      }
    >();
    for (const d of derived) {
      const direction = d.subject_id === entityId ? "out" : "in";
      // Each of the four rules has a name. **If the lookup misses, fall back to the raw kind
      // string** -- that means nothing to the reader, but it is more honest than showing another
      // rule's name
      const rule = S.graph.ruleNames[d.rule] ?? d.rule;
      const key = `${direction}|${d.predicate}|${d.rule}`;
      const cur = map.get(key);
      if (cur) cur.rows.push(d);
      else map.set(key, { key, direction, predicate: d.predicate, rule, rows: [d] });
    }
    return [...map.values()];
  }, [derived, entityId]);
  // Relations = grouped by relation (looking relations up); Timeline = the valid-time axis
  // (when things held); History = the record-time axis (when we came to believe it, and when we
  // changed our mind)
  const [view, setView] = useState<
    "relations" | "timeline" | "history" | "derived"
  >("relations");
  useEffect(() => {
    const it = intent?.current;
    if (!it) return;
    intent.current = null;
    setView(it.view);
    setOpenFact(it.open);
  }, [entityId, intent]);

  const e: GraphNode | undefined = detail.data?.entity;

  // Entity correction: extraction gives a first verdict, and until now a wrong verdict could
  // only be fixed by re-extracting the whole KB
  const qc = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [draftType, setDraftType] = useState("");
  // Other entities with the same name: the detail endpoint hands them over as soon as it opens.
  // After a rename, overwrite with the copy from the response -- renaming may run into a fresh
  // batch of same-name entities, and that answer is newer than the one from when it opened
  const [renamedPeers, setRenamedPeers] = useState<GraphNode[] | null>(null);
  const sameName = renamedPeers ?? detail.data?.same_name ?? [];
  const setSameName = setRenamedPeers;
  // Manual merge: fold the same-named one into **this one here**. The direction is hard-coded
  // on purpose -- the one the user is looking at is the one they have judged to be the primary
  const merge = useMutation({
    mutationFn: (source: string) => api.mergeEntities(kbId, source, entityId),
    onSuccess: () => {
      toast.success(S.toast.saved);
      // Drop the merged-away one locally instead of waiting for a refetch -- it no longer
      // exists, and leaving it there invites another click
      setSameName((prev) =>
        (prev ?? sameName).filter((p) => p.id !== merge.variables),
      );
      qc.invalidateQueries({ queryKey: ["entity", kbId, entityId] });
      qc.invalidateQueries({ queryKey: ["graph"] });
      qc.invalidateQueries({ queryKey: ["review", kbId] });
    },
    onError: (err: Error) => toast.error(err.message),
  });
  // The type dropdown wants the full ontology, not just the few that happen to appear in the
  // current view
  const ontology = useQuery({
    queryKey: ["ontology", kbId],
    queryFn: () => api.ontology(kbId),
    enabled: editing,
  });
  const types = ontology.data?.entity_types ?? [];

  const openEdit = () => {
    if (!e) return;
    setDraftName(e.name);
    setDraftType(types.find((t) => t.key === e.type_key)?.id ?? "");
    setSameName([]);
    setEditing(true);
  };
  // The ontology arrives asynchronously: when it is all in, point the type dropdown at the
  // current type
  useEffect(() => {
    if (editing && !draftType && e)
      setDraftType(types.find((t) => t.key === e.type_key)?.id ?? "");
  }, [editing, draftType, e, types]);

  const save = useMutation({
    mutationFn: () => {
      const body: { type_id?: string; canonical_name?: string } = {};
      if (draftName.trim() && draftName.trim() !== e?.name)
        body.canonical_name = draftName.trim();
      const curId = types.find((t) => t.key === e?.type_key)?.id;
      if (draftType && draftType !== curId) body.type_id = draftType;
      return api.updateEntity(kbId, entityId, body);
    },
    onSuccess: (r) => {
      setEditing(false);
      setSameName(r.same_name);
      toast.success(S.graph.editSaved);
      // The type/name changed, so the graph nodes and the ontology counts have to move with it
      qc.invalidateQueries({ queryKey: ["entity", kbId, entityId] });
      qc.invalidateQueries({ queryKey: ["graph", kbId] });
      qc.invalidateQueries({ queryKey: ["ontology", kbId] });
    },
    onError: (err: Error) => toast.error(err.message),
  });

  const dirty =
    !!e &&
    (draftName.trim() !== e.name ||
      draftType !== (types.find((t) => t.key === e.type_key)?.id ?? ""));

  // Relations = the snapshot valid right now (as-of now); closed history only appears in
  // Timeline.
  // Grouped by "direction + predicate": the entity's own name is no longer repeated on every
  // row, and the predicate appears only in the section heading
  const { groups, historicalCount } = useMemo(() => {
    const all = detail.data?.facts ?? [];
    const nowIso = new Date().toISOString();
    const current = all.filter(
      (f) =>
        (!f.valid_from || f.valid_from <= nowIso) &&
        (!f.valid_to || f.valid_to > nowIso),
    );
    const map = new Map<
      string,
      {
        key: string;
        label: string | null;
        inferred: boolean;
        direction: string;
        rows: EntityFact[];
      }
    >();
    for (const f of current) {
      // Facts with an empty predicate go into one group: what they have in common is exactly
      // "we cannot say what the relation is"
      const k = `${f.direction}:${f.predicate_key ?? ""}`;
      if (!map.has(k))
        map.set(k, {
          key: k,
          label: f.predicate_label,
          inferred: f.inferred,
          direction: f.direction,
          rows: [],
        });
      map.get(k)!.rows.push(f);
    }
    const arr = [...map.values()];
    for (const gr of arr)
      gr.rows.sort((a, b) =>
        (a.valid_from ?? "9999") < (b.valid_from ?? "9999") ? -1 : 1,
      );
    arr.sort(
      (a, b) =>
        b.rows.length - a.rows.length ||
        (a.label ?? "").localeCompare(b.label ?? ""),
    );
    return { groups: arr, historicalCount: all.length - current.length };
  }, [detail.data]);

  return (
    <div
      className={`${exiting ? "u-dock-out" : "u-dock-in"} glass-strong absolute top-14 right-3 bottom-20 w-80 z-10 rounded-xl shadow-2xl flex flex-col`}
    >
      <div className="flex items-start justify-between px-4 py-3.5 border-b border-white/10">
        <div>
          {e && (
            <>
              <div className="flex items-center gap-2">
                <span
                  className="h-2.5 w-2.5 rounded-full shrink-0"
                  style={{
                    background: e.color,
                    boxShadow: `0 0 8px ${e.color}55`,
                  }}
                />
                <span
                  className="text-[15px] font-semibold tracking-tight text-white"
                  style={{ fontFamily: "var(--font-display)" }}
                >
                  {e.name}
                </span>
              </div>
              {/* When the disambiguator finds no related fact it falls back to the type label,
                  which then duplicates the type shown after it */}
              <div className="mt-1 text-xs text-neutral-500">
                {e.disambiguator && e.disambiguator !== e.type_label
                  ? `${e.disambiguator} · `
                  : ""}
                {e.type_label ?? S.graph.untyped} ·{" "}
                {detail.data?.facts.length ?? 0} {S.graph.facts}
              </div>
            </>
          )}
        </div>
        <div className="flex items-center gap-1.5 mt-0.5">
          {e && !editing && (
            <button
              onClick={openEdit}
              title={S.graph.edit}
              className="text-neutral-500 hover:text-neutral-200"
            >
              <Pencil size={13} />
            </button>
          )}
          <button
            onClick={onClose}
            className="text-neutral-500 hover:text-neutral-200"
          >
            <X size={15} />
          </button>
        </div>
      </div>

      {editing && e && (
        <div className="px-4 py-3 border-b border-white/10 space-y-2.5">
          <label className="block">
            <span className="text-[10px] uppercase tracking-[0.08em] text-neutral-500">
              {S.graph.editName}
            </span>
            <input
              autoFocus
              value={draftName}
              onChange={(ev) => setDraftName(ev.target.value)}
              onKeyDown={(ev) => {
                if (ev.key === "Enter" && dirty && draftName.trim())
                  save.mutate();
                if (ev.key === "Escape") setEditing(false);
              }}
              className="mt-1 w-full bg-white/[0.04] border border-white/10 rounded px-2 py-1 text-sm text-neutral-100 focus:outline-none focus:border-white/25"
            />
          </label>
          <label className="block">
            <span className="text-[10px] uppercase tracking-[0.08em] text-neutral-500">
              {S.graph.editType}
            </span>
            <select
              value={draftType}
              onChange={(ev) => setDraftType(ev.target.value)}
              className="mt-1 w-full bg-white/[0.04] border border-white/10 rounded px-2 py-1 text-sm text-neutral-100 focus:outline-none focus:border-white/25"
            >
              {types.map((t) => (
                <option key={t.id} value={t.id} className="bg-neutral-900">
                  {t.label}
                </option>
              ))}
            </select>
          </label>
          <div className="flex items-center gap-2 pt-0.5">
            <button
              disabled={!dirty || !draftName.trim() || save.isPending}
              onClick={() => save.mutate()}
              className="u-pop px-2.5 py-1 text-xs rounded bg-white/10 text-neutral-100 hover:bg-white/15 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {S.graph.editSave}
            </button>
            <button
              onClick={() => setEditing(false)}
              className="px-2.5 py-1 text-xs rounded text-neutral-500 hover:text-neutral-300"
            >
              {S.graph.editCancel}
            </button>
            {!draftName.trim() && (
              <span className="text-[11px] text-[var(--u-danger)]">
                {S.graph.editEmptyName}
              </span>
            )}
          </div>
        </div>
      )}

      {/* A shared name is not an error -- two people can both be called Zhang Wei. This only
          points it out; judging whether they are the same one is a person's job */}
      {sameName.length > 0 && !editing && (
        <div className="mx-4 mt-2.5 rounded border border-white/10 bg-white/[0.03] px-2.5 py-2">
          <div className="flex items-start justify-between gap-2">
            <p className="text-[11px] text-neutral-400">
              {S.graph.sameNameNote(sameName.length)}{" "}
              <span className="text-neutral-500">{S.graph.sameNameHint}</span>
            </p>
            <button
              onClick={() => setSameName([])}
              className="text-neutral-600 hover:text-neutral-300 shrink-0"
            >
              <X size={11} />
            </button>
          </div>
          {/* Each same-named entity gets two actions: go and look at it, or fold it in here.
              **The direction is hard-coded to "fold into the current one"** -- a merge has a
              direction (the source disappears, its facts move onto the target), and the one
              currently open is the one the user is looking at and judging */}
          <div className="mt-1.5 space-y-1">
            {sameName.map((p) => (
              <div key={p.id} className="flex items-center gap-1">
                <button
                  onClick={() => onNavigate(p.id)}
                  className="min-w-0 flex-1 truncate text-left text-[11px] px-1.5 py-0.5 rounded bg-white/[0.06] text-neutral-300 hover:bg-white/10"
                >
                  {p.type_label ?? S.graph.untyped}
                  {p.disambiguator && p.disambiguator !== p.type_label
                    ? ` · ${p.disambiguator}`
                    : ""}
                </button>
                <button
                  className="shrink-0 text-[11px] px-1.5 py-0.5 rounded text-neutral-400 hover:bg-white/10 hover:text-neutral-100"
                  disabled={merge.isPending}
                  title={S.graph.mergeIntoHint}
                  onClick={() => {
                    if (confirm(S.graph.mergeConfirm(p.name, e?.name ?? "")))
                      merge.mutate(p.id);
                  }}
                >
                  {S.graph.mergeInto}
                </button>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* View switching: Relations (grouped) | Timeline (chronology) */}
      <div className="px-4 pt-2.5">
        <div className="flex rounded-lg overflow-hidden border border-white/10 w-fit">
          {(["relations", "timeline", "history", "derived"] as const)
            // The derived tab: **it does not appear when there are no derivations**. A KB with
            // inference off should not be shown a tab that is forever empty. The ones that did
            // not land count too -- that is precisely what this tab is there to say
            .filter(
              (v) => v !== "derived" || derived.length > 0 || blocked.length > 0,
            )
            .map((v) => (
              <button
                key={v}
                onClick={() => setView(v)}
                className={`px-3 py-1 text-[11px] transition-colors ${
                  view === v
                    ? "bg-white/10 text-neutral-100"
                    : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300"
                }`}
              >
                {v === "relations"
                  ? S.graph.viewRelations
                  : v === "timeline"
                    ? S.graph.viewTimeline
                    : v === "history"
                      ? S.graph.viewHistory
                      : S.graph.viewDerived}
              </button>
            ))}
        </div>
      </div>

      <div className="u-scroll flex-1 overflow-y-auto px-2 py-2">
        {view === "relations" && historicalCount > 0 && (
          <button
            onClick={() => setView("timeline")}
            className="mx-2 mb-2 mt-0.5 text-[11px] text-neutral-500 hover:text-neutral-300 underline-offset-2 hover:underline"
          >
            {S.graph.historicalNote(historicalCount)}
          </button>
        )}
        {view === "relations" &&
          groups.map((gr) => (
            <div key={gr.key} className="mb-3 last:mb-1">
              <div className="flex items-center gap-1.5 px-2 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-500">
                {gr.direction === "in" ? (
                  <ArrowLeft size={10} />
                ) : (
                  <ArrowRight size={10} />
                )}
                <span
                  className={
                    gr.label === null ? "italic text-neutral-600" : undefined
                  }
                  title={
                    gr.label && gr.inferred
                      ? S.graph.inferredPredicate
                      : undefined
                  }
                >
                  {gr.label ?? S.graph.unknownPredicate}
                </span>
                {gr.rows.length > 1 && (
                  <span className="text-neutral-600">{gr.rows.length}</span>
                )}
              </div>
              <div>
                {gr.rows.map((f) => (
                  <FactRow
                    key={f.id}
                    kbId={kbId}
                    fact={f}
                    open={openFact === f.id}
                    onToggle={() =>
                      setOpenFact(openFact === f.id ? null : f.id)
                    }
                    onNavigate={onNavigate}
                  />
                ))}
              </div>
            </div>
          ))}
        {view === "timeline" && (
          <TimelineView
            kbId={kbId}
            facts={detail.data?.facts ?? []}
            openFact={openFact}
            onToggle={(id) => setOpenFact(openFact === id ? null : id)}
            onNavigate={onNavigate}
          />
        )}
        {view === "history" && (
          <EntityHistory kbId={kbId} entityId={entityId} />
        )}
{view === "derived" && (
          <>
            <p className="px-2 pb-1.5 pt-0.5 text-[11px] leading-relaxed text-neutral-500">
              {S.graph.derivedHint}
            </p>
            {/* **The same skeleton as Relations**: a small heading of direction arrow +
                predicate + count, with compact rows underneath. The rule (transitive/symmetric)
                is folded into the heading -- it holds for the whole group, so hanging it on
                every row is repetition, and that `--u-warn` amber is one more place competing
                for hue with the derived edges */}
            {derivedGroups.map((gr) => (
              <div key={gr.key} className="mb-3 last:mb-1">
                <div className="flex items-center gap-1.5 px-2 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-500">
                  {gr.direction === "in" ? (
                    <ArrowLeft size={10} />
                  ) : (
                    <ArrowRight size={10} />
                  )}
                  <span>{gr.predicate}</span>
                  <span className="text-neutral-600">{gr.rule}</span>
                  {gr.rows.length > 1 && (
                    <span className="ml-auto text-neutral-600">
                      {gr.rows.length}
                    </span>
                  )}
                </div>
                <div>
                  {gr.rows.map((d) => {
                    const out = d.subject_id === entityId;
                    return (
                      <DerivedRow
                        key={d.id}
                        kbId={kbId}
                        d={d}
                        otherId={out ? d.object_id : d.subject_id}
                        otherName={out ? d.object : d.subject}
                        open={openFact === d.id}
                        onToggle={() =>
                          setOpenFact(openFact === d.id ? null : d.id)
                        }
                        onNavigate={onNavigate}
                      />
                    );
                  })}
                </div>
              </div>
            ))}
            {blocked.length > 0 && (
              <div className="mb-3 last:mb-1">
                <div className="flex items-center gap-1.5 px-2 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-[var(--u-contest)]">
                  <span>{S.graph.blockedTitle}</span>
                  <span className="ml-auto text-neutral-600">
                    {blocked.length}
                  </span>
                </div>
                <p className="px-2 pb-1.5 text-[11px] leading-relaxed text-neutral-500">
                  {S.graph.blockedHint}
                </p>
                {blocked.map((b) => (
                  <BlockedRow
                    key={b.violation_id}
                    kbId={kbId}
                    b={b}
                    entityId={entityId}
                    open={openFact === b.violation_id}
                    onToggle={() =>
                      setOpenFact(
                        openFact === b.violation_id ? null : b.violation_id,
                      )
                    }
                    onNavigate={onNavigate}
                  />
                ))}
              </div>
            )}
          </>
        )}
        {view !== "history" &&
          view !== "derived" &&
          detail.data?.facts.length === 0 && (
            <p className="text-sm text-neutral-500 p-2">{S.graph.noFacts}</p>
          )}
      </div>
    </div>
  );
}

/** Chronology view: facts with an interval are laid out along a vertical timeline by their
 *  start point; the timeless ones sink to the bottom under undated. */
function TimelineView({
  kbId,
  facts,
  openFact,
  onToggle,
  onNavigate,
}: {
  kbId: string;
  facts: EntityFact[];
  openFact: string | null;
  onToggle: (id: string) => void;
  onNavigate: (entityId: string) => void;
}) {
  const dated = facts
    .filter((f) => f.temporal !== "eternal" && (f.valid_from || f.valid_to))
    .sort((a, b) =>
      (a.valid_from ?? a.valid_to ?? "") < (b.valid_from ?? b.valid_to ?? "")
        ? -1
        : 1,
    );
  const undated = facts.filter((f) => !dated.includes(f));

  return (
    <div className="px-2 pt-1">
      <div className="relative ml-1.5 border-l border-white/15 pl-3 space-y-0.5">
        {dated.map((f) => (
          <div key={f.id} className="relative">
            <span className="absolute -left-[17.5px] top-2.5 h-2 w-2 rounded-full bg-neutral-600 ring-2 ring-[#0f0f0f]" />
            <TimelineRow
              kbId={kbId}
              fact={f}
              open={openFact === f.id}
              onToggle={() => onToggle(f.id)}
              onNavigate={onNavigate}
            />
          </div>
        ))}
        {dated.length === 0 && (
          <p className="py-2 text-xs text-neutral-500">
            {S.graph.timelineEmpty}
          </p>
        )}
      </div>
      {undated.length > 0 && (
        <div className="mt-3">
          <div className="px-2 pb-1 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-600">
            {S.graph.undated}
          </div>
          {undated.map((f) => (
            <FactRow
              key={f.id}
              kbId={kbId}
              fact={f}
              open={openFact === f.id}
              onToggle={() => onToggle(f.id)}
              onNavigate={onNavigate}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** Chronology entry: the interval + a mark for how it closed + the last confirmation time for
 *  open facts; click to expand the evidence. */
function TimelineRow({
  kbId,
  fact,
  open,
  onToggle,
  onNavigate,
}: {
  kbId: string;
  fact: EntityFact;
  open: boolean;
  onToggle: () => void;
  onNavigate: (entityId: string) => void;
}) {
  const interval = fmtInterval(fact);
  const isOpenEnded = !fact.valid_to;
  const literal = fmtObjectValue(fact.object_value);
  return (
    <div
      className={`rounded-lg transition-colors ${open ? "bg-white/[0.05]" : "hover:bg-white/[0.04]"} ${
        fact.stale ? "opacity-55" : ""
      }`}
      title={fact.stale ? S.graph.staleFactHint : undefined}
    >
      <button onClick={onToggle} className="w-full text-left px-2 py-1.5">
        <div className="flex items-center gap-1.5 u-num text-[10.5px] text-neutral-500">
          {interval || "—"}
          {fact.corrected && (
            <span className="text-neutral-600" title={S.graph.correctedHint}>
              ⟲
            </span>
          )}
          {isOpenEnded && fact.last_evidence_time && (
            <span className="ml-auto text-neutral-600">
              {S.graph.lastConfirmed(fact.last_evidence_time.slice(0, 10))}
            </span>
          )}
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 text-[13px] text-neutral-200">
          <span className="text-neutral-500 text-xs">
            {fact.direction === "in" ? "←" : "→"}{" "}
            <span
              className={
                fact.predicate_label === null
                  ? "italic text-neutral-600"
                  : undefined
              }
              title={
                fact.predicate_label && fact.inferred
                  ? S.graph.inferredPredicate
                  : undefined
              }
            >
              {fact.predicate_label ?? S.graph.unknownPredicate}
            </span>
          </span>
          {fact.other_id ? (
            <span
              role="link"
              tabIndex={0}
              onClick={(ev) => {
                ev.stopPropagation();
                onNavigate(fact.other_id!);
              }}
              onKeyDown={(ev) => {
                if (ev.key === "Enter") {
                  ev.stopPropagation();
                  onNavigate(fact.other_id!);
                }
              }}
              className="truncate hover:text-white hover:underline underline-offset-2 decoration-white/30"
            >
              {fact.other_name ?? "?"}
            </span>
          ) : (
            <span className="truncate">
              {fact.other_name ?? literal ?? "?"}
            </span>
          )}
          {fact.stale && (
            <span className="u-chip u-chip-neutral shrink-0 !text-[10px] !px-1.5">
              {S.graph.staleFactChip}
            </span>
          )}
          {fact.contested && (
            <ContestedChip kbId={kbId} c={fact.contested} />
          )}
        </div>
      </button>
      {open && <EvidenceList kbId={kbId} fact={fact} />}
    </div>
  );
}

/** Display of literal-valued objects: attribute {value,unit} / data-Q&A mapping {summary} /
 *  other JSON as a fallback. */
function fmtObjectValue(v: Record<string, unknown> | null): string | null {
  if (!v) return null;
  if (v.value !== undefined) {
    const val =
      typeof v.value === "boolean" ? (v.value ? "✓" : "✗") : String(v.value);
    return typeof v.unit === "string" && v.unit ? `${val} ${v.unit}` : val;
  }
  if (typeof v.summary === "string") return v.summary;
  return JSON.stringify(v);
}

function FactRow({
  kbId,
  fact,
  open,
  onToggle,
  onNavigate,
}: {
  kbId: string;
  fact: EntityFact;
  open: boolean;
  onToggle: () => void;
  onNavigate: (entityId: string) => void;
}) {
  const interval = fmtInterval(fact);
  // Consistent with Review's low-confidence framing: a chip is attached only when it is low
  // enough to warrant doubt, while ordinary confidence stays silent
  const lowConfidence = fact.confidence < 0.75;

  return (
    <div
      className={`rounded-lg transition-colors ${open ? "bg-white/[0.05]" : "hover:bg-white/[0.04]"} ${
        fact.stale ? "opacity-55" : ""
      }`}
      title={fact.stale ? S.graph.staleFactHint : undefined}
    >
      <button
        onClick={onToggle}
        className="w-full text-left px-2 py-1.5 flex items-center gap-1.5"
      >
        <ChevronRight
          size={11}
          className={`shrink-0 text-neutral-600 transition-transform ${open ? "rotate-90" : ""}`}
        />
        {fact.other_id ? (
          <span
            role="link"
            tabIndex={0}
            onClick={(ev) => {
              ev.stopPropagation();
              onNavigate(fact.other_id!);
            }}
            onKeyDown={(ev) => {
              if (ev.key === "Enter") {
                ev.stopPropagation();
                onNavigate(fact.other_id!);
              }
            }}
            className="truncate text-[13px] text-neutral-200 hover:text-white hover:underline underline-offset-2 decoration-white/30"
          >
            {fact.other_name ?? "?"}
          </span>
        ) : (
          <span className="truncate text-[13px] text-neutral-200">
            {fact.other_name ?? fmtObjectValue(fact.object_value) ?? "?"}
          </span>
        )}
        {lowConfidence && (
          <span className="shrink-0 u-num u-meta-warn text-[10.5px]">
            {Math.round(fact.confidence * 100)}%
          </span>
        )}
        {fact.stale && (
          <span className="u-chip u-chip-neutral shrink-0 !text-[10px] !px-1.5">
            {S.graph.staleFactChip}
          </span>
        )}
        {fact.contested && <ContestedChip kbId={kbId} c={fact.contested} />}
        {interval && (
          <span className="ml-auto shrink-0 pl-2 u-num text-[10.5px] text-neutral-500">
            {interval}
          </span>
        )}
      </button>
      {open && <EvidenceList kbId={kbId} fact={fact} />}
    </div>
  );
}

/** The evidence expansion area (shared by FactRow and TimelineRow): quote + link to the source
 *  text + version badge + confidence. */
function EvidenceList({ kbId, fact }: { kbId: string; fact: EntityFact }) {
  const evidence = useQuery({
    queryKey: ["evidence", fact.id],
    queryFn: () => api.factEvidence(kbId, fact.id),
  });
  return (
    <div className="mx-2 mb-2 mt-0.5 space-y-2 border-l border-white/15 pl-2.5">
      {evidence.data?.evidence.map((ev: Evidence) => (
        <Link
          key={ev.chunk_id}
          to="/kb/$kbId/doc/$docId"
          params={{ kbId, docId: ev.document_id }}
          search={{ chunk: ev.chunk_id }}
          className="block text-xs text-neutral-500 hover:text-neutral-300"
        >
          {/* The predicate as the source text put it, written out only when it differs from what
              the fact row shows. For predicates outside the ontology the fact row already shows
              the source's wording (0052), so writing an identical one out again is noise; this
              only has something to say when one fact has several wordings (3% of them) */}
          {ev.proposed_predicate &&
            ev.proposed_predicate !== fact.predicate_key && (
              <div className="mb-0.5 text-[11px] text-neutral-400">
                {S.graph.proposedPredicate(ev.proposed_predicate)}
              </div>
            )}
          <div className="line-clamp-2 italic">
            {ev.quote ? `“${ev.quote}”` : S.graph.noQuote}
          </div>
          <div className="mt-0.5 text-neutral-400">
            {S.graph.sectionRef(ev.filename, ev.seq + 1)}
            {ev.stale && (
              <span
                className="ml-1.5 u-num text-[10px] text-neutral-600"
                title={S.graph.staleEvidenceHint}
              >
                {S.graph.fromVersion(ev.doc_version)}
              </span>
            )}
          </div>
        </Link>
      ))}
      {evidence.data?.evidence.length === 0 && (
        <p className="text-xs text-neutral-500">{S.graph.noEvidence}</p>
      )}
      {/* Confidence speaks only when it is low enough to be worth doubting (consistent with
          Review's low-confidence framing); ordinarily it is not labelled */}
      {fact.confidence < 0.75 && (
        <p className="text-[10px] text-[var(--u-warn)]">
          {Math.round(fact.confidence * 100)}% {S.graph.confidence}
        </p>
      )}
    </div>
  );
}
