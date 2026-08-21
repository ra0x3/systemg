import type { ComponentType } from "react";
import { ENTRIES, type Entry, type Section } from "~/content/manifest";

export type TocItem = { depth: number; id: string; title: string };

type MdxModule = {
  default: ComponentType<{ components?: Record<string, unknown> }>;
  frontmatter?: { title?: string; description?: string };
  toc?: TocItem[];
};

const MODULES = import.meta.glob("../../content/docs/**/*.mdx", { eager: true }) as Record<string, MdxModule>;

export type Page = Entry & { mod: MdxModule };

export const PAGES = new Map<string, Page>();

for (const entry of ENTRIES) {
  const mod = MODULES[`../../content/docs/${entry.source}`];
  if (mod) PAGES.set(entry.route, { ...entry, mod });
}

export function pagesFor(section: Section) {
  return ENTRIES.filter((e) => e.section === section);
}

export function navFor(section: Section) {
  const groups: { group: string | null; items: Entry[] }[] = [];
  for (const entry of pagesFor(section)) {
    const last = groups[groups.length - 1];
    if (last && last.group === entry.group) last.items.push(entry);
    else groups.push({ group: entry.group, items: [entry] });
  }
  return groups;
}

export function neighbours(route: string, section: Section) {
  const list = pagesFor(section);
  const i = list.findIndex((e) => e.route === route);
  return { prev: i > 0 ? list[i - 1] : null, next: i >= 0 && i < list.length - 1 ? list[i + 1] : null };
}
