/** Hand-written blog posts, as opposed to the release notes in releases.json. */
export type Article = {
  /** Path segment: /blog/{date}/{slug} */
  date: string;
  slug: string;
  title: string;
  summary: string;
};

export const ARTICLES: Article[] = [
  {
    date: "2026-08-21",
    slug: "how-systemg-compares",
    title: "How systemg compares to other process managers",
    summary:
      "Eight benchmarks against systemd, Supervisor, and Docker Compose — install, boot, memory, teardown, crash recovery, readiness. Every number re-runs from a script in the repo.",
  },
];

export function articleRoutes() {
  return ARTICLES.map((a) => `/blog/${a.date}/${a.slug}`);
}
