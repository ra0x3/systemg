import { Box, chakra, Flex, Grid, Heading, Stack, Text } from "@chakra-ui/react";
import { useState } from "react";
import { BarRow, Eyebrow, InlineCode, K, N, Panel, PanelHeader, Pill, S, Yaml } from "~/ds/components";

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

const BARS = [
  { label: "sysg", value: "0.41 s", width: 14, subject: true },
  { label: "docker compose", value: "2.30 s", width: 78 },
  { label: "supervisord", value: "2.95 s", width: 100 },
];

const YAML_LINES = [
  <>
    <K>version</K>: <S>"2"</S>
  </>,
  <>
    <K>services</K>:
  </>,
  <>
    {"  "}
    <K>postgres</K>:
  </>,
  <>
    {"    "}
    <K>command</K>: <S>"postgres -D /var/lib/postgresql/data"</S>
  </>,
  <>
    {"  "}
    <K>api</K>:
  </>,
  <>
    {"    "}
    <K>command</K>: <S>"python app.py"</S>
  </>,
  <>
    {"    "}
    <K>depends_on</K>: [<S>"postgres"</S>]
  </>,
  <>
    {"    "}
    <K>restart</K>: {"{ "}
    <K>backoff</K>: <N>1s</N>, <K>max</K>: <N>5</N>
    {" }"}
  </>,
  <>
    {"  "}
    <K>backup</K>:
  </>,
  <>
    {"    "}
    <K>command</K>: <S>"pg_dump mydb &gt; /backups/db.sql"</S>
  </>,
  <>
    {"    "}
    <K>cron</K>: <S>"0 2 * * *"</S>
  </>,
];

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

      <Box
        as="section"
        maxW="1020px"
        mx="auto"
        px={{ base: "20px", md: "gutter" }}
        pt="72px"
        display="grid"
        gridTemplateColumns={{ base: "1fr", lg: "1fr 1fr" }}
        gap="28px"
        alignItems="start"
      >
        <Box>
          <Heading as="h2" fontSize="h2" lineHeight="1.1" letterSpacing="-0.03em" fontWeight="bold" mb="14px">
            One file describes the graph
          </Heading>
          <Text mb="20px" color="text.body">
            Declare commands, dependencies, restart policy and schedules in one manifest.{" "}
            <InlineCode>sysg validate</InlineCode> exits <InlineCode>0</InlineCode> when the file is sound, so CI can
            gate on it.
          </Text>
          <Stack gap="10px">
            {BARS.map((b) => (
              <BarRow key={b.label} {...b} />
            ))}
          </Stack>
          <Text mt="14px" fontSize="12.5px" lineHeight="1.5" color="text.faint">
            Illustrative — not a measured benchmark. Cold start of an eleven-service graph, Linux x64, median of 5 runs.
            Replace with real figures before publishing.
          </Text>
        </Box>

        <Panel radius="md">
          <PanelHeader>sysg.yaml</PanelHeader>
          <Yaml lines={YAML_LINES} />
        </Panel>
      </Box>

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
