import { Box, chakra, Flex, Stack, Text } from "@chakra-ui/react";
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";

/* ------------------------------------------------------------------ */
/* replay harness: autoplay once on scroll, then ▶ / ↻ like bun's      */
/* ------------------------------------------------------------------ */

/**
 * Fires when the element reaches the MIDDLE of the viewport, not when it first
 * peeks in from the bottom — so a figure has finished its animation by the time
 * the reader is actually looking at it, rather than playing off-screen.
 *
 * `rootMargin` collapses the observation area to a thin band across the centre;
 * an element only intersects it once it crosses that line. Anything already on
 * screen at load fires immediately, so above-the-fold content is not stuck
 * waiting for a scroll that may never come.
 */
export function useInView<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [seen, setSeen] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setSeen(true);
      return;
    }
    // Already in the reader's view when the page loaded.
    const rect = el.getBoundingClientRect();
    if (rect.top < window.innerHeight * 0.72 && rect.bottom > 0) {
      setSeen(true);
      return;
    }
    const io = new IntersectionObserver((es) => es.some((e) => e.isIntersecting) && setSeen(true), {
      rootMargin: "-42% 0px -42% 0px",
      threshold: 0,
    });
    io.observe(el);
    return () => io.disconnect();
  }, []);
  return { ref, seen };
}

/**
 * Drives a 0→1 progress value over `ms`, restartable. Children read `t` and
 * draw themselves, so every visual shares one clock and one control.
 */
export function Replay({ ms, label, children }: { ms: number; label?: string; children: (t: number) => ReactNode }) {
  const { ref, seen } = useInView<HTMLDivElement>();
  // Starts at the FINAL frame so the server-rendered HTML carries every number.
  // A crawler, or a reader with JS off, gets the complete chart. On the client
  // we rewind to 0 and play once the figure scrolls into view.
  const [t, setT] = useState(1);
  const [playing, setPlaying] = useState(false);
  const [armed, setArmed] = useState(false);
  const raf = useRef<number | null>(null);
  const started = useRef(false);

  const run = useCallback(() => {
    if (raf.current) cancelAnimationFrame(raf.current);
    const t0 = performance.now();
    setPlaying(true);
    const step = (now: number) => {
      const p = Math.min(1, (now - t0) / ms);
      setT(p);
      if (p < 1) raf.current = requestAnimationFrame(step);
      else setPlaying(false);
    };
    raf.current = requestAnimationFrame(step);
  }, [ms]);

  // Rewind only after hydration, and only if motion is welcome.
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    setT(0);
    setArmed(true);
  }, []);

  useEffect(() => {
    if (armed && seen && !started.current) {
      started.current = true;
      run();
    }
  }, [armed, seen, run]);

  useEffect(() => () => void (raf.current && cancelAnimationFrame(raf.current)), []);

  return (
    <Box ref={ref}>
      {children(t)}
      <Flex mt="14px" align="center" gap="10px" css={{ zoom: "calc(1 / var(--fit-scale, 1))" }}>
        <Box
          as="button"
          onClick={run}
          fontFamily="mono"
          fontSize="15px"
          letterSpacing="0.04em"
          color={playing ? "text.faint" : "accent.500"}
          border="1px solid"
          borderColor="border.control"
          borderRadius="sm"
          px="10px"
          py="4px"
          _hover={{ borderColor: "accent.500" }}
        >
          {playing ? "◼ playing" : started.current ? "↻ replay" : "▶ play"}
        </Box>
        {label ? (
          <Text fontFamily="mono" fontSize="15px" color="text.faint">
            {label}
          </Text>
        ) : null}
      </Flex>
    </Box>
  );
}

const ease = (x: number) => 1 - (1 - x) ** 3;

/* ------------------------------------------------------------------ */
/* 1. Gantt — units starting over a real time axis, with a playhead     */
/* ------------------------------------------------------------------ */

export type GanttUnit = { name: string; start: number; dur: number };

