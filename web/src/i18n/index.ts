// The UI language: **the person reading picks it**, without going through the backend (see
// docs/decisions/0004). Same category as dark mode -- a setting like the concurrency cap
// describes the deployment, the UI language describes the reader.
//
// The locale is resolved in this one file only. The other 600-odd places just consume `S`; not
// one of them reads where it came from. Whether to follow the browser later, whether to add a
// per-user override, is a change here, not across those 600 places.
import { en, type Strings } from "./en";
import { zh } from "./zh";

export type { Strings };

export const LANGS = ["en", "zh"] as const;
export type Lang = (typeof LANGS)[number];

/** A language's own name, never translated -- in the switcher, "中文" is the signpost for the people who cannot read English */
export const LANG_NAMES: Record<Lang, string> = { en: "English", zh: "中文" };

const BUNDLES: Record<Lang, Strings> = { en, zh };
const KEY = "utopia.lang";

function detect(): Lang {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved && (LANGS as readonly string[]).includes(saved))
      return saved as Lang;
  } catch {
    // localStorage throws in private mode -- fall back to English, do not let the first paint die
  }
  // **Do not follow the browser language.** The Chinese bundle is still trailing behind the
  // English one, and the price of guessing the language wrong is that a Chinese user sees a
  // half-finished UI instead of a complete English one. Once zh has caught up, put the
  // navigator.language line back -- that is one line of code.
  // A choice the person made themselves still counts (the block above), and the switcher is in
  // the user menu as always
  return "en";
}

export const lang: Lang = detect();

/** The active bundle. Fixed once at module load -- so a module-level constant (`const X = S.a.b`) is fine too */
export const S: Strings = BUNDLES[lang];

/**
 * Switching the language does a full page reload, not a remount.
 *
 * One of the 635 references already evaluates at module top level (`ROLE_OPTIONS` in
 * `Members.tsx`), and people will keep writing it that way. A remount would leave those stuck
 * in the old language **without any error**; a reload re-evaluates the whole module graph, and
 * the only cost is one refresh -- nobody switches language more than a few times a year.
 */
export function setLang(next: Lang) {
  if (next === lang) return;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // If it cannot be stored, it applies to this session only -- better than nothing happening
  }
  location.reload();
}
