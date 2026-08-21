import { Box, Flex, Text, chakra } from "@chakra-ui/react";
import { Monitor, Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { NavLink } from "react-router";
import { Logo } from "~/ds/Logo";
import { Pill } from "~/ds/components";
import { IconLink, SOCIALS } from "~/ds/icons";
import { Search } from "~/ds/Search";
import { applyMode, MODES, readMode, type Mode } from "~/ds/theme";

const LINKS = [
  { to: "/docs", label: "Docs" },
  { to: "/reference", label: "Reference" },
  { to: "/blog", label: "Blog" },
];

const ICONS: Record<Mode, typeof Sun> = { light: Sun, dark: Moon, system: Monitor };

function ModeToggle() {
  const [mode, setMode] = useState<Mode>("system");
  useEffect(() => setMode(readMode()), []);
  const Icon = ICONS[mode];

  return (
    <chakra.button
      type="button"
      aria-label={`Appearance: ${mode}`}
      title={`Appearance: ${mode}`}
      onClick={() => {
        const next = MODES[(MODES.indexOf(mode) + 1) % MODES.length];
        setMode(next);
        applyMode(next);
      }}
      flex="none"
      width="34px"
      height="34px"
      display="flex"
      alignItems="center"
      justifyContent="center"
      border="1px solid"
      borderColor="border.control"
      bg="surface.subtle"
      color="text.secondary"
      borderRadius="pill"
      cursor="pointer"
      transition="var(--transition-hover)"
      _hover={{ borderColor: "border.controlHover", color: "text.heading" }}
    >
      <Icon size={16} strokeWidth={1.6} />
    </chakra.button>
  );
}

export function Navbar() {
  return (
    <Box
      as="header"
      borderBottom="1px solid"
      borderColor="border.rule"
      position="sticky"
      top="0"
      zIndex="20"
      bg="surface.nav"
      backdropFilter="var(--blur-nav)"
    >
      <Flex align="center" gap="24px" maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} py="14px">
      <NavLink to="/" aria-label="systemg home">
        <Logo size={15} />
      </NavLink>

      <Flex as="nav" align="center" gap="2px" display={{ base: "none", md: "flex" }}>
        {LINKS.map((link) => (
          <NavLink key={link.to} to={link.to}>
            {({ isActive }) => (
              <Box
                px="12px"
                py="7px"
                borderRadius="pill"
                fontSize="15px"
                color={isActive ? "text.heading" : "text.muted"}
                bg={isActive ? "action.ghostHover" : "transparent"}
                transition="var(--transition-hover)"
                _hover={{ bg: "action.ghostHover", color: "text.heading" }}
              >
                {link.label}
              </Box>
            )}
          </NavLink>
        ))}
      </Flex>

      <Flex align="center" gap="2px" ml="6px" display={{ base: "none", sm: "flex" }}>
        {SOCIALS.map(({ href, label, Icon }) => (
          <IconLink key={label} href={href} label={label}>
            <Icon />
          </IconLink>
        ))}
      </Flex>

      <Box flex="1" />

      <Search />

        <Pill href="/docs/installation">Install</Pill>
        <ModeToggle />
      </Flex>
    </Box>
  );
}