export function Gantt({
  units,
  span,
  t,
  accent,
  total,
}: {
  units: GanttUnit[];
  span: number;
  t: number;
  accent?: boolean;
  total: string;
}) {
  const now = t * span;
  const ticks = Array.from({ length: Math.floor(span / 2) + 1 }, (_, i) => i * 2);
  return (
    <Box>
      <Flex justify="space-between" align="baseline" mb="10px">
        <Text fontFamily="mono" fontSize="15px" color="text.faint">
          {now.toFixed(2)}s
        </Text>
        <Text fontFamily="mono" fontSize="16.5px" fontWeight="bold" color={accent ? "accent.500" : "text.secondary"}>
          {total}
        </Text>
      </Flex>
      <Box position="relative">
        <Stack gap="4px">
          {units.map((u) => {
            const on = now >= u.start;
            const done = now >= u.start + u.dur;
            const grow = on ? Math.min(1, (now - u.start) / u.dur) : 0;
            return (
              <Flex key={u.name} align="center" gap="10px">
                <Text
                  fontFamily="mono"
                  fontSize="14px"
                  width="78px"
                  flexShrink={0}
                  color={done ? "text.secondary" : "text.faint"}
                  transition="color 200ms"
                >
                  {u.name}
                </Text>
                <Box position="relative" height="12px" flex="1" bg="surface.track" borderRadius="2px">
                  <Box
                    position="absolute"
                    top="0"
                    height="100%"
                    borderRadius="2px"
                    left={`${(u.start / span) * 100}%`}
                    width={`${((u.dur * grow) / span) * 100}%`}
                    background={accent ? "var(--accent-bar)" : "channel.neutral"}
                  />
                  {done ? (
                    <Box
                      position="absolute"
                      top="50%"
                      transform="translateY(-50%)"
                      left={`calc(${((u.start + u.dur) / span) * 100}% + 6px)`}
                      fontSize="19.5px"
                      fontFamily="mono"
                      color={accent ? "accent.500" : "text.faint"}
                    >
                      ✓
                    </Box>
                  ) : null}
                </Box>
              </Flex>
            );
          })}
        </Stack>
        {/* playhead */}
        <Box
          position="absolute"
          top="-4px"
          bottom="-4px"
          left={`calc(${(t * 100).toFixed(3)}% * ${1} )`}
          width="1px"
          bg="accent.500"
          opacity={t > 0 && t < 1 ? 0.55 : 0}
          ml="88px"
          pointerEvents="none"
        />
      </Box>
      <Flex mt="8px" ml="88px" justify="space-between">
        {ticks.map((s) => (
          <Text key={s} fontFamily="mono" fontSize="13px" color="text.faint">
            {s}s
          </Text>
        ))}
      </Flex>
    </Box>
  );
}

/* ------------------------------------------------------------------ */
/* 2. Blocks — proportional area, for numbers orders of magnitude apart */
/* ------------------------------------------------------------------ */

export function Blocks({
  items,
  t,
  unit,
}: {
  items: { label: string; value: number; display: string; subject?: boolean }[];
  t: number;
  unit: string;
}) {
  const max = Math.max(...items.map((i) => i.value));
  const MAX_PX = 300;
  const MIN_PX = 30;
  return (
    <Flex gap="44px" align="flex-end" wrap="wrap" justify="space-between">
      {items.map((it, i) => {
        const scale = Math.sqrt(it.value / max);
        const size = MIN_PX + scale * (MAX_PX - MIN_PX);
        const p = ease(Math.max(0, Math.min(1, t * 1.6 - i * 0.18)));
        return (
          <Box key={it.label}>
            <Flex height={`${MAX_PX + 6}px`} align="flex-end">
              <Box
                width={`${size}px`}
                height={`${size * p}px`}
                background={it.subject ? "var(--accent-bar)" : "channel.neutral"}
                borderRadius="3px"
                opacity={0.25 + 0.75 * p}
              />
            </Flex>
            <Text mt="10px" fontFamily="mono" fontSize="14.5px" color="text.muted">
              {it.label}
            </Text>
            <Text
              fontFamily="mono"
              fontSize="15px"
              fontWeight="bold"
              color={it.subject ? "accent.500" : "text.heading"}
            >
              {p > 0.6 ? it.display : " "}
            </Text>
          </Box>
        );
      })}
      <Text fontFamily="mono" fontSize="14px" color="text.faint" pb="4px">
        area ∝ {unit}
      </Text>
    </Flex>
  );
}

/* ------------------------------------------------------------------ */
/* 3. LineChart — two fitted lines that cross                          */
/* ------------------------------------------------------------------ */

