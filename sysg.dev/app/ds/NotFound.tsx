import { Box, Flex, Stack, Text } from "@chakra-ui/react";
import { MorphLogo } from "~/ds/MorphLogo";
import { Eyebrow, Pill } from "~/ds/components";

export function NotFound({ path }: { path?: string }) {
  return (
    <Flex flex="1" align="center" justify="center" px={{ base: "20px", md: "gutter" }} py="96px">
      <Stack align="center" textAlign="center" gap="block" maxW="520px">
        <MorphLogo />

        <Box mt="16px">
          <Eyebrow>404</Eyebrow>
        </Box>

        <Box
          as="h1"
          fontSize={{ base: "40px", md: "h1" }}
          lineHeight="0.98"
          letterSpacing="-0.045em"
          fontWeight="bold"
          color="text.heading"
        >
          The supervisor lost track of this path.
        </Box>

        <Text fontSize="lead" lineHeight="1.5" color="text.secondary">
          We couldn't find this content.
        </Text>

        {path ? (
          <Box
            as="code"
            fontFamily="mono"
            fontSize="meta"
            bg="surface.inlineCode"
            border="1px solid"
            borderColor="border.default"
            borderRadius="xs"
            px="8px"
            py="4px"
            color="text.secondary"
            maxW="100%"
            overflowX="auto"
          >
            {path}
          </Box>
        ) : null}

        <Flex gap="10px" mt="8px" wrap="wrap" justify="center">
          <Pill size="lg" href="/docs">
            Read the docs
          </Pill>
          <Pill size="lg" variant="secondary" href="/">
            Back home
          </Pill>
        </Flex>
      </Stack>
    </Flex>
  );
}
