import { visit } from "unist-util-visit";

const KINDS: Record<string, string> = {
  info: "info",
  note: "note",
  tip: "info",
  warning: "warning",
  danger: "warning",
  caution: "warning",
};

function textOf(node: Record<string, unknown>): string {
  if (node.type === "text" || node.type === "inlineCode") return String(node.value ?? "");
  const children = (node.children as Record<string, unknown>[]) || [];
  return children.map(textOf).join("");
}

export function remarkCallouts() {
  return (tree: unknown) => {
    visit(tree as never, (node: Record<string, unknown>) => {
      if (node.type !== "containerDirective" && node.type !== "leafDirective") return;
      const kind = KINDS[node.name as string];
      if (!kind) return;

      const children = (node.children as Record<string, unknown>[]) || [];
      const i = children.findIndex((c) => (c.data as Record<string, unknown> | undefined)?.directiveLabel);
      const label = i >= 0 ? textOf(children[i]) : undefined;
      if (i >= 0) children.splice(i, 1);

      const data = (node.data as Record<string, unknown>) || (node.data = {});
      data.hName = "callout";
      data.hProperties = label ? { type: kind, label } : { type: kind };
    });
  };
}
