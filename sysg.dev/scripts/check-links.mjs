/**
 * Fails when a prerendered page links somewhere the site does not serve.
 *
 * A relative MDX link resolves against the page it sits on, so `[Security](security)`
 * on /docs/philosophy points at /docs/philosophy/security — a 404 that renders
 * perfectly in review. This walks the built output the way a browser would:
 * every internal href, resolved against its own page, then matched to a file,
 * a redirect in vercel.json, or a fragment that actually exists on the target.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "../build/client");
const VERCEL = JSON.parse(readFileSync(resolve(HERE, "../vercel.json"), "utf8"));
/** Absolute links to our own host are internal links wearing a hostname. */
const SITE = "https://sysg.dev";

const REDIRECTS = new Map((VERCEL.redirects ?? []).map((r) => [r.source.replace(/\/$/, "") || "/", r.destination]));

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (entry.endsWith(".html")) out.push(full);
  }
  return out;
}

function served(path) {
  const clean = path.replace(/^\//, "");
  for (const candidate of [clean, join(clean, "index.html"), `${clean}.html`]) {
    const full = candidate === "" ? join(ROOT, "index.html") : join(ROOT, candidate);
    try {
      if (statSync(full).isFile()) return full;
    } catch {}
  }
  return null;
}

const anchorCache = new Map();
function anchors(file) {
  if (!anchorCache.has(file)) {
    const html = readFileSync(file, "utf8");
    anchorCache.set(file, new Set([...html.matchAll(/id="([^"]+)"/g)].map((m) => m[1])));
  }
  return anchorCache.get(file);
}

const pages = walk(ROOT);
const broken = new Map();

function report(link, page, why) {
  if (!broken.has(link)) broken.set(link, { why, pages: [] });
  broken.get(link).pages.push(page);
}

for (const file of pages) {
  const url =
    `/${relative(ROOT, file)
      .replace(/(^|\/)index\.html$/, "")
      .replace(/\.html$/, "")}`.replace(/\/$/, "") || "/";
  const html = readFileSync(file, "utf8");
  for (const [, raw] of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    if (/^(?:mailto:|tel:|data:|javascript:)/i.test(raw)) continue;
    if (/^(?:[a-z]+:)?\/\//i.test(raw) && !raw.startsWith(SITE)) continue;
    // Resolve the way a browser does: `trailingSlash: false` means the page URL
    // has no trailing slash, so a relative href replaces its last segment
    // rather than nesting under it.
    const resolvedUrl = new URL(raw, `${SITE}${url === "/" ? "" : url}`);
    if (resolvedUrl.origin !== SITE) continue;
    const fragment = resolvedUrl.hash.slice(1);
    const target = resolvedUrl.pathname.replace(/\/$/, "") || "/";
    const resolved = REDIRECTS.get(target) ?? target;
    const targetFile = served(resolved);
    if (!targetFile) {
      report(raw, url, "no page, file, or redirect serves this path");
      continue;
    }
    if (fragment && !anchors(targetFile).has(fragment)) {
      report(raw, url, `target has no id="${fragment}"`);
    }
  }
}

console.log(`links: crawled ${pages.length} prerendered pages`);
if (broken.size === 0) {
  console.log("links: every internal link resolves");
  process.exit(0);
}
for (const [link, { why, pages: sources }] of [...broken].sort()) {
  console.error(
    `\n  ${link}\n    ${why}\n    linked from: ${sources.slice(0, 5).join(", ")}${sources.length > 5 ? ` (+${sources.length - 5} more)` : ""}`,
  );
}
console.error(`\nlinks: ${broken.size} broken internal link(s)`);
process.exit(1);