export function LineChart({
  series,
  t,
  xMax,
  yMax,
  xLabel,
  yLabel,
  cross,
}: {
  series: { name: string; intercept: number; slope: number; accent?: boolean; dash?: boolean }[];
  t: number;
  xMax: number;
  yMax: number;
  xLabel: string;
  yLabel: string;
  cross?: { x: number; note: string };
}) {
  // Geometry lives in the SVG; every piece of TEXT is HTML on top of it, so
  // labels stay at their real size instead of being scaled up with the plot.
  const PLOT = 236;
  const shown = t * xMax;
  const yTicks = [0, 0.25, 0.5, 0.75, 1].map((f) => Math.round(yMax * f));
  const xTicks = [0, 100, 200, 300, 400, 500].filter((x) => x <= xMax);

  const xPct = (x: number) => (x / xMax) * 100;
  const yPct = (y: number) => (1 - y / yMax) * 100;

  // Where each series leaves the top of the plot, so a steep line stops there.
  const geom = series.map((s) => {
    const exitX = s.slope > 0 ? (yMax - s.intercept) / s.slope : Number.POSITIVE_INFINITY;
    const endX = Math.min(shown, exitX);
    return { ...s, endX, clipped: exitX <= shown, endY: s.intercept + s.slope * Math.min(shown, exitX) };
  });

  const crossY = cross ? series[0].intercept + series[0].slope * cross.x : 0;
  const crossOn = cross ? shown >= cross.x : false;

  return (
    <Box>
      <Flex>
        {/* y axis, HTML so the numbers stay small */}
        <Box position="relative" width="34px" height={`${PLOT}px`} flexShrink={0}>
          {yTicks.map((v) => (
            <Text
              key={v}
              position="absolute"
              right="8px"
              top={`${yPct(v)}%`}
              transform="translateY(-50%)"
              fontFamily="mono"
              fontSize="14px"
              color="text.faint"
            >
              {v}
            </Text>
          ))}
        </Box>

        <Box position="relative" flex="1" height={`${PLOT}px`}>
          <svg
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            width="100%"
            height="100%"
            role="img"
            style={{ display: "block", overflow: "visible" }}
          >
            <title>{yLabel} against services supervised</title>
            {yTicks.map((v) => (
              <line
                key={v}
                x1={0}
                x2={100}
                y1={yPct(v)}
                y2={yPct(v)}
                stroke="currentColor"
                strokeOpacity={0.1}
                strokeWidth={1}
                vectorEffect="non-scaling-stroke"
              />
            ))}
            {geom.map((g) => (
              <line
                key={g.name}
                x1={xPct(0)}
                y1={yPct(g.intercept)}
                x2={xPct(g.endX)}
                y2={yPct(g.endY)}
                stroke={g.accent ? "var(--accent-500)" : "currentColor"}
                strokeOpacity={g.accent ? 1 : 0.42}
                strokeWidth={g.accent ? 2.5 : 2}
                strokeDasharray={g.dash ? "6 5" : undefined}
                strokeLinecap="round"
                vectorEffect="non-scaling-stroke"
              />
            ))}
            {cross && crossOn ? (
              <line
                x1={xPct(cross.x)}
                x2={xPct(cross.x)}
                y1={yPct(crossY)}
                y2={100}
                stroke="var(--accent-500)"
                strokeOpacity={0.3}
                strokeDasharray="3 3"
                strokeWidth={1}
                vectorEffect="non-scaling-stroke"
              />
            ) : null}
          </svg>

          {/* crossover marker + badge, HTML */}
          {cross && crossOn ? (
            <>
              <Box
                position="absolute"
                left={`${xPct(cross.x)}%`}
                top={`${yPct(crossY)}%`}
                transform="translate(-50%, -50%)"
                width="9px"
                height="9px"
                borderRadius="full"
                bg="accent.500"
              />
              <Text
                position="absolute"
                left={`${xPct(cross.x)}%`}
                top={`${yPct(crossY)}%`}
                transform="translate(-50%, -190%)"
                whiteSpace="nowrap"
                fontFamily="mono"
                fontSize="14.5px"
                color="accent.500"
                bg="surface.card"
                px="5px"
              >
                {cross.note}
              </Text>
            </>
          ) : null}

          {/* clip markers where a line leaves the top */}
          {geom
            .filter((g) => g.clipped)
            .map((g) => (
              <Text
                key={`${g.name}-clip`}
                position="absolute"
                left={`${xPct(g.endX)}%`}
                top="0"
                transform="translate(-50%, -110%)"
                fontFamily="mono"
                fontSize="14px"
                color="text.faint"
              >
                ↑
              </Text>
            ))}
        </Box>

        {/* legend replaces end-of-line labels, so nothing can collide */}
        <Stack gap="10px" pl="16px" width="132px" flexShrink={0} justify="center">
          {geom.map((g) => (
            <Box key={`${g.name}-leg`}>
              <Flex align="center" gap="7px">
                <Box
                  width="14px"
                  height="0"
                  borderTop={g.dash ? "2px dashed" : "2px solid"}
                  borderColor={g.accent ? "accent.500" : "text.faint"}
                  opacity={g.accent ? 1 : 0.6}
                  flexShrink={0}
                />
                <Text fontFamily="mono" fontSize="14.5px" color={g.accent ? "accent.500" : "text.muted"}>
                  {g.name}
                </Text>
              </Flex>
              <Text
                ml="21px"
                fontFamily="mono"
                fontSize="16.5px"
                fontWeight="bold"
                color={g.accent ? "accent.500" : "text.secondary"}
              >
                {(g.intercept + g.slope * shown).toFixed(1)}
                <Box as="span" fontSize="13.5px" fontWeight="normal" color="text.faint">
                  {" MB"}
                </Box>
              </Text>
            </Box>
          ))}
        </Stack>
      </Flex>

      {/* x axis */}
      <Flex ml="34px" mr="132px" position="relative" height="18px">
        {xTicks.map((x) => (
          <Text
            key={x}
            position="absolute"
            left={`${xPct(x)}%`}
            transform="translateX(-50%)"
            fontFamily="mono"
            fontSize="14px"
            color="text.faint"
          >
            {x}
          </Text>
        ))}
      </Flex>

      <Flex justify="space-between" mt="6px" ml="34px">
        <Text fontFamily="mono" fontSize="14px" color="text.faint">
          {yLabel}
        </Text>
        <Text fontFamily="mono" fontSize="14px" color="text.faint">
          {xLabel} · N={Math.round(shown)}
        </Text>
      </Flex>
    </Box>
  );
}

