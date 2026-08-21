import { Box } from "@chakra-ui/react";
import type { ReactNode } from "react";

export function Prose({ children }: { children: ReactNode }) {
  return (
    <Box
      maxW="content"
      color="text.body"
      fontSize="body"
      lineHeight="body"
      css={{
        "& > * + *": { marginTop: "var(--stack-block)" },
        "& :is(h1, h2, h3, h4, h5, h6) + :is(pre, .code-block, div:has(> pre))": {
          marginTop: "calc(var(--stack-block) + 5px)",
        },
        "& h2": {
          fontSize: "var(--fs-h2)",
          lineHeight: "var(--lh-h2)",
          letterSpacing: "var(--ls-h2)",
          fontWeight: "var(--fw-bold)",
          marginTop: "var(--stack-section)",
        },
        "& h3": {
          fontSize: "var(--fs-h3)",
          lineHeight: "var(--lh-h3)",
          letterSpacing: "var(--ls-h3)",
          fontWeight: "var(--fw-bold)",
          marginTop: "var(--sp-8)",
        },
        "& h4": { fontSize: "var(--fs-body-lg)", fontWeight: "var(--fw-semibold)", marginTop: "var(--sp-6)" },
        "& a": { color: "inherit", textDecoration: "none" },
        "& p a, & li a, & td a, & th a, & blockquote a": {
          color: "var(--text-link)",
          textDecoration: "underline",
          textUnderlineOffset: "3px",
        },
        "& p a:hover, & li a:hover, & td a:hover, & th a:hover, & blockquote a:hover": {
          color: "var(--text-link-hover)",
        },
        "& strong": { fontWeight: "var(--fw-semibold)", color: "var(--text-heading)" },
        "& ul": { listStyle: "disc", paddingInlineStart: "1.1em" },
        "& ol": { listStyle: "decimal", paddingInlineStart: "1.3em" },
        "& li": { paddingBlock: "var(--list-item-padding, 0.3em)" },
        "& li::marker": { color: "var(--glyph-dim)" },
        "& :not(pre) > code": {
          fontFamily: "var(--font-mono)",
          fontSize: "0.85em",
          background: "var(--surface-inline-code)",
          border: "1px solid var(--border-default)",
          borderRadius: "var(--radius-xs)",
          padding: "2px 5px",
        },
        "& pre": {
          margin: 0,
          padding: "var(--pad-code)",
          overflowX: "auto",
          border: "1px solid var(--border-code)",
          borderRadius: "var(--radius-md)",
          fontFamily: "var(--font-mono)",
          fontSize: "13px",
          lineHeight: "1.85",
          boxShadow: "var(--shadow-card)",
        },
        "& hr": { border: 0, borderTop: "1px solid var(--border-rule)", marginBlock: "var(--sp-8)" },
        "& blockquote": {
          borderInlineStart: "var(--rule-width) solid var(--border-strong)",
          paddingInlineStart: "var(--sp-4)",
          color: "var(--text-secondary)",
        },
        "& table": {
          width: "100%",
          borderCollapse: "collapse",
          fontSize: "var(--fs-body-sm)",
          border: "1px solid var(--border-default)",
          borderRadius: "var(--radius-md)",
          overflow: "hidden",
        },
        "& th": {
          textAlign: "start",
          fontFamily: "var(--font-mono)",
          fontSize: "var(--fs-micro)",
          letterSpacing: "var(--ls-micro)",
          textTransform: "uppercase",
          color: "var(--text-muted)",
          background: "var(--surface-subtle)",
          padding: "10px 14px",
          borderBottom: "1px solid var(--border-default)",
        },
        "& td": { padding: "10px 14px", borderTop: "1px solid var(--border-default)", verticalAlign: "top" },
        "& img": { maxWidth: "100%", height: "auto", borderRadius: "var(--radius-md)" },
        "& h2 > a, & h3 > a, & h4 > a": { textDecoration: "none", color: "inherit" },
      }}
    >
      {children}
    </Box>
  );
}
