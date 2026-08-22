import { Box, chakra, Flex, Stack, Text } from "@chakra-ui/react";
import { Link } from "react-router";
import { Eyebrow } from "~/ds/components";
import { IconLink, SOCIALS } from "~/ds/icons";
import { Logo } from "~/ds/Logo";

const COLUMNS = [
  {
    header: "Docs",
    items: [
      { label: "Introduction", href: "/docs" },
      { label: "Quickstart", href: "/docs/quickstart" },
      { label: "Examples", href: "/docs/examples" },
    ],
  },
  {
    header: "Reference",
    items: [
      { label: "CLI commands", href: "/reference/commands" },
      { label: "Diagnostics", href: "/reference/dialog/codes" },
      { label: "Configuration", href: "/docs/how-it-works/configuration" },
    ],
  },
  {
    header: "More",
    items: [
      { label: "Blog", href: "/blog" },
      { label: "GitHub", href: "https://github.com/ra0x3/systemg" },
      { label: "Releases", href: "https://github.com/ra0x3/systemg/releases" },
    ],
  },
];

const ChakraLink = chakra(Link);

const linkStyle = {
  fontSize: "bodySm",
  lineHeight: "1.7",
  color: "text.footer",
  transition: "var(--transition-hover)",
  _hover: { color: "text.heading" },
} as const;

function FooterLink({ href, children }: { href: string; children: React.ReactNode }) {
  if (href.startsWith("http")) {
    return (
      <chakra.a href={href} target="_blank" rel="noreferrer" {...linkStyle}>
        {children}
      </chakra.a>
    );
  }
  return (
    <ChakraLink to={href} {...linkStyle}>
      {children}
    </ChakraLink>
  );
}

export function Footer() {
  return (
    <Box as="footer" borderTop="1px solid" borderColor="border.rule" bg="surface.subtle" mt="auto">
      <Box maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} py="48px">
        <Flex gap={{ base: "40px", md: "64px" }} wrap="wrap" justify="space-between" align="flex-start">
          <Stack gap="12px" minW="200px" maxW="280px">
            <Logo size={15} />
            <Text fontSize="meta" lineHeight="1.6" color="text.footer">
              An agent-friendly general-purpose program orchestrator for busy people.
            </Text>
            <Flex gap="2px" ml="-8px">
              {SOCIALS.map(({ href, label, Icon }) => (
                <IconLink key={label} href={href} label={label}>
                  <Icon />
                </IconLink>
              ))}
            </Flex>
          </Stack>

          <Flex gap={{ base: "40px", md: "64px" }} wrap="wrap">
            {COLUMNS.map((col) => (
              <Stack key={col.header} gap="8px">
                <Box mb="4px">
                  <Eyebrow color="text.muted">{col.header}</Eyebrow>
                </Box>
                {col.items.map((item) => (
                  <FooterLink key={item.label} href={item.href}>
                    {item.label}
                  </FooterLink>
                ))}
              </Stack>
            ))}
          </Flex>
        </Flex>

        <Box mt="40px" pt="20px" borderTop="1px solid" borderColor="border.rule">
          <Text fontFamily="mono" fontSize="11.5px" color="text.faint">
            systemg is MIT licensed.
          </Text>
        </Box>
      </Box>
    </Box>
  );
}
