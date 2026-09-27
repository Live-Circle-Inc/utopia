/* Login page background: megastructure transformations (Canvas 2D, zero dependencies).
   Four colossal forms -- planet (sphere) → ring city (torus) → city plain (undulating grid) →
   undulating monolith (vertical mega-wall). The structure keeps rotating slowly the whole time
   (motion itself is cheap); a form switch does not morph point by point by interpolation (that
   is exactly where the stutter comes from), it fades the whole thing out → swaps the form →
   fades it back in, with a touch of small-to-large growth as it emerges. Perspective
   projection, structure bigger than the viewport (a colossus ought to run off the frame).
   Performance: form geometry is recomputed into TypedArrays only on a switch; points are
   bucketed by brightness, one fill per bucket; DPR capped at 1.5. Pure neutral white-grey;
   a single static frame under prefers-reduced-motion. */
import { useEffect, useRef } from "react";

const U = 48; // longitudinal point count
const V = 26; // latitudinal point count
const N = U * V;
const HOLD_MS = 7000; // hold at full visibility
const FADE_MS = 1400; // duration of each fade-out / fade-in
const ROT_SPEED = 0.000024; // radians/ms (~260s per revolution; a colossus ought to be slow)
const BUCKETS = 12; // number of point-brightness buckets

type Vec3 = [number, number, number];

/** Form family: each function maps (u,v)∈[0,1) into ~[-1,1]³ */
const FORMS: ((u: number, v: number) => Vec3)[] = [
  // Planet: sphere
  (u, v) => {
    const lon = u * Math.PI * 2;
    const lat = (v - 0.5) * Math.PI * 0.92;
    return [Math.cos(lat) * Math.cos(lon), Math.sin(lat) * 0.95, Math.cos(lat) * Math.sin(lon)];
  },
  // Ring city: torus
  (u, v) => {
    const a = u * Math.PI * 2;
    const b = v * Math.PI * 2;
    const R = 0.74;
    const r = 0.32;
    return [
      (R + r * Math.cos(b)) * Math.cos(a),
      r * Math.sin(b) * 1.05,
      (R + r * Math.cos(b)) * Math.sin(a),
    ];
  },
  // City plain: undulating grid
  (u, v) => {
    const x = (u - 0.5) * 2.5;
    const z = (v - 0.5) * 2.5;
    const y = Math.sin(x * 2.3) * 0.14 + Math.cos(z * 2.1 + x * 1.2) * 0.12 - 0.15;
    return [x, y, z];
  },
  // Undulating monolith: vertical mega-wall
  (u, v) => {
    const x = (u - 0.5) * 2.3;
    const y = (v - 0.5) * 1.5;
    const z = Math.sin(x * 2.8 + y * 1.6) * 0.22;
    return [x, y, z];
  },
];

function smoothstep(t: number): number {
  return t * t * (3 - 2 * t);
}

