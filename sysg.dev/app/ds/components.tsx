import { Box, chakra, Flex, Text } from "@chakra-ui/react";
import type { ReactNode } from "react";

export function Eyebrow({ children, color = "accent.500" }: { children: ReactNode; color?: string }) {
  return (
    <Text
      fontFamily="mono"
      fontSize="11.5px"
      letterSpacing="0.06em"
      textTransform="uppercase"
      color={color}
      lineHeight="1"
    >
      {children}
    </Text>
  );
}

export function InlineCode({ children }: { children: ReactNode }) {
  return (
    <chakra.code
      fontFamily="mono"
      fontSize="0.85em"
      bg="surface.inlineCode"
      border="1px solid"
      borderColor="border.default"
      borderRadius="xs"
      px="5px"
      py="2px"
      whiteSpace="nowrap"
    >
      {children}
    </chakra.code>
  );
}

type PillProps = {
  variant?: "primary" | "secondary" | "ghost";
  size?: "sm" | "md" | "lg";
  href?: string;
  onClick?: () => void;
  children: ReactNode;
  ariaLabel?: string;
};

const pad = { sm: "6px 11px", md: "7px 14px", lg: "11px 20px" };
const fs = { sm: "11.5px", md: "13px", lg: "13px" };

export function Pill({ variant = "primary", size = "md", href, onClick, children, ariaLabel }: PillProps) {
  const tone =
    variant === "primary"
      ? {
          bg: "action.primary",
          color: "text.inverse",
          borderColor: "action.primary",
          _hover: { bg: "action.primaryHover", borderColor: "action.primaryHover" },
        }
      : variant === "secondary"
        ? {
            bg: "surface.subtle",
            color: "text.heading",
            borderColor: "border.control",
            _hover: { borderColor: "accent.500" },
          }
        : {
            bg: "transparent",
            color: "text.muted",
            borderColor: "transparent",
            _hover: { bg: "action.ghostHover", color: "text.heading" },
          };

  return (
    <chakra.a
      as={href ? "a" : "button"}
      href={href}
      onClick={onClick}
      aria-label={ariaLabel}
      display="inline-flex"
      alignItems="center"
      gap="2"
      fontFamily="mono"
      fontWeight="medium"
      fontSize={fs[size]}
      lineHeight="1"
      padding={pad[size]}
      borderRadius="pill"
      border="1px solid"
      cursor="pointer"
      whiteSpace="nowrap"
      transition="var(--transition-hover)"
      {...tone}
    >
      {children}
    </chakra.a>
  );
}

export function Panel({
  children,
  radius = "lg",
  ...rest
}: {
  children: ReactNode;
  radius?: string;
  [k: string]: unknown;
}) {
  return (
    <Box
      border="1px solid"
      borderColor="border.default"
      borderRadius={radius}
      bg="surface.card"
      boxShadow="card"
      overflow="hidden"
      {...rest}
    >
      {children}
    </Box>
  );
}

export function PanelHeader({ children }: { children: ReactNode }) {
  return (
    <Flex
      align="center"
      gap="2"
      px="16px"
      py="10px"
      borderBottom="1px solid"
      borderColor="border.default"
      bg="surface.subtle"
      fontFamily="mono"
      fontSize="11.5px"
      color="text.faint"
    >
      {children}
    </Flex>
  );
}

export function Callout({
  type = "info",
  label,
  children,
}: {
  type?: "info" | "note" | "warning";
  label?: string;
  children: ReactNode;
}) {
  const map = {
    info: { bg: "callout.infoBg", rule: "callout.infoRule", fallback: "Info" },
    note: { bg: "callout.noteBg", rule: "callout.noteRule", fallback: "Note" },
    warning: { bg: "callout.warningBg", rule: "callout.warningRule", fallback: "Warning" },
  }[type];

  return (
    <Box bg={map.bg} borderLeft="3px solid" borderColor={map.rule} borderRadius="0 16px 16px 0" px="18px" py="16px">
      <Box mb="7px">
        <Eyebrow color={map.rule}>{label || map.fallback}</Eyebrow>
      </Box>
      <Box fontSize="bodySm" lineHeight="1.65" color="text.body">
        {children}
      </Box>
    </Box>
  );
}

export function Yaml({ lines }: { lines: ReactNode[] }) {
  return (
    <chakra.pre
      margin="0"
      px="20px"
      py="18px"
      overflowX="auto"
      fontFamily="mono"
      fontSize="13px"
      lineHeight="1.85"
      color="text.body"
    >
      {lines.map((line, i) => (
        <Box as="div" key={i}>
          {line}
        </Box>
      ))}
    </chakra.pre>
  );
}

export const K = ({ children }: { children: ReactNode }) => <chakra.span color="code.key">{children}</chakra.span>;
export const S = ({ children }: { children: ReactNode }) => <chakra.span color="code.string">{children}</chakra.span>;
export const N = ({ children }: { children: ReactNode }) => <chakra.span color="code.number">{children}</chakra.span>;
