import { visit } from "unist-util-visit";

function parse(style: string) {
  const out: [string, string][] = [];
  for (const part of style.split(";")) {
    const i = part.indexOf(":");
    if (i < 0) continue;
    const name = part.slice(0, i).trim();
    const value = part.slice(i + 1).trim();
    if (!name || !value) continue;
    out.push([name.startsWith("--") ? name : name.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase()), value]);
  }
  return out;
}

function objectExpression(pairs: [string, string][]) {
  return {
    type: "ObjectExpression",
    properties: pairs.map(([key, value]) => ({
      type: "Property",
      kind: "init",
      method: false,
      shorthand: false,
      computed: false,
      key: { type: "Literal", value: key },
      value: { type: "Literal", value },
    })),
  };
}

export function remarkStyleProps() {
  return (tree: unknown) => {
    visit(tree as never, (node: Record<string, unknown>) => {
      if (node.type !== "mdxJsxFlowElement" && node.type !== "mdxJsxTextElement") return;
      const attrs = node.attributes as Record<string, unknown>[] | undefined;
      if (!attrs) return;
      for (const attr of attrs) {
        if (attr.type !== "mdxJsxAttribute" || attr.name !== "style" || typeof attr.value !== "string") continue;
        const expression = objectExpression(parse(attr.value));
        attr.value = {
          type: "mdxJsxAttributeValueExpression",
          value: "",
          data: {
            estree: {
              type: "Program",
              sourceType: "module",
              body: [{ type: "ExpressionStatement", expression }],
            },
          },
        };
      }
    });
  };
}