/* ------------------------------------------------------------------ */
/* 4. ProcessDots — six processes; watch which ones stay alive          */
/* ------------------------------------------------------------------ */

export function ProcessDots({
  groups,
  t,
}: {
  groups: { label: string; survivors: number; note: string }[];
  t: number;
}) {
  const SLOTS = ["leader", "child", "shell", "shell-kid", "setsid", "setsid-kid"] as const;
  // Timeline within each row: spawn in, hold, then kill one process at a time
  // so teardown reads as a cascade rather than a single frame flip.
  const SPAWN_END = 0.3;
  const KILL_START = 0.42;
  const KILL_STEP = 0.075;
  const KILL_DUR = 0.16;

  return (
    <Stack gap="20px">
      {groups.map((g, gi) => {
        const local = Math.max(0, Math.min(1, (t - gi * 0.05) * 1.18));
        const doomed = SLOTS.length - g.survivors;
        const killEnd = KILL_START + Math.max(0, doomed - 1) * KILL_STEP + KILL_DUR;
        const settled = local >= killEnd;
        // count still standing, fractionally, so the readout eases down
        let standing = 0;
        for (let i = 0; i < SLOTS.length; i++) {
          const order = i - g.survivors;
          if (order < 0) {
            standing += 1;
            continue;
          }
          const dieAt = KILL_START + order * KILL_STEP;
          standing += 1 - Math.max(0, Math.min(1, (local - dieAt) / KILL_DUR));
        }

        return (
          <Box key={g.label}>
            <Flex align="center" gap="12px" wrap="wrap">
              <Text fontFamily="mono" fontSize="15px" color="text.muted" width="160px">
                {g.label}
              </Text>
              <Flex gap="7px">
                {SLOTS.map((slot, i) => {
                  const born = ease(Math.max(0, Math.min(1, (local - i * 0.035) / SPAWN_END)));
                  const survives = i < g.survivors;
                  const order = i - g.survivors;
                  const dieAt = KILL_START + order * KILL_STEP;
                  const dying = survives ? 0 : ease(Math.max(0, Math.min(1, (local - dieAt) / KILL_DUR)));
                  const held = survives && local > KILL_START;
                  return (
                    <Box
                      key={`${g.label}-${slot}`}
                      width="18px"
                      height="18px"
                      borderRadius="4px"
                      border="1px solid"
                      borderColor={held ? "channel.blue" : "border.control"}
                      background={
                        held
                          ? "channel.blue"
                          : survives
                            ? "channel.neutral"
                            : // doomed squares sit darker than the track so the
                              // ones that are about to go are legible before
                              // they drain away
                              "var(--bar-doomed)"
                      }
                      opacity={born * (1 - dying * 0.88)}
                      transform={`scale(${(0.72 + 0.28 * born) * (1 - dying * 0.4)})`}
                      style={{ willChange: "transform, opacity" }}
                    />
                  );
                })}
              </Flex>
              <Text
                fontFamily="mono"
                fontSize="16px"
                fontWeight="bold"
                minW="62px"
                color={g.survivors > 0 ? "channel.blue" : "accent.500"}
                opacity={local > KILL_START ? 1 : 0}
                style={{ fontVariantNumeric: "tabular-nums" }}
              >
                {settled ? (g.survivors === 0 ? "none left" : `${g.survivors} left`) : `${Math.round(standing)} left`}
              </Text>
            </Flex>
            <Text mt="6px" ml="172px" fontFamily="mono" fontSize="14px" color="text.faint">
              {g.note}
            </Text>
          </Box>
        );
      })}
    </Stack>
  );
}

