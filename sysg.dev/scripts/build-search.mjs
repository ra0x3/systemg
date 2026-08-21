import { readFileSync, writeFileSync } from "node:fs";
import { ENTRIES } from "../app/content/manifest.ts";

const DOCS = new URL("../content/docs/", import.meta.url);
const RELEASES = new URL("../content/releases.json", import.meta.url);
const OUT = new URL("../content/search.json", import.meta.url);

function plain(mdx) {
  return mdx
    .replace(/^---\n[\s\S]*?\n---\n/, "")
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/^import .*$/gm, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/:::\w+/g, " ")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[#*_>`|]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function headings(mdx) {
  return [...mdx.matchAll(/^#{2,3}\s+(.+)$/gm)].map((m) => m[1].replace(/[`*_]/g, "").trim());
}

const docs = ENTRIES.map((entry) => {
  const raw = readFileSync(new URL(entry.source, DOCS), "utf8");
  return {
    id: entry.route,
    route: entry.route,
    section: entry.section,
    group: entry.group,
    title: entry.title,
    description: entry.description ?? "",
    headings: headings(raw).join(" · "),
    text: plain(raw).slice(0, 3000),
  };
});

const releases = JSON.parse(readFileSync(RELEASES, "utf8")).map((r) => ({
  id: `/blog/${r.slug}`,
  route: `/blog/${r.slug}`,
  section: "blog",
  group: r.prerelease ? "Prerelease" : "Release",
  title: r.title,
  description: r.summary,
  headings: "",
  text: r.html
    .replace(/<[^>]+>/g, " ")
    .replace(/\s+/g, " ")
    .slice(0, 1200),
}));

const docsIndex = [...docs, ...releases];
writeFileSync(OUT, `${JSON.stringify(docsIndex)}\n`);
console.log(`search: indexed ${docsIndex.length} pages (${docs.length} docs, ${releases.length} releases)`);
