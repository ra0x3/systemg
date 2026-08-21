import { writeFileSync, existsSync, readFileSync } from "node:fs";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import rehypeSlug from "rehype-slug";
import rehypeStringify from "rehype-stringify";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import rehypeShiki from "@shikijs/rehype";
import { unified } from "unified";
import { changelog, commitsFor } from "./git-log.mjs";
import { shikiOptions } from "../app/mdx/shiki.ts";

const OUT = new URL("../content/releases.json", import.meta.url);
const REPO = "ra0x3/systemg";

const md = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeRaw)
  .use(rehypeSlug)
  .use(rehypeSanitize)
  .use(rehypeShiki, shikiOptions)
  .use(rehypeStringify);

function summarise(body) {
  const line = body
    .split("\n")
    .map((l) => l.trim())
    .find(
      (l) =>
        l &&
        !l.startsWith("#") &&
        !l.startsWith("<!--") &&
        !/^\**Full Changelog/i.test(l) &&
        !/^\[Full changelog/i.test(l),
    );
  if (!line) return "";
  const text = line
    .replace(/^[-*]\s*/, "")
    .replace(/[*_`]/g, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  return text.length > 200 ? `${text.slice(0, 197)}…` : text;
}

async function fetchAll() {
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
  const headers = { accept: "application/vnd.github+json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const out = [];
  for (let page = 1; page <= 10; page++) {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=100&page=${page}`, { headers });
    if (!res.ok) throw new Error(`GitHub API ${res.status} ${res.statusText}`);
    const batch = await res.json();
    out.push(...batch);
    if (batch.length < 100) break;
  }
  return out;
}

let releases;
try {
  const raw = await fetchAll();
  releases = await Promise.all(
    raw
      .filter((r) => !r.draft)
      .map(async (r) => {
        const md_body = changelog(r.tag_name, r.body || "");
        return {
          tag: r.tag_name,
          slug: r.tag_name.replace(/^v/, "").replace(/[^\w.-]/g, "-"),
          title: r.name?.trim() || r.tag_name,
          date: r.published_at || r.created_at,
          prerelease: Boolean(r.prerelease),
          author: r.author?.login ?? null,
          url: r.html_url,
          summary: summarise(md_body),
          html: md_body ? String(await md.process(md_body)) : "",
          commits: commitsFor(r.tag_name).length,
        };
      }),
  );
  releases.sort((a, b) => (a.date < b.date ? 1 : -1));
  writeFileSync(OUT, `${JSON.stringify(releases, null, 2)}\n`);
  console.log(`releases: wrote ${releases.length}`);
} catch (error) {
  if (existsSync(OUT)) {
    const cached = JSON.parse(readFileSync(OUT, "utf8"));
    console.warn(`releases: fetch failed (${error.message}) — keeping ${cached.length} cached`);
  } else {
    throw error;
  }
}