/* ------------------------------------------------------------------ */
/* 5. GapBar — reported-up vs actually-usable, gap shaded               */
/* ------------------------------------------------------------------ */

export function GapBar({
  rows,
  span,
  t,
}: {
  rows: { label: string; reported: number; usable: number; note?: string }[];
  span: number;
  t: number;
}) {
  return (
    <Stack gap="15px">
      {rows.map((r, i) => {
        const p = ease(Math.max(0, Math.min(1, t * 1.5 - i * 0.12)));
        const a = Math.min(r.reported, r.usable);
        const b = Math.max(r.reported, r.usable);
        const early = r.reported < r.usable;
        const gap = b - a;
        return (
          <Box key={r.label}>
            <Flex align="center" gap="10px">
              <Text fontFamily="mono" fontSize="14.5px" color="text.muted" width="148px">
                {r.label}
              </Text>
              <Box position="relative" height="16px" flex="1" bg="surface.track" borderRadius="2px">
                <Box
                  position="absolute"
                  height="100%"
                  left="0"
                  width={`${((r.usable / span) * 100 * p).toFixed(2)}%`}
                  bg="channel.neutral"
                  borderRadius="2px"
                />
                {gap > 0.01 ? (
                  <Box
                    position="absolute"
                    height="100%"
                    left={`${(a / span) * 100}%`}
                    width={`${((gap / span) * 100 * p).toFixed(2)}%`}
                    bg={early ? "channel.blue" : "accent.tint"}
                    opacity={0.9}
                  />
                ) : null}
                <Box
                  position="absolute"
                  top="-3px"
                  bottom="-3px"
                  left={`${(r.reported / span) * 100}%`}
                  width="2px"
                  bg="accent.500"
                  opacity={p}
                />
              </Box>
              <Text
                fontFamily="mono"
                fontSize="15.5px"
                fontWeight="bold"
                width="58px"
                textAlign="right"
                color={gap < 0.01 ? "accent.500" : early ? "channel.blue" : "text.secondary"}
                opacity={p > 0.7 ? 1 : 0}
              >
                {early ? `+${gap.toFixed(2)}s` : gap < 0.01 ? "0.00s" : `−${gap.toFixed(2)}s`}
              </Text>
            </Flex>
            {r.note ? (
              <Text mt="4px" ml="158px" fontFamily="mono" fontSize="14px" color="text.faint">
                {r.note}
              </Text>
            ) : null}
          </Box>
        );
      })}
    </Stack>
  );
}

/* ------------------------------------------------------------------ */
/* 6. DataTable — dense, right-aligned numerics, bold multipliers       */
/* ------------------------------------------------------------------ */

export function DataTable({
  head,
  rows,
  subjectCol = 1,
}: {
  head: string[];
  rows: (string | number)[][];
  subjectCol?: number;
}) {
  return (
    <Box overflowX="auto" mt="4px">
      <Box as="table" width="100%" borderCollapse="collapse" fontFamily="mono" fontSize="15.5px" minW="440px">
        <Box as="thead">
          <Box as="tr">
            {head.map((h, i) => (
              <Box
                key={h}
                as="th"
                textAlign={i === 0 ? "left" : "right"}
                fontWeight="normal"
                color={i === subjectCol ? "accent.500" : "text.faint"}
                pb="9px"
                px="12px"
                borderBottom="1px solid"
                borderColor="border.rule"
                whiteSpace="nowrap"
              >
                {h}
              </Box>
            ))}
          </Box>
        </Box>
        <Box as="tbody">
          {rows.map((r, ri) => (
            <Box as="tr" key={String(r[0])} bg={ri % 2 ? "surface.subtle" : "transparent"}>
              {r.map((c, i) => (
                <Box
                  key={`${String(r[0])}-${head[i]}`}
                  as="td"
                  textAlign={i === 0 ? "left" : "right"}
                  py="10px"
                  px="12px"
                  whiteSpace="nowrap"
                  color={i === 0 ? "text.secondary" : i === subjectCol ? "accent.500" : "text.body"}
                  fontWeight={i === subjectCol ? "bold" : "normal"}
                >
                  {c}
                </Box>
              ))}
            </Box>
          ))}
        </Box>
      </Box>
    </Box>
  );
}

