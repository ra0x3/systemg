import mdx from "@mdx-js/rollup";
import { reactRouter } from "@react-router/dev/vite";
import rehypeShiki from "@shikijs/rehype";
import rehypeSlug from "rehype-slug";
import remarkDirective from "remark-directive";
import remarkFrontmatter from "remark-frontmatter";
import remarkGfm from "remark-gfm";
import remarkMdxFrontmatter from "remark-mdx-frontmatter";
import { defineConfig } from "vite";
import { remarkCallouts } from "./app/mdx/directives";
import { normalizeDirectives } from "./app/mdx/normalize";
import { remarkStyleProps } from "./app/mdx/style-props";
import { remarkStripTitle } from "./app/mdx/strip-title";
import { remarkToc } from "./app/mdx/toc";
import { shikiOptions } from "./app/mdx/shiki";

export default defineConfig({
  plugins: [
    normalizeDirectives(),
    {
      enforce: "pre",
      ...mdx({
        providerImportSource: "@mdx-js/react",
        remarkPlugins: [
          remarkFrontmatter,
          [remarkMdxFrontmatter, { name: "frontmatter" }],
          remarkGfm,
          remarkDirective,
          remarkCallouts,
          remarkStripTitle,
          remarkStyleProps,
          remarkToc,
        ],
        rehypePlugins: [rehypeSlug, [rehypeShiki, shikiOptions]],
      }),
    },
    reactRouter(),
  ],
  resolve: { tsconfigPaths: true },
});
