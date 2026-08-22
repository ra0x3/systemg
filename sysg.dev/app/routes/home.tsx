import { Box, chakra, Flex, Grid, Heading, Stack, Text } from "@chakra-ui/react";
import { type ReactNode, useState } from "react";
import { Blocks, Race, Replay, TreeSwap } from "~/ds/charts";
import { Eyebrow, InlineCode, Panel, Pill } from "~/ds/components";
import { Figure, Stat } from "~/ds/figure";

export function meta() {
  return [
    { title: "systemg — An agent-friendly general-purpose program orchestrator for busy people" },
    { name: "description", content: "An agent-friendly general-purpose program orchestrator for busy people." },
  ];
}

const INSTALL = [
  { id: "curl", cmd: "curl --proto '=https' -fsSL https://sh.sysg.dev/ | sh" },
  { id: "brew", cmd: "brew install systemg/tap/sysg" },
  { id: "cargo", cmd: "cargo install systemg" },
];

const FEATURES = [
  {
    eyebrow: "supervise",
    title: "Dependency-ordered startup",
    body: "Independent branches start in parallel; dependents wait on a health check, not a sleep.",
  },
  {
    eyebrow: "recover",
    title: "Restart with backoff",
    body: "Unsuccessful exits retry on a doubling delay and reset once the process stays healthy.",
  },
  {
    eyebrow: "observe",
    title: "One prefixed log stream",
    body: "Every service writes to the same stream, so ordering across processes is preserved.",
  },
];

const METRICS = [
  {
    id: "boot",
    to: 6.91,
    decimals: 2,
    unit: "s",
    label: "ten-service graph\ncold boot",
    heading: "Ten services, five levels deep",
    compare: "1.2× faster than Docker Compose · Supervisor cannot express the graph",
    blurb:
      "Two independent roots and a five-wide fan-in, where every unit whose dependencies are already satisfied is dispatched at once — so the graph boots in five waves rather than ten steps.",
    caption: (
      <>
        Supervisor has no dependency edges — <InlineCode>priority</InlineCode> orders starts but never gates on
        readiness. Compose ran each unit as a container against the host daemon while sysg ran processes in a container,
        so that lane is indicative rather than controlled.
      </>
    ),
    ms: 2600,
    chart: (t: number) => (
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
    ),
  },
  {
    id: "overhead",
    to: 12.4,
    decimals: 1,
    unit: "MB",
    label: "supervisor overhead\nat ten services",
    heading: "What the supervisor itself costs",
    compare: "28× less supervisor overhead than Docker Compose · 1.5× less than Supervisor",
    blurb:
      "Memory for the supervisor itself, minus the same ten near-idle services run bare — orders of magnitude apart, so each block is sized against the largest rather than on a shared axis.",
    caption: (
      <>
        Compose is dockerd + containerd + one containerd-shim per container, measured inside the VM; on macOS Docker
        Desktop adds a further 476 MB host-side. Per added service Supervisor is cheaper than sysg — 0.020 MB against
        0.035 MB.
      </>
    ),
    ms: 1500,
    chart: (t: number) => (
      <Blocks
        t={t}
        unit="MB"
        items={[
          { label: "sysg", value: 12.4, display: "12.4 MB", subject: true },
          { label: "Supervisor", value: 18.1, display: "18.1 MB" },
          { label: "Docker Compose", value: 354, display: "354 MB" },
        ]}
      />
    ),
  },
  {
    id: "recovery",
    to: 5,
    decimals: 0,
    unit: "/5",
    label: "recovery checks passed\nSupervisor 4 · Compose 1",
    heading: "When the supervisor itself is killed",
    blurb:
      "kill -9 the supervisor — dockerd for Compose, not the CLI — then start it again and look at what happened to the workload underneath it.",
    compare: "Supervisor starts a duplicate · Compose terminates the workload",
    caption: (
      <>
        Five checks: services survive, visible while down, no duplicate started, workload kept, recovers unattended.
        sysg passes five, Supervisor four, Docker Compose one. Docker tested at its default{" "}
        <InlineCode>live-restore: false</InlineCode> and restarted by hand; Supervisor&apos;s services keep running, but
        supervisord has no record of them and starts a second copy beside the orphan.
      </>
    ),
    ms: 2700,
    chart: (t: number) => (
      <Flex gap="14px" wrap={{ base: "wrap", md: "nowrap" }}>
        <TreeSwap
          t={t}
          tool="sysg"
          before={["sysg", " \u2514\u2500 web  pid 77"]}
          after={["sysg  (new pid)", " \u2514\u2500 web  pid 77"]}
          verdict="re-adopted, same pid"
        />
        <TreeSwap
          t={t}
          tool="Supervisor"
          before={["supervisord", " \u2514\u2500 web  pid 77"]}
          after={["supervisord (new)", " \u251c\u2500 web  pid 77  orphan", " \u2514\u2500 web  pid 91  duplicate"]}
          verdict="duplicate started"
          bad
        />
        <TreeSwap
          t={t}
          tool="Docker Compose"
          before={["dockerd", " \u2514\u2500 web  running"]}
          after={["dockerd (manual)", " \u2514\u2500 web  exited"]}
          verdict="workload terminated"
          bad
        />
      </Flex>
    ),
  },
];

