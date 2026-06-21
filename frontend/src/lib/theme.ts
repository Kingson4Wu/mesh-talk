import { create } from "zustand";

export type Theme = "light" | "dark";

const KEY = "mesh-talk-theme";

function read(): Theme {
  if (typeof localStorage === "undefined") return "dark";
  return localStorage.getItem(KEY) === "light" ? "light" : "dark";
}

function apply(t: Theme) {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("dark", t === "dark");
}

const initial = read();
apply(initial); // run on import so the theme is set before/at first paint

interface ThemeState {
  theme: Theme;
  toggle: () => void;
}

export const useTheme = create<ThemeState>((set, get) => ({
  theme: initial,
  toggle: () => {
    const next: Theme = get().theme === "dark" ? "light" : "dark";
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY, next);
    apply(next);
    set({ theme: next });
  },
}));
