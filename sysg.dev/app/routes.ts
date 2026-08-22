import { index, type RouteConfig, route } from "@react-router/dev/routes";

export default [
  index("routes/home.tsx"),
  route("docs/*", "routes/docs.tsx"),
  route("reference/*", "routes/reference.tsx"),
  route("blog", "routes/blog.tsx"),
  route("blog/:date/:slug", "routes/blog-article.tsx"),
  route("blog/:slug", "routes/blog-post.tsx"),
  route("*", "routes/catchall.tsx"),
] satisfies RouteConfig;