const B = ({ children }: { children: ReactNode }) => (
  <chakra.strong fontWeight="bold" color="text.heading">
    {children}
  </chakra.strong>
);

function Proof() {
  return (
    <Box as="section" maxW="1020px" mx="auto" px={{ base: "20px", md: "gutter" }} pt="96px">
      <Flex align="center" gap="12px" wrap="wrap" mb="14px">
        <Eyebrow>measured</Eyebrow>
        <Box width="24px" height="1px" bg="border.control" />
        <Text fontFamily="mono" fontSize="12px" color="text.muted">
          three of eight benchmarks
        </Text>
      </Flex>

      <Heading as="h2" fontSize="h2" lineHeight="1.1" letterSpacing="-0.03em" fontWeight="bold" mb="12px">
        Faster, lighter, and recovers better
      </Heading>
      <Text color="text.body" maxW="content">
        We ran systemg against Supervisor and Docker Compose across eight benchmarks, and against systemd on installed
        size — the one metric it compares on without being PID 1. sysg booted a ten-service dependency graph{" "}
        <B>1.2× faster than Docker Compose</B> — a graph Supervisor cannot express at all — held it on{" "}
        <B>28× less supervisor overhead than Compose</B> and <B>1.5× less than Supervisor</B>, and was the{" "}
        <B>only one of the three to recover from its own kill -9 without losing or duplicating the workload</B>.
      </Text>
      <Box mt="20px">
        <Pill href="/blog/2026-08-21/how-systemg-compares">View the full report →</Pill>
      </Box>

      <Stack gap="72px" mt="56px">
        {METRICS.map((m) => (
          <Box key={m.id}>
            <Flex gap={{ base: "20px", md: "40px" }} align="baseline" wrap="wrap" mb="18px">
              <Stat to={m.to} decimals={m.decimals} unit={m.unit} label={m.label} />
              <Box flex="1" minW="280px">
                <Text fontSize="h3" lineHeight="1.3" letterSpacing="-0.02em" fontWeight="bold" color="text.heading">
                  {m.heading}
                </Text>
                <Text mt="8px" fontSize="bodySm" lineHeight="1.55" color="text.secondary">
                  {m.blurb}
                </Text>
                <Text mt="10px" fontFamily="mono" fontSize="13px" lineHeight="1.5" color="accent.500">
                  {m.compare}
                </Text>
              </Box>
            </Flex>
            <Figure caption={m.caption}>
              <Replay ms={m.ms}>{(t) => m.chart(t)}</Replay>
            </Figure>
          </Box>
        ))}
      </Stack>

      <Flex mt="44px" gap="10px" wrap="wrap">
        <Pill href="/blog/2026-08-21/how-systemg-compares">See the other five benchmarks →</Pill>
        <Pill variant="secondary" href="https://github.com/ra0x3/systemg/tree/main/tests/comp-harness-2026-08-21">
          Scripts and raw output
        </Pill>
      </Flex>
    </Box>
  );
}

