import { Box, Flex, Text } from "@chakra-ui/react";
import { type CSSProperties, type ReactNode, useEffect, useRef, useState } from "react";
import { CountUp } from "~/ds/charts";

/** The width every figure is composed at; below it a figure scales rather than reflows. */
const FIT = 860;

/**
 * Charts are laid out once, at FIT, and shrunk to whatever room the card has.
 * A narrow screen gets the same composition as a wide one, just smaller -- no
 * label wraps to two lines, no bar row breaking onto a second line.
 */
function Fit({ children }: { children: ReactNode }) {
  const outer = useRef<HTMLDivElement>(null);
  const inner = useRef<HTMLDivElement>(null);
  const [scale, setScale] = useState(1);
  const [height, setHeight] = useState<number>();

  useEffect(() => {
    const o = outer.current;
    const i = inner.current;
    if (!o || !i) return;
    const measure = () => {
      const next = Math.min(1, o.clientWidth / FIT);
      setScale(next);
      setHeight(i.offsetHeight * next);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(o);
    ro.observe(i);
    return () => ro.disconnect();
  }, []);

  return (
    <Box ref={outer} height={height ? `${height}px` : undefined}>
      <Box
        ref={inner}
        width={scale < 1 ? `${FIT}px` : "100%"}
        transformOrigin="top left"
        transform={scale < 1 ? `scale(${scale})` : undefined}
        style={{ "--fit-scale": scale } as CSSProperties}
      >
        {children}
      </Box>
    </Box>
  );
}

export function Figure({ children, caption }: { children: ReactNode; caption?: ReactNode }) {
  return (
    <Box mt="30px">
      <Box
        border="1px solid"
        borderColor="border.rule"
        borderRadius="lg"
        bg="surface.card"
        p={{ base: "14px", md: "26px" }}
      >
        <Fit>{children}</Fit>
      </Box>
      {caption ? (
        <Text mt="10px" fontFamily="mono" fontSize="14.5px" lineHeight="1.55" color="text.faint">
          {caption}
        </Text>
      ) : null}
    </Box>
  );
}

const STAT_MS = 720;
const STAT_GAP = 90;

export function Stat({
  to,
  from,
  decimals,
  unit,
  label,
  order = 0,
}: {
  to: number;
  from?: number;
  decimals: number;
  unit?: string;
  label: string;
  /** Position in the hero row; each stat waits for the one before it. */
  order?: number;
}) {
  return (
    <Box minW="128px">
      <Flex align="baseline" gap="3px">
        <Text
          fontSize={{ base: "38px", md: "50px" }}
          lineHeight="1"
          letterSpacing="-0.045em"
          fontWeight="bold"
          color="text.heading"
        >
          <CountUp to={to} from={from} decimals={decimals} ms={STAT_MS} delay={order * (STAT_MS + STAT_GAP)} />
        </Text>
        {unit ? (
          <Text fontFamily="mono" fontSize="15px" color="accent.500">
            {unit}
          </Text>
        ) : null}
      </Flex>
      <Text mt="9px" fontFamily="mono" fontSize="14.5px" lineHeight="1.5" color="text.muted" whiteSpace="pre-line">
        {label}
      </Text>
    </Box>
  );
}
