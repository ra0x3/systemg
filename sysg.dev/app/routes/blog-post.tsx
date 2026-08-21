import { Box, Flex, Stack, Text } from "@chakra-ui/react";
import { useLocation } from "react-router";
import { formatDate, RELEASE_BY_SLUG, RELEASES } from "~/content/releases";
import { Eyebrow, Pill } from "~/ds/components";
import { NotFound } from "~/ds/NotFound";
import { Prose } from "~/ds/Prose";

export default function BlogPost() {
  const { pathname } = useLocation();
  const slug = pathname.replace(/\/$/, "").split("/").pop() ?? "";
  const release = RELEASE_BY_SLUG.get(slug);
  if (!release) return <NotFound path={pathname} />;

  const i = RELEASES.findIndex((r) => r.slug === slug);
  const newer = i > 0 ? RELEASES[i - 1] : null;
  const older = i >= 0 && i < RELEASES.length - 1 ? RELEASES[i + 1] : null;

  return (
    <Box maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} pt="64px" pb="120px">
      <Box maxW="content" mx="auto">
        <Flex align="center" gap="12px" wrap="wrap">
          <Eyebrow>{release.prerelease ? "Prerelease" : "Release"}</Eyebrow>
          <Box width="24px" height="1px" bg="border.control" />
          <Text fontFamily="mono" fontSize="meta" color="text.muted">
            {formatDate(release.date)}
          </Text>
        </Flex>

        <Box
          as="h1"
          mt="18px"
          fontSize={{ base: "44px", md: "h1" }}
          lineHeight="0.98"
          letterSpacing="-0.045em"
          fontWeight="bold"
          color="text.heading"
        >
          {release.title}
        </Box>

        <Flex mt="20px" gap="10px" wrap="wrap" align="center">
          <Pill size="sm" variant="secondary" href={release.url}>
            View on GitHub
          </Pill>
          {release.author ? (
            <Text fontFamily="mono" fontSize="micro" color="text.faint">
              {release.author}
            </Text>
          ) : null}
        </Flex>

        <Box mt="section">
          <Prose>
            <div dangerouslySetInnerHTML={{ __html: release.html }} />
          </Prose>
        </Box>

        <Flex mt="section" pt="24px" borderTop="1px solid" borderColor="border.rule" gap="12px" justify="space-between" wrap="wrap">
          {older ? (
            <Pill variant="secondary" href={`/blog/${older.slug}`}>
              ← {older.title}
            </Pill>
          ) : (
            <Box />
          )}
          {newer ? (
            <Pill variant="secondary" href={`/blog/${newer.slug}`}>
              {newer.title} →
            </Pill>
          ) : null}
        </Flex>

        <Stack mt="32px">
          <Pill variant="ghost" href="/blog">
            ← All releases
          </Pill>
        </Stack>
      </Box>
    </Box>
  );
}