/* ------------------------------------------------------------------ */
/* 7. TreeSwap — before/after process trees for the crash section       */
/* ------------------------------------------------------------------ */

export function TreeSwap({
  tool,
  before,
  after,
  verdict,
  bad,
  t,
}: {
  tool: string;
  before: string[];
  after: string[];
  verdict: string;
  bad?: boolean;
  t: number;
}) {
  const phase = t < 0.34 ? 0 : t < 0.67 ? 1 : 2;
  return (
    <Box border="1px solid" borderColor="border.rule" borderRadius="md" p="16px" bg="surface.card" minW="0" flex="1">
      <Text fontFamily="mono" fontSize="15px" color="text.heading" mb="12px">
        {tool}
      </Text>
      <Stack gap="3px" minH="86px">
        {(phase === 0 ? before : after).map((l, i) => (
          <Text
            key={l}
            fontFamily="mono"
            fontSize="14.5px"
            color={phase === 1 && i === 0 ? "text.faint" : "text.secondary"}
            opacity={phase === 0 ? 1 : phase === 1 ? 0.45 : 1}
            transition="opacity 220ms ease"
            whiteSpace="pre"
          >
            {l}
          </Text>
        ))}
      </Stack>
      <Text
        mt="12px"
        fontFamily="mono"
        fontSize="15px"
        fontWeight="bold"
        color={phase < 2 ? "text.faint" : bad ? "channel.blue" : "accent.500"}
      >
        {phase === 0 ? "running" : phase === 1 ? "kill -9 supervisor…" : verdict}
      </Text>
    </Box>
  );
}

/* ------------------------------------------------------------------ */
/* 8. Race — markers advancing on a shared clock, each stopping at its  */
/*    own finish time. For comparing wall-clock across tools.           */
/* ------------------------------------------------------------------ */

export function Race({
  lanes,
  span,
  t,
  unit = "s",
}: {
  lanes: { label: string; time: number | null; note?: string; subject?: boolean }[];
  span: number;
  t: number;
  unit?: string;
}) {
  const now = t * span;
  return (
    <Stack gap="16px">
      {lanes.map((l) => {
        const done = l.time !== null && now >= l.time;
        const pos = l.time === null ? 0 : Math.min(now, l.time) / span;
        return (
          <Box key={l.label}>
            <Flex align="center" gap="12px">
              <Text
                fontFamily="mono"
                fontSize="15px"
                width="132px"
                flexShrink={0}
                color={l.subject ? "text.heading" : "text.muted"}
              >
                {l.label}
              </Text>
              <Box position="relative" height="22px" flex="1">
                <Box position="absolute" top="10px" left="0" right="0" height="2px" bg="surface.track" />
                {l.time !== null ? (
                  <Box
                    position="absolute"
                    top="10px"
                    left="0"
                    height="2px"
                    width={`${pos * 100}%`}
                    background={l.subject ? "var(--accent-bar)" : "channel.neutral"}
                  />
                ) : null}
                {l.time !== null ? (
                  <Box
                    position="absolute"
                    top="4px"
                    left={`calc(${pos * 100}% - 7px)`}
                    width="14px"
                    height="14px"
                    borderRadius="full"
                    background={l.subject ? "var(--accent-bar)" : "channel.neutral"}
                    boxShadow={done ? "0 0 0 3px var(--surface-card)" : undefined}
                    opacity={l.time === null ? 0 : 1}
                  />
                ) : (
                  <Text
                    position="absolute"
                    top="3px"
                    left="0"
                    fontFamily="mono"
                    fontSize="14.5px"
                    color="text.faint"
                    fontStyle="italic"
                  >
                    cannot express this graph
                  </Text>
                )}
              </Box>
              <Text
                fontFamily="mono"
                fontSize="16px"
                fontWeight="bold"
                width="62px"
                textAlign="right"
                color={l.subject ? "accent.500" : "text.secondary"}
                opacity={l.time === null ? 0 : done ? 1 : 0.45}
              >
                {l.time === null ? "" : `${(done ? l.time : now).toFixed(2)}${unit}`}
              </Text>
            </Flex>
            {l.note ? (
              <Text mt="3px" ml="144px" fontFamily="mono" fontSize="14px" color="text.faint">
                {l.note}
              </Text>
            ) : null}
          </Box>
        );
      })}
    </Stack>
  );
}

