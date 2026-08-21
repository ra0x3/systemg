export type Mode = "light" | "dark" | "system";

export const MODES: Mode[] = ["system", "light", "dark"];
export const STORAGE_KEY = "sysg-theme";

export const themeInitScript = `(function(){try{var m=localStorage.getItem("${STORAGE_KEY}");document.documentElement.dataset.theme=(m==="light"||m==="dark"||m==="system")?m:"system"}catch(e){}})()`;

export function readMode(): Mode {
  if (typeof document === "undefined") return "system";
  const value = document.documentElement.dataset.theme;
  return value === "light" || value === "dark" ? value : "system";
}

export function applyMode(mode: Mode) {
  document.documentElement.dataset.theme = mode;
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    return;
  }
}
