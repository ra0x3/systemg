import { Box, chakra, Flex, Stack, Text } from "@chakra-ui/react";
import { Search as SearchIcon, X } from "lucide-react";
import type MiniSearch from "minisearch";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useNavigate } from "react-router";
import { Eyebrow } from "~/ds/components";

type Doc = {
  id: string;
  route: string;
  section: string;
  group: string | null;
  title: string;
  description: string;
  headings: string;
  text: string;
};

type Hit = Doc & { snippet: string };

const LABEL: Record<string, string> = { docs: "Docs", reference: "Reference", blog: "Blog" };

let indexPromise: Promise<{ engine: MiniSearch<Doc>; byId: Map<string, Doc> }> | null = null;

let opener: (() => void) | null = null;

/** Opens the one mounted Search, so the mobile menu does not need a second copy of it. */
export function requestSearch() {
  opener?.();
}

function loadIndex() {
  indexPromise ??= (async () => {
    const [{ default: MiniSearchCtor }, { default: docs }] = await Promise.all([
      import("minisearch"),
      import("../../content/search.json"),
    ]);
    const list = docs as Doc[];
    const engine = new MiniSearchCtor<Doc>({
      fields: ["title", "description", "headings", "text"],
      storeFields: ["route"],
      searchOptions: {
        boost: { title: 6, headings: 3, description: 2 },
        prefix: true,
        fuzzy: 0.2,
        boostDocument: (id) => {
          const route = String(id);
          if (route.startsWith("/docs")) return 2.4;
          if (route.startsWith("/reference")) return 2;
          return 1;
        },
      },
    });
    engine.addAll(list);
    return { engine, byId: new Map(list.map((d) => [d.id, d])) };
  })();
  return indexPromise;
}

function snippetFor(doc: Doc, query: string) {
  const needle = query.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  const hay = doc.text;
  const at = needle ? hay.toLowerCase().indexOf(needle) : -1;
  if (at < 0) return doc.description || hay.slice(0, 120);
  const start = Math.max(0, at - 40);
  return `${start > 0 ? "…" : ""}${hay.slice(start, start + 150).trim()}…`;
}

function Portal({ children }: { children: React.ReactNode }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  if (!mounted) return null;
  return createPortal(children, document.body);
}

