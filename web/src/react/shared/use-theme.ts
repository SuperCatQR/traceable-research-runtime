import { useEffect, useState } from "react";

export type Theme = "light" | "dark";

function initialTheme(): Theme {
  const saved = window.localStorage.getItem("traceable-demo-theme");
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}
export function useTheme() {
  const [theme, setTheme] = useState<Theme>(initialTheme);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    window.localStorage.setItem("traceable-demo-theme", theme);
    return () => {
      delete document.documentElement.dataset.theme;
    };
  }, [theme]);

  return {
    theme,
    toggleTheme: () => setTheme((current) => current === "light" ? "dark" : "light"),
  };
}
