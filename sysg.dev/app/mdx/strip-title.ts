export function remarkStripTitle() {
  return (tree: Record<string, unknown>) => {
    const children = tree.children as Record<string, unknown>[];
    const i = children.findIndex((n) => n.type !== "yaml" && n.type !== "mdxjsEsm");
    if (i >= 0 && children[i].type === "heading" && children[i].depth === 1) children.splice(i, 1);
  };
}