/* ------------------------------------------------------------------ */
/* 9. Matrix — yes/no grid whose cells resolve one at a time            */
/* ------------------------------------------------------------------ */

export function Matrix({
  cols,
  rows,
  t,
}: {
  cols: string[];
  rows: { label: string; cells: (boolean | null)[] }[];
  t: number;
}) {
  const total = rows.length * cols.length;
  const revealed = Math.floor(t * total * 1.08);
  return (
    <Box>
      <Flex gap="10px" mb="12px" pl="180px">
        {cols.map((c, ci) => (
          <Text
            key={c}
            fontFamily="mono"
            fontSize="14.5px"
            flex="1"
            textAlign="center"
            color={ci === 0 ? "accent.500" : "text.faint"}
          >
            {c}
          </Text>
        ))}
      </Flex>
      <Stack gap="7px">
        {rows.map((r, ri) => (
          <Flex key={r.label} align="center" gap="10px">
            <Text fontFamily="mono" fontSize="14.5px" width="170px" flexShrink={0} color="text.muted">
              {r.label}
            </Text>
            {r.cells.map((v, ci) => {
              const idx = ri * cols.length + ci;
              const on = revealed > idx;
              const good = v === true;
              return (
                <Flex
                  key={`${r.label}-${cols[ci]}`}
                  flex="1"
                  height="30px"
                  align="center"
                  justify="center"
                  borderRadius="sm"
                  border="1px solid"
                  borderColor={on ? (good ? "verdict.good" : "verdict.bad") : "border.rule"}
                  bg={on ? "surface.subtle" : "transparent"}
                  opacity={on ? 1 : 0.3}
                  transition="all 260ms ease"
                >
                  <Text
                    fontFamily="mono"
                    fontSize="15.5px"
                    fontWeight="bold"
                    color={good ? "verdict.good" : "verdict.bad"}
                    opacity={on ? 1 : 0}
                    transition="opacity 200ms ease"
                  >
                    {good ? "yes" : "no"}
                  </Text>
                </Flex>
              );
            })}
          </Flex>
        ))}
      </Stack>
    </Box>
  );
}

/* ------------------------------------------------------------------ */
/* 10. Meters — paired magnitude bars that count up                     */
/* ------------------------------------------------------------------ */

export function Meters({
  groups,
  t,
}: {
  groups: {
    title: string;
    unit: string;
    rows: { label: string; value: number; display: string; subject?: boolean }[];
  }[];
  t: number;
}) {
  return (
    <Flex gap="44px" wrap="wrap">
      {groups.map((g, gi) => {
        const max = Math.max(...g.rows.map((r) => r.value));
        return (
          <Box key={g.title} flex="1" minW="230px">
            <Text
              fontFamily="mono"
              fontSize="14px"
              letterSpacing="0.08em"
              textTransform="uppercase"
              color="text.faint"
              mb="14px"
            >
              {g.title}
            </Text>
            <Stack gap="13px">
              {g.rows.map((r, ri) => {
                const p = ease(Math.max(0, Math.min(1, t * 1.7 - gi * 0.1 - ri * 0.12)));
                return (
                  <Box key={r.label}>
                    <Flex justify="space-between" align="baseline" mb="5px">
                      <Text fontFamily="mono" fontSize="14.5px" color={r.subject ? "text.heading" : "text.muted"}>
                        {r.label}
                      </Text>
                      <Text
                        fontFamily="mono"
                        fontSize="16px"
                        fontWeight="bold"
                        color={r.subject ? "accent.500" : "text.secondary"}
                      >
                        {p > 0.55 ? r.display : ""}
                      </Text>
                    </Flex>
                    <Box height="7px" bg="surface.track" borderRadius="full" overflow="hidden">
                      <Box
                        height="100%"
                        borderRadius="full"
                        width={`${(r.value / max) * 100 * p}%`}
                        background={r.subject ? "var(--accent-bar)" : "channel.neutral"}
                      />
                    </Box>
                  </Box>
                );
              })}
            </Stack>
            <Text mt="10px" fontFamily="mono" fontSize="13.5px" color="text.faint">
              {g.unit}
            </Text>
          </Box>
        );
      })}
    </Flex>
  );
}

/* ------------------------------------------------------------------ */
/* 11. CountUp — a number that races to its value when scrolled to      */
/* ------------------------------------------------------------------ */

