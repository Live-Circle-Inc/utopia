/* Page title system: `{domain} | {page}`, brand first (a product decision; the price is that once
   many tabs are truncated they all share the same prefix).
   Domains: Utopia (the main app) / Utopia Charter (docs) / Utopia Persona (account). */
import { useEffect } from "react";

export function usePageTitle(...parts: (string | null | undefined)[]) {
  const title = parts.filter(Boolean).join(" | ");
  useEffect(() => {
    if (title) document.title = title;
  }, [title]);
}