function InstallCard() {
  const [tab, setTab] = useState("curl");
  const [copied, setCopied] = useState(false);
  const active = INSTALL.find((i) => i.id === tab) ?? INSTALL[0];

  return (
    <Panel radius="md" maxW="720px">
      <Flex
        align="center"
        gap="2px"
        px="10px"
        py="8px"
        borderBottom="1px solid"
        borderColor="border.default"
        bg="surface.subtle"
      >
        {INSTALL.map((item) => {
          const on = item.id === tab;
          return (
            <chakra.button
              key={item.id}
              type="button"
              onClick={() => {
                setTab(item.id);
                setCopied(false);
              }}
              bg={on ? "accent.tint" : "transparent"}
              border="1px solid"
              borderColor={on ? "accent.500" : "transparent"}
              color={on ? "accent.700" : "text.muted"}
              fontFamily="mono"
              fontSize="11.5px"
              px="11px"
              py="6px"
              borderRadius="pill"
              cursor="pointer"
              transition="var(--transition-hover)"
            >
              {item.id}
            </chakra.button>
          );
        })}
      </Flex>
      <Grid gridTemplateColumns="1fr auto" alignItems="center" gap="12px" pl="18px" pr="16px" py="16px">
        <Box fontFamily="mono" fontSize="13px" overflowX="auto" color="text.body" whiteSpace="nowrap">
          <chakra.span color="code.caret">$</chakra.span> {active.cmd}
        </Box>
        <chakra.button
          type="button"
          onClick={() => {
            navigator.clipboard?.writeText(active.cmd);
            setCopied(true);
          }}
          bg="surface.subtle"
          border="1px solid"
          borderColor="border.control"
          color="text.secondary"
          fontFamily="mono"
          fontSize="11px"
          px="11px"
          py="6px"
          borderRadius="pill"
          cursor="pointer"
          transition="var(--transition-hover)"
          _hover={{ borderColor: "border.controlHover", color: "text.heading" }}
        >
          {copied ? "copied" : "copy"}
        </chakra.button>
      </Grid>
    </Panel>
  );
}

export default function Home() {
  return (
    <Box as="main">
      <Box as="section" position="relative" overflow="hidden" borderBottom="1px solid" borderColor="border.rule">
        <Box
          position="absolute"
          insetInline="-200px"
          top="-80px"
          height="520px"
          background="var(--hero-wash)"
          pointerEvents="none"
        />
        <Box
          position="relative"
          maxW="1020px"
          mx="auto"
          px={{ base: "20px", md: "gutter" }}
          pt={{ base: "56px", md: "96px" }}
          pb="72px"
        >
          <Flex align="center" gap="12px" wrap="wrap">
            <Eyebrow>v0.65.0 is out</Eyebrow>
            <Box width="24px" height="1px" bg="border.control" />
            <Text fontFamily="mono" fontSize="12px" color="text.muted">
              read the release notes
            </Text>
          </Flex>

          <Heading
            as="h1"
            fontSize={{ base: "44px", md: "62px", lg: "hero" }}
            lineHeight="0.96"
            letterSpacing="-0.045em"
            fontWeight="bold"
            maxW="820px"
            mt="22px"
            mb="20px"
          >
            The process manager for busy people.
          </Heading>

          <Text fontSize="lead" lineHeight="1.5" color="text.secondary" maxW="640px" mb="32px">
            systemg is an agent-friendly process composer. Services start in dependency order, restart with backoff, and
            log to one stream — from a single YAML file, with no daemon to install alongside it.
          </Text>

          <Flex wrap="wrap" align="center" gap="12px" mb="36px">
            <Pill size="lg" href="/docs/quickstart">
              Get started
            </Pill>
            <Pill size="lg" variant="secondary" href="/docs">
              Read the docs →
            </Pill>
          </Flex>

          <InstallCard />
        </Box>
      </Box>

      <Box as="section" maxW="1020px" mx="auto" px={{ base: "20px", md: "gutter" }} pt="72px">
        <Grid gridTemplateColumns={{ base: "1fr", md: "repeat(3, minmax(0, 1fr))" }} gap="18px">
          {FEATURES.map((f) => (
            <Panel key={f.eyebrow} p="card">
              <Box mb="10px">
                <Eyebrow>{f.eyebrow}</Eyebrow>
              </Box>
              <Text fontSize="h3" lineHeight="1.3" letterSpacing="-0.02em" fontWeight="bold" color="text.heading">
                {f.title}
              </Text>
              <Text mt="8px" fontSize="bodySm" lineHeight="1.55" color="text.secondary">
                {f.body}
              </Text>
            </Panel>
          ))}
        </Grid>
      </Box>

      <Proof />

      <Box as="section" maxW="1020px" mx="auto" px={{ base: "20px", md: "gutter" }} pt="72px" pb="96px">
        <Panel px={{ base: "24px", md: "36px" }} py="34px">
          <Flex align="center" justify="space-between" gap="24px" wrap="wrap">
            <Box>
              <Text fontSize="h2" lineHeight="1.1" letterSpacing="-0.03em" fontWeight="bold" color="text.heading">
                Start with the quickstart
              </Text>
              <Text mt="8px" color="text.secondary">
                One binary, one file, five minutes.
              </Text>
            </Box>
            <Flex gap="10px" flex="none">
              <Pill size="lg" href="/docs/quickstart">
                Quickstart
              </Pill>
              <Pill size="lg" variant="secondary" href="/reference">
                CLI reference
              </Pill>
            </Flex>
          </Flex>
        </Panel>
      </Box>
    </Box>
  );
}
