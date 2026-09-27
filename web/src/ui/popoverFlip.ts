// "Morph in place" (FLIP) for the top-bar popovers: the panel sits right on top of the
// trigger button, squashed to the button's real bounds on the first frame (999px radius),
// then transitions to the panel's full shape on the next -- the button "grows into" the
// panel; on close it shrinks back the other way.
//
// **Factored out into one shared hook** instead of letting the user menu and the alerts
// each write their own: these two panels sit right next to each other, so if the duration
// or the easing is off by even a hair, two clicks back and forth make it obvious.
import { useEffect, useLayoutEffect, useRef, useState } from "react";

const OPEN_MS = 260;
const CLOSE_MS = 190;

function reduced(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Returns `{ open, setOpen, close, anchorRef, panelRef }`.
 *
 * `close()` plays the retract animation before unmounting; to close right now (because we
 * navigated away, say) just call `setOpen(false)`.
 * Click-outside and Esc are already wired up, hung off `rootRef`.
 */
export function usePopoverFlip<A extends HTMLElement, P extends HTMLElement>(
  /** The corner the morph is anchored to. **Name whichever edge the panel hugs**: panels
   *  on the right of the top bar hug the top-right corner, a panel that hugs the left (the
   *  legend's "+N types", say) has to say "top left", otherwise it grows leftwards out of
   *  the right edge and looks like it flew in from somewhere else */
  origin: "top right" | "top left" | "bottom left" = "top right",
) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const anchorRef = useRef<A>(null);
  const panelRef = useRef<P>(null);
  const closingRef = useRef(false);

  useLayoutEffect(() => {
    if (!open) return;
    const panel = panelRef.current;
    const anchor = anchorRef.current;
    if (!panel || !anchor || reduced()) return;
    const a = anchor.getBoundingClientRect();
    const p = panel.getBoundingClientRect();
    if (p.width < 1 || p.height < 1) return;
    panel.style.transformOrigin = origin;
    panel.style.transform = `scale(${a.width / p.width}, ${a.height / p.height})`;
    panel.style.borderRadius = "999px";
    panel.style.opacity = "0.35";
    let done: number | undefined;
    const raf = requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        panel.style.transition = `transform ${OPEN_MS}ms cubic-bezier(0.16,1,0.3,1), border-radius ${OPEN_MS}ms cubic-bezier(0.16,1,0.3,1), opacity 0.18s ease`;
        panel.style.transform = "scale(1, 1)";
        panel.style.borderRadius = "12px";
        panel.style.opacity = "1";
        // Once the animation is done, **wipe the inline styles clean**; do not leave a
        // `scale(1,1)` sitting there. An identity transform looks harmless, but it still
        // spawns a compositing layer, and then the absolutely positioned children inside
        // the panel snap to device pixels once -- at DPR 1.5 that is a 0.67px shift, and
        // the close button has to land **exactly on top of** the button that opened it;
        // being off by a single physical pixel shows
        done = window.setTimeout(() => {
          panel.style.transition = "";
          panel.style.transform = "";
          panel.style.borderRadius = "";
          panel.style.opacity = "";
          panel.style.transformOrigin = "";
        }, OPEN_MS + 20);
      }),
    );
    return () => {
      cancelAnimationFrame(raf);
      if (done !== undefined) window.clearTimeout(done);
    };
  }, [open, origin]);

  const close = () => {
    const panel = panelRef.current;
    const anchor = anchorRef.current;
    if (closingRef.current) return;
    if (!panel || !anchor || reduced()) {
      setOpen(false);
      return;
    }
    closingRef.current = true;
    panel.style.transformOrigin = origin;
    const a = anchor.getBoundingClientRect();
    // offsetWidth/Height are layout sizes, unaffected by the current transform --
    // getBoundingClientRect would hand back values that are already scaled down, so each
    // close would shrink further than the last
    panel.style.transition = `transform ${CLOSE_MS}ms cubic-bezier(0.5,0,0.9,0.4), border-radius ${CLOSE_MS}ms cubic-bezier(0.5,0,0.9,0.4), opacity 0.16s ease`;
    panel.style.transform = `scale(${a.width / panel.offsetWidth}, ${a.height / panel.offsetHeight})`;
    panel.style.borderRadius = "999px";
    panel.style.opacity = "0.3";
    window.setTimeout(() => {
      closingRef.current = false;
      setOpen(false);
    }, CLOSE_MS);
  };

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node))
        close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return { open, setOpen, close, rootRef, anchorRef, panelRef };
}
