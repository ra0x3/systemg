import GithubSlugger from "github-slugger";
import { visit } from "unist-util-visit";

function text(node: Record<string, unknown>): string {
  const children = (node.children as Record<string, unknown>[]) || [];
  if (node.type === "text" || node.type === "inlineCode") return String(node.value ?? "");
  return children.map(text).join("");
}

export function remarkToc() {
  return (tree: Record<string, unknown>) => {
    const items: { depth: number; id: string; title: string }[] = [];
    // The heading ids themselves come from rehype-slug, which slugs with
    // github-slugger. Anything else here drifts on the first heading holding
    // punctuation — "Linux / macOS" is `linux--macos` to one and `linux-macos`
    // to the other — and the contents links land nowhere.
    const slugger = new GithubSlugger();
    visit(tree as never, "heading", (node: Record<string, unknown>) => {
      const depth = node.depth as number;
      if (depth < 2 || depth > 3) return;
      const title = text(node);
      if (title) items.push({ depth, id: slugger.slug(title), title });
    });
    (tree.children as unknown[]).push({
      type: "mdxjsEsm",
      value: "",
      data: {
        estree: {
          type: "Program",
          sourceType: "module",
          body: [
            {
              type: "ExportNamedDeclaration",
              specifiers: [],
              source: null,
              declaration: {
                type: "VariableDeclaration",
                kind: "const",
                declarations: [
                  {
                    type: "VariableDeclarator",
                    id: { type: "Identifier", name: "toc" },
                    init: {
                      type: "ArrayExpression",
                      elements: items.map((item) => ({
                        type: "ObjectExpression",
                        properties: Object.entries(item).map(([key, value]) => ({
                          type: "Property",
                          kind: "init",
                          method: false,
                          shorthand: false,
                          computed: false,
                          key: { type: "Identifier", name: key },
                          value: { type: "Literal", value },
                        })),
                      })),
                    },
                  },
                ],
              },
            },
          ],
        },
      },
    });
  };
}