function useAnchor(ref: React.RefObject<HTMLElement | null>, active: boolean) {
  const [rect, setRect] = useState<{ top: number; right: number } | null>(null);
  useEffect(() => {
    if (!active) return;
    const measure = () => {
      const r = ref.current?.getBoundingClientRect();
      if (r) setRect({ top: r.bottom, right: window.innerWidth - r.right });
    };
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [ref, active]);
  return rect;
}

export function Search() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Hit[]>([]);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const sheetInputRef = useRef<HTMLInputElement>(null);
  const sheetRef = useRef<HTMLDivElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const returnTo = useRef<HTMLElement | null>(null);
  const navigate = useNavigate();

  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
    setHits([]);
  }, []);

  useEffect(() => {
    opener = () => setOpen(true);
    return () => {
      opener = null;
    };
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      const typing = el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
      if (typing) return;
      if (e.key === "/" || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k")) {
        e.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, close]);

  useEffect(() => {
    if (!open) {
      returnTo.current?.focus();
      returnTo.current = null;
      return;
    }
    returnTo.current = document.activeElement as HTMLElement | null;
    loadIndex();
    setActive(0);
    const id = requestAnimationFrame(() => {
      const visible = sheetInputRef.current?.offsetParent ? sheetInputRef.current : inputRef.current;
      visible?.focus();
    });
    return () => cancelAnimationFrame(id);
  }, [open]);

  useEffect(() => {
    if (!open || !query.trim()) {
      setHits([]);
      return;
    }
    let stale = false;
    loadIndex().then(({ engine, byId }) => {
      if (stale) return;
      const found = engine
        .search(query, { combineWith: query.trim().includes(" ") ? "AND" : "OR" })
        .slice(0, 10)
        .map((r) => {
          const doc = byId.get(String(r.id));
          return doc ? { ...doc, snippet: snippetFor(doc, query) } : null;
        })
        .filter(Boolean) as Hit[];
      setHits(found);
      setActive(0);
    });
    return () => {
      stale = true;
    };
  }, [query, open]);

  const go = useCallback(
    (route: string) => {
      close();
      navigate(route);
    },
    [close, navigate],
  );

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") close();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(i + 1, hits.length - 1));
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    }
    if (e.key === "Enter" && hits[active]) {
      e.preventDefault();
      go(hits[active].route);
    }
  };

  const expanded = open && (hits.length > 0 || query.trim().length > 0);
  const anchor = useAnchor(barRef, expanded);

  const results = (
    <>
      {hits.length === 0 && query.trim() ? (
        <Box px="16px" py="18px">
          <Text fontSize="bodySm" color="text.muted">
            No matches for{" "}
            <chakra.span fontFamily="mono" color="text.heading">
              {query}
            </chakra.span>
            .
          </Text>
        </Box>
      ) : null}

      <Stack gap="0">
        {hits.map((hit, i) => (
          <Box
            key={hit.id}
            as="button"
            textAlign="start"
            width="100%"
            onClick={() => go(hit.route)}
            onMouseEnter={() => setActive(i)}
            px="16px"
            py="11px"
            borderTop={i === 0 ? "none" : "1px solid"}
            borderColor="border.rule"
            bg={i === active ? "action.ghostHover" : "transparent"}
            cursor="pointer"
          >
            <Flex align="center" gap="10px" mb="3px">
              <Eyebrow color={i === active ? "accent.500" : "text.muted"}>{LABEL[hit.section] ?? hit.section}</Eyebrow>
              {hit.group ? (
                <Text fontFamily="mono" fontSize="micro" color="text.faint">
                  {hit.group}
                </Text>
              ) : null}
            </Flex>
            <Text fontSize="body" fontWeight="semibold" color="text.heading" lineHeight="1.3">
              {hit.title}
            </Text>
            {hit.snippet ? (
              <Text mt="2px" fontSize="bodySm" color="text.secondary" lineHeight="1.5" lineClamp={2}>
                {hit.snippet}
              </Text>
            ) : null}
          </Box>
        ))}
      </Stack>
    </>
  );

  return (
    <>
      {open ? (
        <Portal>
          <Box
            position="fixed"
            inset="0"
            zIndex="15"
            display={{ base: "none", lg: "block" }}
            bg="rgb(0 0 0 / 0.34)"
            onClick={close}
          />
        </Portal>
      ) : null}

      <Box position="relative" display={{ base: "none", lg: "block" }} width="220px">
        <Flex
          ref={barRef}
          align="center"
          justify="space-between"
          gap="12px"
          pl="14px"
          pr="8px"
          py="6px"
          border="1px solid"
          borderColor="border.control"
          borderRadius="pill"
          bg="surface.subtle"
          fontSize="15px"
          color="text.muted"
          cursor="text"
          transition="var(--transition-hover)"
          onClick={() => {
            setOpen(true);
            inputRef.current?.focus();
          }}
          _hover={open ? undefined : { borderColor: "border.controlHover", color: "text.heading" }}
        >
          {open ? (
            <chakra.input
              ref={inputRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onKey}
              placeholder="Search docs, reference and releases"
              aria-label="Search"
              flex="1"
              minW="0"
              background="transparent"
              border="none"
              outline="none"
              fontSize="15px"
              color="text.heading"
              _placeholder={{ color: "text.faint" }}
            />
          ) : (
            <Text>Search docs</Text>
          )}
          <chakra.kbd
            flex="none"
            fontFamily="mono"
            fontSize="11.5px"
            color="text.muted"
            border="1px solid"
            borderColor="border.control"
            borderRadius="pill"
            px="7px"
            py="3px"
            bg="surface.card"
          >
            /
          </chakra.kbd>
        </Flex>
      </Box>

      <chakra.button
        type="button"
        aria-label="Search"
        onClick={() => setOpen(true)}
        display={{ base: "none", md: "flex", lg: "none" }}
        flex="none"
        width="34px"
        height="34px"
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
        <SearchIcon size={16} strokeWidth={1.6} />
      </chakra.button>

      {expanded && anchor ? (
        <Portal>
          <Box
            position="fixed"
            top={`${anchor.top + 8}px`}
            right={`${anchor.right}px`}
            width="440px"
            maxWidth="calc(100vw - 40px)"
            zIndex="25"
            display={{ base: "none", lg: "block" }}
            bg="surface.card"
            border="1px solid"
            borderColor="border.default"
            borderRadius="lg"
            boxShadow="card"
            overflow="hidden"
          >
            <Box maxHeight="min(62vh, 440px)" overflowY="auto">
              {results}
            </Box>

            <Flex
              px="16px"
              py="9px"
              borderTop="1px solid"
              borderColor="border.default"
              bg="surface.subtle"
              gap="16px"
              fontFamily="mono"
              fontSize="micro"
              color="text.faint"
            >
              <Text>↑↓ navigate</Text>
              <Text>↵ open</Text>
              <Text>esc close</Text>
            </Flex>
          </Box>
        </Portal>
      ) : null}

      {open ? (
        <Portal>
          <Flex
            ref={sheetRef}
            role="dialog"
            aria-modal="true"
            aria-label="Search"
            onKeyDown={(e) => {
              if (e.key !== "Tab") return;
              const focusable = sheetRef.current?.querySelectorAll<HTMLElement>("input, button");
              if (!focusable?.length) return;
              const edge = e.shiftKey ? focusable[0] : focusable[focusable.length - 1];
              if (document.activeElement !== edge) return;
              e.preventDefault();
              (e.shiftKey ? focusable[focusable.length - 1] : focusable[0]).focus();
            }}
            position="fixed"
            inset="0"
            zIndex="30"
            display={{ base: "flex", lg: "none" }}
            direction="column"
            bg="surface.page"
          >
            <Flex align="center" gap="10px" px="16px" py="12px" borderBottom="1px solid" borderColor="border.rule">
              <SearchIcon size={17} strokeWidth={1.6} color="var(--text-faint)" />
              <chakra.input
                ref={sheetInputRef}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={onKey}
                placeholder="Search docs and releases"
                aria-label="Search"
                flex="1"
                minW="0"
                background="transparent"
                border="none"
                outline="none"
                fontSize="17px"
                color="text.heading"
                _placeholder={{ color: "text.faint" }}
              />
              <chakra.button
                type="button"
                aria-label="Close search"
                onClick={close}
                flex="none"
                display="flex"
                alignItems="center"
                color="text.muted"
                cursor="pointer"
              >
                <X size={19} strokeWidth={1.6} />
              </chakra.button>
            </Flex>
            <Box flex="1" overflowY="auto">
              {results}
            </Box>
          </Flex>
        </Portal>
      ) : null}
    </>
  );
}
