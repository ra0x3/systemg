import { Navigate, useLocation } from "react-router";
import { REDIRECTS } from "~/content/manifest";
import { NotFound } from "~/ds/NotFound";

const MAP = new Map(REDIRECTS.map((r) => [r.from, r.to]));

export default function CatchAll() {
  const { pathname, hash, search } = useLocation();
  const target = MAP.get(pathname.replace(/\/$/, "") || "/");
  if (target) return <Navigate to={`${target}${search}${hash}`} replace />;
  return <NotFound path={pathname} />;
}
