import githubLight from "shiki/themes/github-light.mjs";
import tokyoNight from "shiki/themes/tokyo-night.mjs";

const PLAIN = [
  "constant.other.option",
  "constant.other.option.dash.shell",
  "string.unquoted.argument",
  "string.unquoted.argument.shell",
];

function neutralise(theme: typeof githubLight) {
  const fg = theme.colors?.["editor.foreground"] ?? "#000000";
  return {
    ...theme,
    name: `sysg-${theme.name}`,
    tokenColors: [...(theme.tokenColors ?? []), { scope: PLAIN, settings: { foreground: fg } }],
  };
}

export const shikiOptions = {
  themes: { light: neutralise(githubLight), dark: neutralise(tokyoNight) },
  defaultColor: false,
  cssVariablePrefix: "--sysg-code-",
  transformers: [
    {
      pre(node: Record<string, unknown>) {
        const props = node.properties as Record<string, unknown>;
        const opts = (this as unknown as { options: { lang: string; meta?: { __raw?: string } } }).options;
        props["data-lang"] = opts.lang;
        const raw = opts.meta?.__raw?.trim();
        if (raw) props["data-title"] = raw;
      },
    },
  ],
  langs: [
    "bash",
    "shell",
    "sh",
    "yaml",
    "json",
    "toml",
    "ini",
    "xml",
    "markdown",
    "dockerfile",
    "rust",
    "python",
    "typescript",
    "javascript",
    "text",
  ],
};
