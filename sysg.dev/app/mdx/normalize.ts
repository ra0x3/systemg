const DIRECTIVE = /^(:{3,})([a-zA-Z][\w-]*)[ \t]+(?!\[)(.+?)[ \t]*$/gm;

export function normalizeDirectives() {
  return {
    name: "sysg-normalize-directives",
    enforce: "pre" as const,
    transform(code: string, id: string) {
      if (!id.endsWith(".mdx")) return null;
      const out = code.replace(DIRECTIVE, (_, colons, name, title) => `${colons}${name}[${title}]`);
      return out === code ? null : { code: out, map: null };
    },
  };
}
