import { Box, chakra, Flex, Stack, Text } from "@chakra-ui/react";
import type { ReactNode } from "react";
import { useLocation } from "react-router";
import { ARTICLES } from "~/content/articles";
import {
  Blocks,
  CountUp,
  Gantt,
  type GanttUnit,
  GapBar,
  LineChart,
  Matrix,
  Meters,
  ProcessDots,
  Race,
  Replay,
  SummaryGrid,
  TreeSwap,
} from "~/ds/charts";
import { Eyebrow, InlineCode, Pill } from "~/ds/components";
import { Figure, Stat } from "~/ds/figure";
import { NotFound } from "~/ds/NotFound";

export function meta() {
  return [
    { title: "How systemg compares to other process managers" },
    {
      name: "description",
      content: "sysg against systemd, Supervisor, and Docker Compose. Eight benchmarks, every number reproducible.",
    },
  ];
}

/* ---------------- layout primitives ---------------- */

function H2({ children, kicker }: { children: ReactNode; kicker: string }) {
  return (
    <Box mt="104px" mb="6px">
      <Text
        fontFamily="mono"
        fontSize="11px"
        letterSpacing="0.1em"
        textTransform="uppercase"
        color="accent.500"
        mb="12px"
      >
        {kicker}
      </Text>
      <Box
        as="h2"
        fontSize={{ base: "30px", md: "38px" }}
        lineHeight="1.06"
        letterSpacing="-0.035em"
        fontWeight="bold"
        color="text.heading"
      >
        {children}
      </Box>
    </Box>
  );
}

function P({ children }: { children: ReactNode }) {
  return (
    <Text mt="16px" fontSize="body" lineHeight="1.65" color="text.body" maxW="content">
      {children}
    </Text>
  );
}

const RESULTS = "https://github.com/ra0x3/systemg/blob/main/tests/comp-harness-2026-08-21/README.md";

/** Deep-links the section of the writeup that shows how a figure was reached. */
function Deeper({ anchor }: { anchor: string }) {
  return (
    <chakra.a
      href={`${RESULTS}#${anchor}`}
      display="inline-block"
      mt="12px"
      fontFamily="mono"
      fontSize="14.5px"
      color="text.link"
      borderBottom="1px solid"
      borderColor="border.control"
      _hover={{ color: "text.linkHover", borderColor: "accent.500" }}
    >
      how this was measured →
    </chakra.a>
  );
}

function Note({ children }: { children: ReactNode }) {
  return (
    <Text mt="16px" fontFamily="mono" fontSize="15px" lineHeight="1.7" color="text.faint" maxW="content">
      {children}
    </Text>
  );
}

function Mult({ to, decimals, suffix, of }: { to: number; decimals: number; suffix: string; of: string }) {
  return (
    <Flex align="baseline" gap="9px">
      <Text
        fontSize={{ base: "34px", md: "42px" }}
        lineHeight="1"
        letterSpacing="-0.04em"
        fontWeight="bold"
        color="accent.500"
      >
        <CountUp to={to} decimals={decimals} suffix={suffix} ms={1200} />
      </Text>
      <Text fontFamily="mono" fontSize="16px" color="text.muted">
        {of}
      </Text>
    </Flex>
  );
}

/* ---------------- data ---------------- */

const LEVEL: Record<string, number> = {
  db: 0,
  queue: 0,
  cache: 1,
  ingest1: 1,
  ingest2: 1,
  worker1: 2,
  worker2: 2,
  worker3: 2,
  aggregator: 3,
  reporter: 4,
};
const NAMES = Object.keys(LEVEL);
const STEP = 1.38;

const SERIAL: GanttUnit[] = NAMES.map((name, i) => ({ name, start: i * STEP, dur: STEP }));
const PARALLEL: GanttUnit[] = NAMES.map((name) => ({ name, start: LEVEL[name] * STEP, dur: STEP }));

/* ---------------- article ---------------- */

