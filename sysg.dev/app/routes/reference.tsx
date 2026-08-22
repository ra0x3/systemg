import { Navigate, useLocation } from "react-router";
import { metaForRoute } from "~/content/meta";
import { PAGES, pagesFor } from "~/content/pages";
import { DocsShell } from "~/ds/DocsShell";
import { NotFound } from "~/ds/NotFound";

export function meta({ location }: { location: { pathname: string } }) {
  return metaForRoute(location.pathname);
}

const SECTION = "reference";
const ROOT = "/reference";

export default function Reference() {
  const { pathname } = useLocation();
  const route = pathname.replace(/\/$/, "") || ROOT;
  const page = PAGES.get(route);
  if (page) return <DocsShell page={page} />;
  if (route === ROOT) {
    const first = pagesFor(SECTION)[0];
    if (first) return <Navigate to={first.route} replace />;
  }
  return <NotFound path={pathname} />;
}
