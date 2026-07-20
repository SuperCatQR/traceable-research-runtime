import { useEffect, useState } from "react";

export function useDocumentVisibility(): boolean {
  const [hidden, setHidden] = useState(() => document.hidden);

  useEffect(() => {
    const update = () => setHidden(document.hidden);
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, []);

  return hidden;
}
export function prefersReducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}