export default function BlogArticle() {
  const { pathname } = useLocation();
  const here = pathname.replace(/^\/blog\//, "").replace(/\/$/, "");
  const article = ARTICLES.find((a) => `${a.date}/${a.slug}` === here);
  if (!article) return <NotFound path={pathname} />;

  return (
    <Box maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} pt="64px" pb="150px">
      <Box maxW="960px" mx="auto">
        <Flex align="center" gap="12px" wrap="wrap">
          <Eyebrow>Benchmarks</Eyebrow>
          <Box width="24px" height="1px" bg="border.control" />
          <Text fontFamily="mono" fontSize="meta" color="text.muted">
            Aug 21, 2026
          </Text>
        </Flex>

        <Box
          as="h1"
          mt="20px"
          fontSize={{ base: "48px", md: "76px" }}
          lineHeight="0.94"
          letterSpacing="-0.05em"
          fontWeight="bold"
          color="text.heading"
        >
          How systemg compares
        </Box>
        <Box
          as="p"
          mt="10px"
          fontSize={{ base: "22px", md: "30px" }}
          lineHeight="1.15"
          letterSpacing="-0.03em"
          fontWeight="bold"
          color="text.muted"
        >
          to other process managers
        </Box>

        <Text mt="24px" fontSize="lead" lineHeight="1.5" color="text.secondary" maxW="content">
          We ran systemg through a gauntlet of tests against systemd, Supervisor, and Docker Compose — install, boot,
          memory, teardown, crash recovery, and readiness.
        </Text>
        <Text mt="18px" fontSize="lead" lineHeight="1.5" color="text.heading" fontWeight="bold" maxW="content">
          TL;DR: systemg installs faster than Supervisor, boots a dependency graph faster than Docker Compose, and uses
          less memory than either. It leaves nothing running after you stop a service — so does Compose, where
          Supervisor strands up to five — and it is the only one of the three that neither duplicates nor loses your
          workload when the supervisor itself dies.
        </Text>

        <Flex mt="26px" gap="10px" wrap="wrap">
          <Pill href="https://github.com/ra0x3/systemg/blob/main/tests/comp-harness-2026-08-21/README.md">
            Full results &amp; methodology
          </Pill>
          <Pill variant="secondary" href="/docs/quickstart">
            Install sysg
          </Pill>
        </Flex>

        <Flex mt="46px" gap={{ base: "26px", md: "44px" }} wrap="wrap">
          <Stat order={0} to={1.49} decimals={2} unit="s" label={"install to first\nsupervised service"} />
          <Stat order={1} to={6.91} decimals={2} unit="s" label={"ten-service graph\ncold boot"} />
          <Stat order={2} to={12.4} decimals={1} unit="MB" label={"supervisor overhead\nat ten services"} />
          <Stat order={3} to={0} from={5} decimals={0} label={"processes left behind\non stop"} />
        </Flex>

        {/* 1. INSTALL — what you do first */}
        <H2 kicker="Install">Getting to a running service</H2>
        <P>Each tool installed the way its users install it, timed until one service is actually supervised.</P>

        <Figure caption="systemd ships with the distribution, so its marginal install cost is zero.">
          <Replay ms={1900}>
            {(t) => (
              <Meters
                t={t}
                groups={[
                  {
                    title: "to first supervised service",
                    unit: "seconds, cold",
                    rows: [
                      { label: "sysg", value: 1.49, display: "1.49s", subject: true },
                      { label: "Supervisor", value: 5.53, display: "5.53s" },
                      { label: "Docker Compose", value: 15.85, display: "15.85s+" },
                    ],
                  },
                  {
                    title: "pulled over the network",
                    unit: "megabytes, cold cache",
                    rows: [
                      { label: "sysg", value: 6.8, display: "6.8 MB", subject: true },
                      { label: "Supervisor", value: 48, display: "48.0 MB" },
                      { label: "Docker Compose", value: 172.5, display: "172.5 MB" },
                    ],
                  },
                  {
                    title: "installed on disk",
                    unit: "megabytes",
                    rows: [
                      { label: "systemd", value: 14.6, display: "14.6 MB" },
                      { label: "sysg", value: 19.5, display: "19.5 MB", subject: true },
                      { label: "Supervisor", value: 26.8, display: "26.8 MB" },
                      { label: "Docker Compose", value: 278.6, display: "278.6 MB" },
                    ],
                  },
                ]}
              />
            )}
          </Replay>
        </Figure>

        <Box mt="34px">
          <Mult to={3.7} decimals={1} suffix="×" of="faster to a supervised service than Supervisor" />
        </Box>
        <Note>
          Supervisor's 48 MB is 47.6 MB of apt index plus 0.4 MB of packages — on a machine that ran apt today it pulls
          0.4 MB. sysg pulls 6.8 MB in every cache state. systemd is 14.6 MB installed and ships with the distribution,
          so its marginal install cost is zero. 86% of Supervisor's disk figure is the CPython runtime; sysg carries its
          runtime compiled into the binary.
        </Note>
        <Deeper anchor="2-install-payload" />

        {/* 2. BOOT — then you start your services */}
        <H2 kicker="Dependency graph">Ten services, five levels deep</H2>
        <P>
          Two independent roots, a five-wide fan-in. 0.64 walked the topological order one unit at a time; 0.65
          dispatches every unit whose dependencies are already satisfied.
        </P>

        <Figure caption="Each bar is one service becoming ready. Playhead is wall-clock.">
          <Stack gap="30px">
            <Box>
              <Text fontFamily="mono" fontSize="15px" color="text.muted" mb="12px">
                sysg 0.64.4 — one at a time
              </Text>
              <Replay ms={2600} label="10 steps">
                {(t) => <Gantt units={SERIAL} span={14} t={t} total="13.83s" />}
              </Replay>
            </Box>
            <Box height="1px" bg="border.rule" />
            <Box>
              <Text fontFamily="mono" fontSize="15px" color="text.muted" mb="12px">
                sysg 0.65.0 — by dependency level
              </Text>
              <Replay ms={2600} label="5 waves">
                {(t) => <Gantt units={PARALLEL} span={14} t={t} accent total="6.91s" />}
              </Replay>
            </Box>
          </Stack>
        </Figure>

        <Box mt="34px">
          <Mult to={2.0} decimals={2} suffix="×" of="faster than sysg 0.64.4" />
        </Box>

        <Figure caption="Same graph, same clock. Each marker stops when its last service reports healthy.">
          <Replay ms={2800} label="all four, one clock">
            {(t) => (
              <Race
                t={t}
                span={14}
                lanes={[
                  { label: "sysg 0.65.0", time: 6.91, subject: true },
                  { label: "Docker Compose", time: 8.31, note: "1.20× slower" },
                  { label: "sysg 0.64.4", time: 13.83, note: "2.00× slower" },
                  { label: "Supervisor", time: null },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Note>
          Supervisor has no dependency edges. <InlineCode>priority</InlineCode> orders starts but does not gate on
          readiness, so this graph cannot be expressed. Compose runs each unit as a container and sysg runs each as a
          process — different isolation, and Compose ran against the host daemon while sysg ran in a container.
        </Note>
        <Deeper anchor="4-dependency-graph-start" />

        {/* 3. READINESS — then you ask if they are up */}
        <H2 kicker="Readiness">Reporting a service as up</H2>
        <P>
          A service that starts but cannot do work for five seconds. The marker is when the tool says it is up; the
          shaded band is the distance from there to when it actually works.
        </P>

        <Figure caption="Band left of the marker is reporting late. Band right of it is reporting early.">
          <Replay ms={1700}>
            {(t) => (
              <GapBar
                t={t}
                span={9}
                rows={[
                  { label: "sysg", reported: 5.64, usable: 5.64, note: "health probe" },
                  {
                    label: "Supervisor, tuned",
                    reported: 5.22,
                    usable: 5.22,
                    note: "startsecs=5, matched to startup",
                  },
                  { label: "Compose", reported: 5.73, usable: 5.21, note: "probe lands on the next 1s tick" },
                  { label: "Supervisor, default", reported: 1.22, usable: 5.1, note: "startsecs=1" },
                  {
                    label: "Supervisor, 8s service",
                    reported: 5.19,
                    usable: 8.12,
                    note: "startsecs=5, startup varies",
                  },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Note>
          <InlineCode>startsecs</InlineCode> is a fixed timer: set it to the real startup time and it is exact. A probe
          is closed-loop, so it does not need to be told.
        </Note>
        <Deeper anchor="8-readiness-semantics" />

        {/* 4. OVERHEAD — what it costs while running */}
        <H2 kicker="Resident cost">What the supervisor itself costs</H2>
        <P>
          Memory for the tool, minus the same services run bare. Ten near-idle services, so the tool dominates. These
          are orders of magnitude apart, so area is proportional to megabytes.
        </P>

        <Figure caption="dockerd + containerd + one containerd-shim per container, measured inside the VM.">
          <Replay ms={1500}>
            {(t) => (
              <Blocks
                t={t}
                unit="MB"
                items={[
                  { label: "sysg", value: 12.4, display: "12.4 MB", subject: true },
                  { label: "Supervisor", value: 18.1, display: "18.1 MB" },
                  { label: "Docker Compose", value: 354, display: "354 MB" },
                ]}
              />
            )}
          </Replay>
        </Figure>

        <Box mt="34px">
          <Mult to={28} decimals={0} suffix="×" of="less than Docker Compose, at ten services" />
        </Box>
        <Note>On macOS, Docker Desktop adds a further 476 MB host-side for the VM and its helpers.</Note>
        <Deeper anchor="5-resource-usage-overhead" />

        {/* 5. SLOPE — and as you add more */}
        <H2 kicker="Scaling">Cost per service, as services are added</H2>
        <P>
          The intercept matters less than the slope. sysg 0.66 adds an <InlineCode>exec:</InlineCode> argv form that
          runs a service directly instead of through a shell, which removes a resident process per service.
        </P>

        <Figure caption="Fitted from N = 1, 10, 40. Drawn to N = 500.">
          <Replay ms={2200} label="drawn to N=500">
            {(t) => (
              <LineChart
                t={t}
                xMax={500}
                yMax={32}
                xLabel="services supervised →"
                yLabel="MB overhead"
                cross={{ x: 395, note: "crosses at N≈395" }}
                series={[
                  { name: "sysg exec:", intercept: 12.18, slope: 0.0346, accent: true },
                  { name: "Supervisor", intercept: 17.86, slope: 0.0202 },
                  { name: "sysg command:", intercept: 12.18, slope: 0.1451, dash: true },
                ]}
              />
            )}
          </Replay>
        </Figure>

        <Figure caption="Processes at 40 services. One per service under exec:, two under command:.">
          <Replay ms={1500}>
            {(t) => (
              <Meters
                t={t}
                groups={[
                  {
                    title: "added MB per service",
                    unit: "fitted slope",
                    rows: [
                      { label: "Supervisor", value: 0.0202, display: "0.020" },
                      { label: "sysg exec:", value: 0.0346, display: "0.035", subject: true },
                      { label: "sysg command:", value: 0.1451, display: "0.145" },
                    ],
                  },
                  {
                    title: "processes at N=40",
                    unit: "supervisor + children",
                    rows: [
                      { label: "sysg exec:", value: 45, display: "45", subject: true },
                      { label: "Supervisor", value: 45, display: "45" },
                      { label: "sysg command:", value: 85, display: "85" },
                    ],
                  },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Note>
          <InlineCode>command:</InlineCode> is unchanged and still spawns through a shell, which stays resident as the
          service's parent — 85 processes for 40 services instead of 45.
        </Note>
        <Deeper anchor="5-resource-usage-overhead" />

        {/* 6. TEARDOWN — then you stop something */}
        <H2 kicker="Teardown">Stopping a service that forked children</H2>
        <P>
          A service and five descendants, one of which deliberately escapes the process group. Stop it with each
          tool&apos;s own command, then count what is still running.
        </P>

        <Figure caption="Filled squares are processes still alive after stop returned.">
          <Replay ms={2000} label="stop →">
            {(t) => (
              <ProcessDots
                t={t}
                groups={[
                  { label: "sysg", survivors: 0, note: "session teardown reaches the setsid child" },
                  { label: "Docker Compose", survivors: 0, note: "container PID namespace collapses" },
                  {
                    label: "Supervisor, tuned",
                    survivors: 2,
                    note: "stopasgroup + killasgroup; setsid child escaped the group",
                  },
                  { label: "Supervisor, default", survivors: 5, note: "signals only the pid it spawned" },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Note>
          Survivors reparent to init and keep running. The loss is per stop/restart cycle, so a service that restarts
          hourly accumulates them.
        </Note>
        <Deeper anchor="6-descendant-containment" />

        {/* 7. CRASH — and eventually something breaks */}
        <H2 kicker="Control plane">When the supervisor dies</H2>
        <P>
          <InlineCode>kill -9</InlineCode> the supervisor — dockerd for Compose, not the CLI — then start it again and
          look at what happened to the workload.
        </P>

        <Figure caption="Docker tested at its default live-restore: false.">
          <Replay ms={2700} label="kill -9 → restart">
            {(t) => (
              <Flex gap="14px" wrap={{ base: "wrap", md: "nowrap" }}>
                <TreeSwap
                  t={t}
                  tool="sysg"
                  before={["sysg", " └─ web  pid 77"]}
                  after={["sysg  (new pid)", " └─ web  pid 77"]}
                  verdict="re-adopted, same pid"
                />
                <TreeSwap
                  t={t}
                  tool="Supervisor"
                  before={["supervisord", " └─ web  pid 77"]}
                  after={["supervisord (new)", " ├─ web  pid 77  orphan", " └─ web  pid 91  duplicate"]}
                  verdict="duplicate started"
                  bad
                />
                <TreeSwap
                  t={t}
                  tool="Docker Compose"
                  before={["dockerd", " └─ web  running"]}
                  after={["dockerd (manual)", " └─ web  exited"]}
                  verdict="workload terminated"
                  bad
                />
              </Flex>
            )}
          </Replay>
        </Figure>

        <Figure caption="Green is the outcome you want, whichever way the question is phrased.">
          <Replay ms={2000}>
            {(t) => (
              <Matrix
                t={t}
                cols={["sysg", "Supervisor", "Compose"]}
                rows={[
                  { label: "services survive", cells: [true, true, false] },
                  { label: "visible while down", cells: [true, true, false] },
                  { label: "no duplicate started", cells: [true, false, true] },
                  { label: "workload kept", cells: [true, true, false] },
                  { label: "recovers unattended", cells: [true, true, false] },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Note>
          Supervisor's services keep running, but supervisord has no record of them and starts a second copy. With{" "}
          <InlineCode>live-restore: false</InlineCode> the Docker daemon does not re-attach to containers from the
          previous session; <InlineCode>live-restore: true</InlineCode> is untested here. While dockerd was down the
          workload could not be inspected at all — every path goes through the daemon.
        </Note>
        <Deeper anchor="7-control-plane-crash-durability" />

        {/* summary */}
        <H2 kicker="Summary">The process manager for busy people</H2>
        <P>
          Numbers where a figure was measured, marks where the answer is yes or no. A dash means it was not measured for
          that tool.
        </P>

        <Figure caption="systemd was measured on install size only; its runtime rows need a machine where it is PID 1.">
          <Replay ms={2100}>
            {(t) => (
              <SummaryGrid
                t={t}
                cols={["sysg", "systemd", "Supervisor", "Compose"]}
                rows={[
                  { label: "install to first service", cells: ["1.49s", null, "5.53s", "15.85s+"] },
                  { label: "pulled over network", cells: ["6.8 MB", null, "48.0 MB", "172.5 MB"] },
                  { label: "installed on disk", cells: ["19.5 MB", "14.6 MB", "26.8 MB", "278.6 MB"] },
                  { label: "ten-service graph", cells: ["6.91s", null, "n/a", "8.31s"] },
                  { label: "overhead at ten services", cells: ["12.4 MB", null, "18.1 MB", "354 MB"] },
                  { label: "added per service", cells: ["0.035 MB", null, "0.020 MB", "7.26 MB"] },
                  { label: "expresses a dependency graph", cells: [true, true, false, true] },
                  { label: "gates on a probe, not a timer", cells: [true, true, false, true] },
                  { label: "leaves nothing behind on stop", cells: [true, null, false, true] },
                  { label: "workload survives its crash", cells: [true, null, true, false] },
                  { label: "starts no duplicate after", cells: [true, null, false, true] },
                  { label: "recovers without an operator", cells: [true, null, true, false] },
                  { label: "runs without a separate runtime", cells: [true, true, false, false] },
                  { label: "installs without root", cells: [true, false, true, false] },
                ]}
              />
            )}
          </Replay>
        </Figure>
        <Deeper anchor="scope-and-ground-rules" />

        <P>
          systemg is open source and always welcomes contributors and users. Bugs, benchmark disputes, and re-runs that
          disagree with anything above are all useful — every script that produced these figures is in the repo, so a
          contradicting result is a pull request rather than an argument.
        </P>

        <Flex mt="26px" gap="10px" wrap="wrap">
          <Pill href="https://github.com/ra0x3/systemg">Contribute on GitHub</Pill>
          <Pill variant="secondary" href="https://www.linkedin.com/company/sysg">
            Follow on LinkedIn
          </Pill>
        </Flex>

        {/* method */}
        <H2 kicker="Method">Check any of it</H2>
        <P>
          The full writeup carries every trial, the pinned versions and images, the reasoning behind each metric, and
          the four harness bugs that produced plausible wrong numbers before they were caught.
        </P>
        <P>
          Every tool gets the same service body and the same probe interval. Where a setting changes a result —{" "}
          <InlineCode>stopasgroup</InlineCode>, <InlineCode>startsecs</InlineCode>, <InlineCode>exec:</InlineCode> —
          both configurations are charted. Trial counts are 2–5 per figure, cold, with raw output published alongside
          the scripts.
        </P>

        <Flex mt="30px" gap="10px" wrap="wrap">
          <Pill href="https://github.com/ra0x3/systemg/blob/main/tests/comp-harness-2026-08-21/README.md">
            Full results &amp; methodology
          </Pill>
          <Pill variant="secondary" href="https://github.com/ra0x3/systemg/tree/main/tests/comp-harness-2026-08-21">
            Scripts and raw output
          </Pill>
          <Pill variant="secondary" href="/docs/quickstart">
            Install sysg
          </Pill>
        </Flex>

        <Box mt="44px" borderTop="1px solid" borderColor="border.rule" pt="20px">
          <Text fontFamily="mono" fontSize="14.5px" lineHeight="1.7" color="text.faint" maxW="content">
            Gaps: Compose ran against the host daemon while sysg ran in a container, so the boot figure is indicative
            rather than controlled. systemd is charted on install size only — its runtime rows need a VM. Docker's{" "}
            <InlineCode>live-restore: true</InlineCode> is untested. N=40 Compose memory is extrapolated from the
            measured per-container cost.
          </Text>
          <Deeper anchor="open-items-and-caveats" />
        </Box>
      </Box>
    </Box>
  );
}
