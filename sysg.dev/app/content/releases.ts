import data from "../../content/releases.json";

export type Release = {
  tag: string;
  slug: string;
  title: string;
  date: string;
  prerelease: boolean;
  author: string | null;
  url: string;
  summary: string;
  html: string;
};

export const RELEASES = data as Release[];

export const RELEASE_BY_SLUG = new Map(RELEASES.map((r) => [r.slug, r]));

export function blogRoutes() {
  return ["/blog", ...RELEASES.map((r) => `/blog/${r.slug}`)];
}

export function formatDate(iso: string) {
  return new Date(iso).toLocaleDateString("en-US", { month: "short", day: "2-digit", year: "numeric", timeZone: "UTC" });
}
