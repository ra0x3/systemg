import { Box, Grid, Text } from "@chakra-ui/react";
import { useState } from "react";
import { Link } from "react-router";
import { Callout, Panel, PanelHeader } from "~/ds/components";

type Kids = { children?: React.ReactNode };

function Info({ children }: Kids) {
  return <Callout type="info">{children}</Callout>;
}
function Note({ children }: Kids) {
  return <Callout type="note">{children}</Callout>;
}
function Warning({ children }: Kids) {
  return <Callout type="warning">{children}</Callout>;
}

function Card({ title, href, children }: Kids & { title?: string; href?: string }) {
  const inner = (
    <Panel p="card" height="100%" transition="var(--transition-hover)" _hover={{ borderColor: "border.controlHover" }}>
      {title ? (
        <Text fontSize="h3" lineHeight="1.3" letterSpacing="-0.02em" fontWeight="bold" color="text.heading" mb="8px">
          {title}
        </Text>
      ) : null}
      <Box fontSize="bodySm" lineHeight="1.55" color="text.secondary">
        {children}
      </Box>
    </Panel>
  );
  if (!href) return inner;
  if (href.startsWith("http")) {
    return (
      <a href={href} target="_blank" rel="noreferrer">
        {inner}
      </a>
    );
  }
  return <Link to={href}>{inner}</Link>;
}

function CardGroup({ cols = 2, children }: Kids & { cols?: number }) {
  return (
    <Grid gridTemplateColumns={{ base: "1fr", md: `repeat(${cols}, minmax(0, 1fr))` }} gap="18px">
      {children}
    </Grid>
  );
}

function CodeGroup({ children }: Kids) {
  const items = Array.isArray(children) ? children : [children];
  const [active, setActive] = useState(0);
  const labels = items.map((child, i) => {
    const props = (child as { props?: Record<string, unknown> })?.props ?? {};
    return (props["data-title"] as string) || (props["data-lang"] as string) || `tab ${i + 1}`;
  });

  return (
    <Panel radius="md">
      <PanelHeader>
        <Box display="flex" gap="2px">
          {labels.map((label, i) => (
            <Box
              as="button"
              key={label + i}
              onClick={() => setActive(i)}
              bg={i === active ? "accent.tint" : "transparent"}
              border="1px solid"
              borderColor={i === active ? "accent.500" : "transparent"}
              color={i === active ? "accent.700" : "text.muted"}
              fontFamily="mono"
              fontSize="11.5px"
              px="11px"
              py="6px"
              borderRadius="pill"
              cursor="pointer"
            >
              {label}
            </Box>
          ))}
        </Box>
      </PanelHeader>
      <Box css={{ "& pre": { border: 0, borderRadius: 0, boxShadow: "none" } }}>{items[active]}</Box>
    </Panel>
  );
}

export const mdxComponents = {
  callout: Callout,
  Info,
  Note,
  Tip: Info,
  Warning,
  Danger: Warning,
  Card,
  CardGroup,
  Columns: CardGroup,
  CodeGroup,
  a: ({ href = "", children }: Kids & { href?: string }) =>
    href.startsWith("http") || href.startsWith("#") ? (
      <a href={href} target={href.startsWith("http") ? "_blank" : undefined} rel="noreferrer">
        {children}
      </a>
    ) : (
      <Link to={href}>{children}</Link>
    ),
};
