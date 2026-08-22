import { Box, Flex, Stack, Text } from "@chakra-ui/react";
import { Link } from "react-router";
import { ARTICLES } from "~/content/articles";
import { formatDate, RELEASES } from "~/content/releases";
import { Eyebrow } from "~/ds/components";

export function meta() {
  return [
    { title: "Blog — systemg" },
    { name: "description", content: "Release notes and writing from the systemg project." },
  ];
}

export default function Blog() {
  let year = "";

  return (
    <Box maxW="page" mx="auto" px={{ base: "20px", md: "gutter" }} pt="72px" pb="96px">
      <Box maxW="content">
        <Eyebrow>Blog</Eyebrow>
        <Box
          as="h1"
          mt="14px"
          fontSize={{ base: "44px", md: "h1" }}
          lineHeight="0.98"
          letterSpacing="-0.045em"
          fontWeight="bold"
          color="text.heading"
        >
          Releases &amp; writing
        </Box>
        <Text mt="20px" fontSize="lead" lineHeight="1.5" color="text.secondary">
          Every systemg release, with its changelog.
        </Text>
        <Text
          mt="10px"
          fontFamily="mono"
          fontSize="micro"
          letterSpacing="0.06em"
          textTransform="uppercase"
          color="text.faint"
        >
          {RELEASES.length} posts
        </Text>
      </Box>

      <Stack gap="0" mt="48px">
        {ARTICLES.map((a) => (
          <Box key={a.slug} borderTop="1px solid" borderColor="border.rule">
            <Link to={`/blog/${a.date}/${a.slug}`}>
              <Flex py="24px" gap="18px" align="baseline" wrap="wrap">
                <Text fontFamily="mono" fontSize="micro" color="accent.500" minW="86px">
                  {formatDate(a.date)}
                </Text>
                <Box flex="1" minW="260px">
                  <Text fontSize="20px" letterSpacing="-0.02em" fontWeight="bold" color="text.heading">
                    {a.title}
                  </Text>
                  <Text mt="6px" fontSize="14px" lineHeight="1.5" color="text.secondary">
                    {a.summary}
                  </Text>
                </Box>
              </Flex>
            </Link>
          </Box>
        ))}
        {RELEASES.map((release) => {
          const y = release.date.slice(0, 4);
          const newYear = y !== year;
          if (newYear) year = y;
          return (
            <Box key={release.slug} borderTop="1px solid" borderColor="border.rule">
              {newYear ? (
                <Text mt="24px" fontFamily="mono" fontSize="micro" letterSpacing="0.06em" color="text.faint">
                  {y}
                </Text>
              ) : null}
              <Link to={`/blog/${release.slug}`}>
                <Flex
                  py="24px"
                  gap={{ base: "8px", sm: "32px" }}
                  direction={{ base: "column", sm: "row" }}
                  css={{ "&:hover h2": { color: "var(--accent-700)" } }}
                >
                  <Stack gap="8px" flex="0 0 auto" width={{ base: "auto", sm: "140px" }} align="flex-start">
                    <Text fontFamily="mono" fontSize="meta" color="text.muted">
                      {formatDate(release.date)}
                    </Text>
                    <Box
                      fontFamily="mono"
                      fontSize="micro"
                      px="10px"
                      py="3px"
                      borderRadius="pill"
                      bg={release.prerelease ? "status.warnBg" : "status.okBg"}
                      color={release.prerelease ? "status.warnFg" : "status.okFg"}
                    >
                      {release.prerelease ? "prerelease" : "release"}
                    </Box>
                  </Stack>
                  <Box minW="0">
                    <Box
                      as="h2"
                      fontSize="h3"
                      lineHeight="1.3"
                      letterSpacing="-0.02em"
                      fontWeight="bold"
                      color="text.heading"
                      transition="var(--transition-hover)"
                    >
                      {release.title}
                    </Box>
                    {release.summary ? (
                      <Text mt="6px" fontSize="bodySm" lineHeight="1.55" color="text.secondary">
                        {release.summary}
                      </Text>
                    ) : null}
                  </Box>
                </Flex>
              </Link>
            </Box>
          );
        })}
      </Stack>
    </Box>
  );
}
