import type { Config } from "@react-router/dev/config";
import { ENTRIES } from "./app/content/manifest";
import { blogRoutes } from "./app/content/releases";

export default {
  ssr: true,
  routeDiscovery: { mode: "initial" },
  prerender: () => ["/", "/404", ...ENTRIES.map((e) => e.route), ...blogRoutes()],
} satisfies Config;
