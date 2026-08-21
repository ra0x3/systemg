import { Box, chakra } from "@chakra-ui/react";

const POINTS = 120;
const ARC_STEPS = 20;

type Pt = [number, number];

const sub = (a: Pt, b: Pt): Pt => [a[0] - b[0], a[1] - b[1]];
const add = (a: Pt, b: Pt): Pt => [a[0] + b[0], a[1] + b[1]];
const mul = (a: Pt, k: number): Pt => [a[0] * k, a[1] * k];
const len = (a: Pt) => Math.hypot(a[0], a[1]);
const norm = (a: Pt): Pt => {
  const l = len(a) || 1;
  return [a[0] / l, a[1] / l];
};

function quad(a: Pt, c: Pt, b: Pt, t: number): Pt {
  const u = 1 - t;
  return [u * u * a[0] + 2 * u * t * c[0] + t * t * b[0], u * u * a[1] + 2 * u * t * c[1] + t * t * b[1]];
}

function roundedOutline(vertices: Pt[], radius: number): Pt[] {
  const n = vertices.length;
  const out: Pt[] = [];
  for (let i = 0; i < n; i++) {
    const v = vertices[i];
    const prev = vertices[(i - 1 + n) % n];
    const next = vertices[(i + 1) % n];
    const toPrev = norm(sub(prev, v));
    const toNext = norm(sub(next, v));
    const r = Math.min(radius, len(sub(prev, v)) / 2, len(sub(next, v)) / 2);
    const t1 = add(v, mul(toPrev, r));
    const t2 = add(v, mul(toNext, r));
    out.push(t1);
    for (let s = 1; s <= ARC_STEPS; s++) out.push(quad(t1, v, t2, s / ARC_STEPS));
  }
  return out;
}

function circleOutline(steps: number): Pt[] {
  return Array.from({ length: steps }, (_, i) => {
    const a = (i / steps) * Math.PI * 2 - Math.PI / 2;
    return [0.5 + 0.5 * Math.cos(a), 0.5 + 0.5 * Math.sin(a)] as Pt;
  });
}

function resample(outline: Pt[], count: number): Pt[] {
  const n = outline.length;
  const seg: number[] = [];
  let total = 0;
  for (let i = 0; i < n; i++) {
    const d = len(sub(outline[(i + 1) % n], outline[i]));
    seg.push(d);
    total += d;
  }

  let startIdx = 0;
  let best = Infinity;
  for (let i = 0; i < n; i++) {
    const score = outline[i][1] + Math.abs(outline[i][0] - 0.5) * 0.001;
    if (score < best) {
      best = score;
      startIdx = i;
    }
  }

  const points: Pt[] = [];
  let idx = startIdx;
  let carried = 0;
  const step = total / count;
  for (let k = 0; k < count; k++) {
    let want = step * k - carried;
    while (want > seg[idx]) {
      want -= seg[idx];
      carried += seg[idx];
      idx = (idx + 1) % n;
    }
    const a = outline[idx];
    const b = outline[(idx + 1) % n];
    const u = seg[idx] ? want / seg[idx] : 0;
    points.push([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]);
  }
  return points;
}

function clip(outline: Pt[]) {
  const pts = resample(outline, POINTS);
  return `polygon(${pts.map(([x, y]) => `${(x * 100).toFixed(2)}% ${(y * 100).toFixed(2)}%`).join(", ")})`;
}

const SQUARE: Pt[] = [
  [0, 0],
  [1, 0],
  [1, 1],
  [0, 1],
];

const TRIANGLE: Pt[] = [
  [0.5, 0.035],
  [0.965, 0.85],
  [0.035, 0.85],
];

const RHOMBUS: Pt[] = [
  [0.258, 0.16],
  [0.978, 0.16],
  [0.742, 0.84],
  [0.022, 0.84],
];

function octagonOutline(): Pt[] {
  const k = 1 / Math.cos(Math.PI / 8);
  return Array.from({ length: 8 }, (_, i) => {
    const a = -Math.PI / 2 + Math.PI / 8 + (i * Math.PI) / 4;
    return [0.5 + 0.5 * k * Math.cos(a), 0.5 + 0.5 * k * Math.sin(a)] as Pt;
  });
}

const DIAMOND: Pt[] = [
  [0.5, 0],
  [1, 0.5],
  [0.5, 1],
  [0, 0.5],
];

const SHAPES = {
  square: clip(roundedOutline(SQUARE, 0.214)),
  rhombus: clip(roundedOutline(RHOMBUS, 0.11)),
  triangle: clip(roundedOutline(TRIANGLE, 0.13)),
  octagon: clip(roundedOutline(octagonOutline(), 0.07)),
  circle: clip(circleOutline(240)),
  diamond: clip(roundedOutline(DIAMOND, 0.1)),
};

const KEYFRAMES = `@keyframes sysg-morph {
  0%, 8% { clip-path: ${SHAPES.square}; }
  16%, 24% { clip-path: ${SHAPES.rhombus}; }
  33%, 41% { clip-path: ${SHAPES.triangle}; }
  50%, 58% { clip-path: ${SHAPES.octagon}; }
  66%, 74% { clip-path: ${SHAPES.circle}; }
  83%, 91% { clip-path: ${SHAPES.diamond}; }
  100% { clip-path: ${SHAPES.square}; }
}

@keyframes sysg-morph-spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}`;

export function MorphLogo({ size = 132 }: { size?: number }) {
  return (
    <>
      <chakra.style>{KEYFRAMES}</chakra.style>
      <Box
        aria-hidden="true"
        width={`${size}px`}
        height={`${size}px`}
        bg="accent.500"
        css={{
          clipPath: SHAPES.square,
          animation: "sysg-morph 14s cubic-bezier(0.65, 0, 0.35, 1) infinite, sysg-morph-spin 26.8s linear infinite",
          "@media (prefers-reduced-motion: reduce)": { animation: "none" },
        }}
      />
    </>
  );
}
