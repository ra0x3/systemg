import { BY_ROUTE } from "~/content/manifest";

const SITE = "https://sysg.dev";

export function metaForRoute(pathname: string) {
  const route = pathname.replace(/\/$/, "") || "/docs";
  const entry = BY_ROUTE.get(route);
  if (!entry) return [{ title: "Not found — systemg" }];

  const canonical = `${SITE}${entry.canonical ?? entry.route}`;
  const tags: Record<string, unknown>[] = [
    { title: `${entry.title} — systemg` },
    { property: "og:title", content: `${entry.title} — systemg` },
    { tagName: "link", rel: "canonical", href: canonical },
  ];
  if (entry.description) {
    tags.push({ name: "description", content: entry.description });
    tags.push({ property: "og:description", content: entry.description });
  }
  return tags;
}
