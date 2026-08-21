import { Box, chakra } from "@chakra-ui/react";

const RADIUS_RATIO = 107 / 500;
const GAP_RATIO = 52 / 176;
const TEXT_RATIO = 168 / 176;

export type LogoProps = {
  variant?: "mark" | "lockup";
  size?: number;
  title?: string;
};

export function Logo({ variant = "lockup", size = 26, title = "systemg" }: LogoProps) {
  const mark = (
    <Box
      as="span"
      aria-hidden="true"
      display="block"
      flex="none"
      width={`${size}px`}
      height={`${size}px`}
      borderRadius={`${size * RADIUS_RATIO}px`}
      bg="accent.500"
    />
  );

  if (variant === "mark") {
    return (
      <Box as="span" display="inline-flex" role="img" aria-label={title}>
        {mark}
      </Box>
    );
  }

  return (
    <Box
      as="span"
      display="inline-flex"
      alignItems="center"
      gap={`${size * GAP_RATIO}px`}
      role="img"
      aria-label={title}
    >
      {mark}
      <chakra.span
        fontFamily="mono"
        fontWeight="semibold"
        fontSize={`${size * TEXT_RATIO}px`}
        lineHeight="1"
        letterSpacing="-0.02em"
        color="text.heading"
        whiteSpace="nowrap"
      >
        systemg
      </chakra.span>
    </Box>
  );
}