export function LoginScene({ leaving }: { leaving?: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    // Parametric coordinates of the point lattice
    const pu = new Float32Array(N);
    const pv = new Float32Array(N);
    for (let j = 0; j < V; j++)
      for (let i = 0; i < U; i++) {
        const k = j * U + i;
        pu[k] = i / U;
        pv[k] = j / (V - 1);
      }
    // Grid adjacency edges, stored flat
    const edgeIdx: number[] = [];
    for (let j = 0; j < V; j++)
      for (let i = 0; i < U; i++) {
        const a = j * U + i;
        if (i < U - 1) edgeIdx.push(a, a + 1);
        if (j < V - 1) edgeIdx.push(a, a + U);
      }
    const edges = new Int32Array(edgeIdx);
    // Edges wrapping u's last column back to its first: drawn only on the forms that close in
    // u (sphere/torus) -- on the flattened forms (plain/monolith) they would cut straight
    // across the entire frame
    const wrapIdx: number[] = [];
    for (let j = 0; j < V; j++) wrapIdx.push(j * U + U - 1, j * U);
    const wrapEdges = new Int32Array(wrapIdx);
    const U_CLOSED = [true, true, false, false]; // one-to-one with FORMS

    // Current form geometry: recomputed only on a switch, zero allocation inside a frame
    const fx = new Float32Array(N), fy = new Float32Array(N), fz = new Float32Array(N);
    let cachedForm = -1;
    const fillForm = (idx: number) => {
      const map = FORMS[idx];
      for (let k = 0; k < N; k++) {
        const P = map(pu[k], pv[k]);
        fx[k] = P[0];
        fy[k] = P[1];
        fz[k] = P[2];
      }
      cachedForm = idx;
    };

    const px = new Float32Array(N);
    const py = new Float32Array(N);
    const pa = new Float32Array(N); // depth → brightness 0..1

    // Brightness buckets: style strings pre-generated, no string concatenation inside a frame
    const bucketStyle: string[] = [];
    for (let b = 0; b < BUCKETS; b++)
      bucketStyle.push(`rgba(255,255,255,${(0.1 + (b / (BUCKETS - 1)) * 0.34).toFixed(3)})`);
    const buckets: number[][] = Array.from({ length: BUCKETS }, () => []);

    // In-place twinkle: toned down into an ambience layer (the main show is the light pulses
    // below)
    const TWINKLES = 40;
    const twIdx = new Int32Array(TWINKLES);
    const twPhase = new Float32Array(TWINKLES);
    const twSpeed = new Float32Array(TWINKLES);
    for (let n = 0; n < TWINKLES; n++) {
      twIdx[n] = (n * 1013 + 389) % N; // deterministic pseudo-random scatter
      twPhase[n] = ((n * 7919) % 628) / 100; // 0..2π
      twSpeed[n] = 0.0008 + ((n * 271) % 100) / 100 * 0.0016; // radians/ms
    }

    // Light pulses: lighting up from one vertex along an edge toward another, dragging a fading
    // trail behind them (a signal conducting across the megastructure)
    const PULSES = 12;
    const TRAIL_MAX = 5; // nodes kept in the trail
    const TRAIL_LEN = 3.2; // visible trail length (in edges)
    type Pulse = {
      trail: number[]; // nodes already passed, old → new
      next: number; // the node it is currently lighting toward
      t: number; // progress along the current edge, 0..1
      speed: number; // edges/ms
      edgesLeft: number;
      fade: number; // birth/death envelope
      dying: boolean;
      delay: number; // respawn countdown (ms)
    };
    const nbuf: number[] = [];
    const neighborsOf = (k: number) => {
      nbuf.length = 0;
      const i = k % U;
      const j = (k / U) | 0;
      if (i > 0) nbuf.push(k - 1);
      if (i < U - 1) nbuf.push(k + 1);
      if (j > 0) nbuf.push(k - U);
      if (j < V - 1) nbuf.push(k + U);
    };
    const spawnPulse = (p: Pulse, first: boolean) => {
      const k = (Math.random() * N) | 0;
      neighborsOf(k);
      p.trail = [k];
      p.next = nbuf[(Math.random() * nbuf.length) | 0];
      p.t = 0;
      p.speed = (1.6 + Math.random() * 1.8) / 1000;
      p.edgesLeft = 5 + ((Math.random() * 8) | 0);
      p.fade = 0;
      p.dying = false;
      p.delay = first ? Math.random() * 4000 : 600 + Math.random() * 3500;
    };
    const pulses: Pulse[] = [];
    for (let n = 0; n < PULSES; n++) {
      const p = {} as Pulse;
      spawnPulse(p, true);
      pulses.push(p);
    }

    let raf = 0;
    let w = 0;
    let h = 0;
    const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
    const resize = () => {
      w = canvas.clientWidth;
      h = canvas.clientHeight;
      canvas.width = Math.max(1, Math.round(w * dpr));
      canvas.height = Math.max(1, Math.round(h * dpr));
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(canvas);

    let lastNow = -1;
    const draw = (now: number) => {
      const dt = lastNow < 0 ? 16 : Math.min(50, now - lastNow);
      lastNow = now;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      // Advance the light pulses (unrelated to scene visibility; they keep moving during a fade)
      for (const p of pulses) {
        if (p.delay > 0) {
          p.delay -= dt;
          continue;
        }
        if (p.dying) {
          p.fade -= dt / 350;
          if (p.fade <= 0) spawnPulse(p, false);
          continue;
        }
        p.fade = Math.min(1, p.fade + dt / 350);
        p.t += p.speed * dt;
        while (p.t >= 1) {
          p.t -= 1;
          const prev = p.trail[p.trail.length - 1];
          const cur = p.next;
          p.trail.push(cur);
          if (p.trail.length > TRAIL_MAX) p.trail.shift();
          if (--p.edgesLeft <= 0) {
            p.dying = true;
            break;
          }
          // Pick the next edge: prefer going straight (that is what reads as a signal), never
          // double back
          const straight = cur + (cur - prev);
          neighborsOf(cur);
          if (nbuf.includes(straight) && Math.random() < 0.72) {
            p.next = straight;
          } else {
            let pick = prev;
            let seen = 0;
            for (const cand of nbuf) {
              if (cand === prev) continue;
              seen++;
              if (Math.random() < 1 / seen) pick = cand;
            }
            p.next = pick;
          }
        }
      }

      // Timeline: fade in → hold → fade out, with the form switched at the boundary (while it
      // is invisible)
      const cycle = HOLD_MS + FADE_MS * 2;
      const tt = now % (FORMS.length * cycle);
      const slot = Math.floor(tt / cycle);
      const tc = tt - slot * cycle;
      if (slot !== cachedForm) fillForm(slot);
      let vis: number;
      if (tc < FADE_MS) vis = smoothstep(tc / FADE_MS);
      else if (tc < FADE_MS + HOLD_MS) vis = 1;
      else vis = 1 - smoothstep((tc - FADE_MS - HOLD_MS) / FADE_MS);
      if (vis <= 0.004) {
        if (!reduced) raf = requestAnimationFrame(draw);
        return;
      }

      const ry = now * ROT_SPEED;
      const sinY = Math.sin(ry);
      const cosY = Math.cos(ry);
      const sinX = Math.sin(0.4);
      const cosX = Math.cos(0.4);

      // Colossal scale: bigger than the viewport; a touch of growth as it emerges
      const scale = Math.max(w, h) * 0.62 * (0.96 + 0.04 * vis);
      const cx = w * 0.5;
      const cy = h * 0.66;

      for (let k = 0; k < N; k++) {
        let x = fx[k];
        let y = fy[k];
        let z = fz[k];
        const x1 = x * cosY + z * sinY;
        const z1 = -x * sinY + z * cosY;
        const y1 = y * cosX - z1 * sinX;
        const z2 = y * sinX + z1 * cosX;
        x = x1;
        y = y1;
        z = z2;
        const persp = 2.6 / (2.6 - z * 0.9);
        px[k] = cx + x * scale * persp;
        py[k] = cy - y * scale * persp;
        pa[k] = Math.min(1, Math.max(0, (z + 1.15) / 2.1)); // nearer is brighter
      }

      // Overall visibility turns exactly one knob: globalAlpha
      ctx.globalAlpha = vis;

      // Edges: a very faint white, one single stroke
      ctx.lineWidth = 1;
      ctx.strokeStyle = "rgba(255,255,255,0.045)";
      ctx.beginPath();
      for (let e = 0; e < edges.length; e += 2) {
        const a = edges[e];
        const b = edges[e + 1];
        ctx.moveTo(px[a], py[a]);
        ctx.lineTo(px[b], py[b]);
      }
      if (U_CLOSED[slot]) {
        for (let e = 0; e < wrapEdges.length; e += 2) {
          const a = wrapEdges[e];
          const b = wrapEdges[e + 1];
          ctx.moveTo(px[a], py[a]);
          ctx.lineTo(px[b], py[b]);
        }
      }
      ctx.stroke();

      // Points: bucketed by brightness, one fill per bucket (rects; at 1~2px they are
      // indistinguishable from circles)
      for (let b = 0; b < BUCKETS; b++) buckets[b].length = 0;
      for (let k = 0; k < N; k++) {
        const b = Math.min(BUCKETS - 1, (pa[k] * BUCKETS) | 0);
        buckets[b].push(k);
      }
      for (let b = 0; b < BUCKETS; b++) {
        const list = buckets[b];
        if (!list.length) continue;
        ctx.fillStyle = bucketStyle[b];
        ctx.beginPath();
        for (let n = 0; n < list.length; n++) {
          const k = list[n];
          const r = 0.8 + pa[k] * 1.1;
          ctx.rect(px[k] - r, py[k] - r, r * 2, r * 2);
        }
        ctx.fill();
      }

      // Ambience twinkle: a spiked pulse (sin⁶), toned-down version
      ctx.fillStyle = "rgba(255,255,255,0.92)";
      for (let n = 0; n < TWINKLES; n++) {
        const s = Math.sin(now * twSpeed[n] + twPhase[n]);
        if (s <= 0) continue;
        const glint = s * s * s * s * s * s;
        if (glint < 0.02) continue;
        const k = twIdx[n];
        ctx.globalAlpha = vis * glint * (0.35 + pa[k] * 0.65) * 0.5;
        const r = 0.9 + glint * 1.3;
        ctx.beginPath();
        ctx.arc(px[k], py[k], r, 0, Math.PI * 2);
        ctx.fill();
      }

      // Light pulses: a bright head plus a trail fading along the edges already travelled
      ctx.strokeStyle = "#ffffff";
      ctx.fillStyle = "#ffffff";
      ctx.lineWidth = 1.2;
      for (const p of pulses) {
        if (p.delay > 0 || p.fade <= 0) continue;
        const tail = p.trail;
        const cur = tail[tail.length - 1];
        const hx = px[cur] + (px[p.next] - px[cur]) * p.t;
        const hy = py[cur] + (py[p.next] - py[cur]) * p.t;
        const base = vis * p.fade * (0.35 + pa[cur] * 0.65);

        // Trail: draw the half-segment from cur → the head first, then walk back segment by
        // segment, fading by the edge distance from the head
        let x2 = hx;
        let y2 = hy;
        let dist = 0; // edge distance from the head to the segment midpoint
        for (let s = tail.length - 1; s >= 0; s--) {
          const k = tail[s];
          const segLen = s === tail.length - 1 ? p.t : 1;
          const a = Math.max(0, 1 - (dist + segLen / 2) / TRAIL_LEN);
          if (a <= 0.01) break;
          ctx.globalAlpha = base * a * 0.55;
          ctx.beginPath();
          ctx.moveTo(px[k], py[k]);
          ctx.lineTo(x2, y2);
          ctx.stroke();
          x2 = px[k];
          y2 = py[k];
          dist += segLen;
        }

        // Head: glow plus bright core
        ctx.globalAlpha = base * 0.16;
        ctx.beginPath();
        ctx.arc(hx, hy, 3.8, 0, Math.PI * 2);
        ctx.fill();
        ctx.globalAlpha = base * 0.95;
        ctx.beginPath();
        ctx.arc(hx, hy, 1.5, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.globalAlpha = 1;

      if (!reduced) raf = requestAnimationFrame(draw);
    };

    raf = requestAnimationFrame(draw);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className={`pointer-events-none fixed inset-0 h-full w-full ${
        leaving ? "u-scene-depart" : ""
      }`}
      aria-hidden
    />
  );
}
