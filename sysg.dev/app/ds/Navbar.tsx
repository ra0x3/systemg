import { Box, chakra, Flex, Stack } from "@chakra-ui/react";
import { Menu, Monitor, Moon, Search as SearchIcon, Sun, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { NavLink } from "react-router";
import { Pill } from "~/ds/components";
import { IconLink, SOCIALS } from "~/ds/icons";
import { Logo } from "~/ds/Logo";
import { requestSearch, Search } from "~/ds/Search";
import { applyMode, MODES, type Mode, readMode } from "~/ds/theme";

const LINKS = [
  { to: "/docs", label: "Docs" },
  { to: "/reference", label: "Reference" },
  { to: "/blog", label: "Blog" },
];

const ICONS: Record<Mode, typeof Sun> = { light: Sun, dark: Moon, system: Monitor };

const CONTROL = {
  flex: "none",
  width: "34px",
  height: "34px",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  border: "1px solid",
  borderColor: "border.control",
  bg: "surface.subtle",
  color: "text.secondary",
  borderRadius: "pill",
  cursor: "pointer",
  transition: "var(--transition-hover)",
  _hover: { borderColor: "border.controlHover", color: "text.heading" },
} as const;

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
      {...CONTROL}
    >
      <Icon size={16} strokeWidth={1.6} />
    </chakra.button>
  );
}

export function Navbar() {
  const [menu, setMenu] = useState(false);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const close = () => setMenu(false);

  useEffect(() => {
    if (!menu) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      setMenu(false);
      toggleRef.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu]);

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
      <Flex
        align="center"
        gap={{ base: "10px", md: "24px" }}
        maxW="page"
        mx="auto"
        px={{ base: "20px", md: "gutter" }}
        py="14px"
      >
        <NavLink to="/" aria-label="systemg home" onClick={close}>
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

        <Pill href="/docs/installation" onClick={close}>
          Install
        </Pill>
        <ModeToggle />

        <chakra.button
          ref={toggleRef}
          type="button"
          aria-label={menu ? "Close menu" : "Open menu"}
          aria-expanded={menu}
          aria-controls="nav-menu"
          onClick={() => setMenu((v) => !v)}
          {...CONTROL}
          display={{ base: "flex", md: "none" }}
        >
          {menu ? <X size={16} strokeWidth={1.6} /> : <Menu size={16} strokeWidth={1.6} />}
        </chakra.button>
      </Flex>

      {menu ? (
        <Box
          id="nav-menu"
          display={{ base: "block", md: "none" }}
          borderTop="1px solid"
          borderColor="border.rule"
          bg="surface.page"
          px="20px"
          py="12px"
        >
          <Stack gap="2px">
            {LINKS.map((link) => (
              <NavLink key={link.to} to={link.to} onClick={close}>
                {({ isActive }) => (
                  <Box
                    px="12px"
                    py="11px"
                    borderRadius="sm"
                    fontSize="17px"
                    color={isActive ? "text.heading" : "text.muted"}
                    bg={isActive ? "action.ghostHover" : "transparent"}
                  >
                    {link.label}
                  </Box>
                )}
              </NavLink>
            ))}

            <chakra.button
              type="button"
              onClick={() => {
                close();
                requestSearch();
              }}
              display="flex"
              alignItems="center"
              gap="10px"
              px="12px"
              py="11px"
              borderRadius="sm"
              fontSize="17px"
              color="text.muted"
              cursor="pointer"
              textAlign="start"
            >
              <SearchIcon size={17} strokeWidth={1.6} />
              Search
            </chakra.button>
          </Stack>

          <Flex align="center" gap="2px" mt="8px" pt="10px" borderTop="1px solid" borderColor="border.rule">
            {SOCIALS.map(({ href, label, Icon }) => (
              <IconLink key={label} href={href} label={label}>
                <Icon />
              </IconLink>
            ))}
          </Flex>
        </Box>
      ) : null}
    </Box>
  );
}
