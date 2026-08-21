import { Box, chakra, Flex, Stack, Text } from "@chakra-ui/react";
import { MDXProvider } from "@mdx-js/react";
import { NavLink } from "react-router";
import type { Section } from "~/content/manifest";
import { navFor, neighbours, type Page } from "~/content/pages";
import { Eyebrow, Pill } from "~/ds/components";
import { Prose } from "~/ds/Prose";
import { mdxComponents } from "~/mdx/components";

function Sidebar({ section, current }: { section: Section; current: string }) {
  return (
    <Box
      as="nav"
      width="sidebar"
      flex="0 0 auto"
      pt="32px"
      pb="96px"
      pr="16px"
      borderRight="1px solid"
      borderColor="border.rule"
      position="sticky"
      top="nav"
      alignSelf="flex-start"
      maxHeight="calc(100vh - var(--layout-nav-h))"
      overflowY="auto"
      display={{ base: "none", lg: "block" }}
    >
      {navFor(section).map((group, i) => (
        <Box key={`${group.group}-${i}`} mb="26px">
          {group.group ? (
            <Box px="12px" mb="12px">
              <Eyebrow>{group.group}</Eyebrow>
            </Box>
          ) : null}
          <Stack gap="1px">
            {group.items.map((item) => {
              const active = item.route === current;
              return (
                <NavLink key={item.route} to={item.route}>
                  <Box
                    px="12px"
                    py="7px"
                    borderRadius="pill"
                    fontSize="15px"
                    color={active ? "text.heading" : "text.muted"}
                    bg={active ? "action.ghostHover" : "transparent"}
                    transition="var(--transition-hover)"
                    _hover={{ bg: "action.ghostHover", color: "text.heading" }}
                  >
                    {item.title}
                  </Box>
                </NavLink>
              );
            })}
          </Stack>
        </Box>
      ))}
    </Box>
  );
}

function Toc({ items }: { items: { depth: number; id: string; title: string }[] }) {
  if (!items.length) return <Box width="toc" flex="0 0 auto" display={{ base: "none", xl: "block" }} />;
  return (
    <Box
      as="aside"
      width="toc"
      flex="0 0 auto"
      pt="40px"
      position="sticky"
      top="nav"
      alignSelf="flex-start"
      maxHeight="calc(100vh - var(--layout-nav-h))"
      overflowY="auto"
      display={{ base: "none", xl: "block" }}
    >
      <Box mb="12px">
        <Eyebrow color="text.muted">On this page</Eyebrow>
      </Box>
      <Stack gap="8px" borderLeft="1px solid" borderColor="border.rule" pl="14px">
        {items.map((item) => (
          <chakra.a
            key={item.id}
            href={`#${item.id}`}
            fontSize="bodySm"
            lineHeight="1.4"
            color="text.muted"
            pl={item.depth === 3 ? "12px" : "0"}
            transition="var(--transition-hover)"
            _hover={{ color: "text.heading" }}
          >
            {item.title}
          </chakra.a>
        ))}
      </Stack>
    </Box>
  );
}

export function DocsShell({ page }: { page: Page }) {
  const Content = page.mod.default;
  const toc = page.mod.toc ?? [];
  const { prev, next } = neighbours(page.route, page.section);

  return (
    <Flex maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} gap="gutter" align="flex-start">
      <Sidebar section={page.section} current={page.route} />

      <Box as="article" flex="1" minW="0" maxW="content" pt="40px" pb="120px">
        {page.group ? (
          <Box mb="16px">
            <Eyebrow>{page.group}</Eyebrow>
          </Box>
        ) : null}
        <Box
          as="h1"
          fontSize={{ base: "40px", md: "h1" }}
          lineHeight="0.98"
          letterSpacing="-0.045em"
          fontWeight="bold"
          color="text.heading"
        >
          {page.title}
        </Box>
        {page.description ? (
          <Text mt="20px" fontSize="lead" lineHeight="1.5" color="text.secondary">
            {page.description}
          </Text>
        ) : null}

        <Box mt="section">
          <MDXProvider components={mdxComponents}>
            <Prose>
              <Content components={mdxComponents} />
            </Prose>
          </MDXProvider>
        </Box>

        {prev || next ? (
          <Flex
            mt="section"
            pt="24px"
            borderTop="1px solid"
            borderColor="border.rule"
            gap="12px"
            justify="space-between"
          >
            {prev ? (
              <Pill variant="secondary" href={prev.route}>
                ← {prev.title}
              </Pill>
            ) : (
              <Box />
            )}
            {next ? (
              <Pill variant="secondary" href={next.route}>
                {next.title} →
              </Pill>
            ) : null}
          </Flex>
        ) : null}
      </Box>

      <Toc items={toc} />
    </Flex>
  );
}