export function CountUp({
  to,
  from = 0,
  decimals = 0,
  ms = 1100,
  delay = 0,
  prefix = "",
  suffix = "",
  onDone,
}: {
  to: number;
  /** Start value. Counting DOWN is the honest direction for some figures --
   *  processes left behind, for instance, where the story is what went away. */
  from?: number;
  decimals?: number;
  ms?: number;
  /** Hold this long after coming into view before counting, so a row of stats
   *  can run one after another instead of all at once. */
  delay?: number;
  prefix?: string;
  suffix?: string;
  onDone?: () => void;
}) {
  const { ref, seen } = useInView<HTMLSpanElement>();
  // Render the final value first so it is correct in the prerendered HTML and
  // for anyone who never triggers the animation.
  const [n, setN] = useState(to);
  const started = useRef(false);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    setN(from);
  }, [from]);

  useEffect(() => {
    if (!seen || started.current) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    started.current = true;
    let raf = 0;
    const timer = setTimeout(() => {
      const t0 = performance.now();
      const step = (now: number) => {
        const p = Math.min(1, (now - t0) / ms);
        setN(from + (to - from) * ease(p));
        if (p < 1) raf = requestAnimationFrame(step);
        else onDone?.();
      };
      raf = requestAnimationFrame(step);
    }, delay);
    return () => {
      clearTimeout(timer);
      cancelAnimationFrame(raf);
    };
  }, [seen, to, from, ms, delay, onDone]);

  return (
    <chakra.span ref={ref} style={{ fontVariantNumeric: "tabular-nums" }}>
      {prefix}
      {n.toFixed(decimals)}
      {suffix}
    </chakra.span>
  );
}

/* ------------------------------------------------------------------ */
/* 12. SummaryGrid — the whole comparison on one screen                 */
/* ------------------------------------------------------------------ */

/** Drawn, not typed: a glyph would inherit the font and render inconsistently. */
function Tick({ kind }: { kind: "yes" | "no" }) {
  return (
    <chakra.svg
      width="15px"
      height="15px"
      viewBox="0 0 16 16"
      fill="none"
      display="inline-block"
      verticalAlign="middle"
      color={kind === "yes" ? "verdict.good" : "verdict.bad"}
      aria-hidden="true"
    >
      {kind === "yes" ? (
        <path
          d="M3 8.5 L6.4 12 L13 4.5"
          stroke="currentColor"
          strokeWidth="2.1"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      ) : (
        <path d="M4 4 L12 12 M12 4 L4 12" stroke="currentColor" strokeWidth="2.1" strokeLinecap="round" />
      )}
    </chakra.svg>
  );
}

export type SummaryCell = boolean | string | null;

export function SummaryGrid({
  cols,
  rows,
  t,
}: {
  cols: string[];
  rows: { label: string; cells: SummaryCell[] }[];
  t: number;
}) {
  const per = 1 / (rows.length + 2);
  return (
    <Box>
      <Flex pb="12px" borderBottom="1px solid" borderColor="border.rule">
        <Box flex="2.1" minW="0" />
        {cols.map((c, ci) => (
          <Text
            key={c}
            flex="1"
            textAlign="center"
            fontFamily="mono"
            fontSize="14px"
            fontWeight={ci === 0 ? "bold" : "normal"}
            color={ci === 0 ? "accent.500" : "text.muted"}
          >
            {c}
          </Text>
        ))}
      </Flex>
      {rows.map((r, ri) => {
        const on = t > per * (ri + 1);
        return (
          <Flex
            key={r.label}
            align="center"
            py="13px"
            borderBottom="1px solid"
            borderColor="border.rule"
            opacity={on ? 1 : 0}
            transform={on ? "translateY(0)" : "translateY(5px)"}
            transition="opacity 320ms ease, transform 320ms ease"
          >
            <Text flex="2.1" minW="0" fontFamily="mono" fontSize="14px" color="text.secondary" pr="12px">
              {r.label}
            </Text>
            {r.cells.map((v, ci) => (
              <Flex key={`${r.label}-${cols[ci]}`} flex="1" justify="center" align="center">
                {v === null ? (
                  <Text fontFamily="mono" fontSize="14px" color="text.faint">
                    —
                  </Text>
                ) : typeof v === "boolean" ? (
                  <Tick kind={v ? "yes" : "no"} />
                ) : (
                  <Text
                    fontFamily="mono"
                    fontSize="14.5px"
                    fontWeight={ci === 0 ? "bold" : "normal"}
                    color={ci === 0 ? "accent.500" : "text.body"}
                  >
                    {v}
                  </Text>
                )}
              </Flex>
            ))}
          </Flex>
        );
      })}
    </Box>
  );
}
