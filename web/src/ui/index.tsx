/* Utopia UI component library -- pages use only the components in here and the semantic
   classes from styles.css, never colour literals. */
import { useEffect, useRef, useState } from "react";
import type {
  ButtonHTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
} from "react";
import {
  ArrowUpRight,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Search as SearchIcon,
} from "lucide-react";
import { S } from "../i18n";

/** The shared base for the in-app left rail: width + glass surface (each page adds its
    own flex/padding on top of it). Sized against the widest one, Ontology (w-64) -- the
    rail holds names, and one notch wider means less truncation. */
export const RAIL_CLS = "w-64 shrink-0 glass-strong border-y-0 border-l-0";

/** The brand wordmark: Marcellus serif, letters fading in left to right; on hover the ↗
    floats out, and clicking goes to the site. The arrow and its offsets are all in em, so
    they scale with the font size where it is used (shared by the login headline and the
    top bar). */
export function Wordmark({ className }: { className?: string }) {
  return (
    <a
      href={S.app.siteUrl}
      target="_blank"
      rel="noreferrer"
      title="utopia.bi"
      className={cn("relative inline-flex text-white", className)}
      style={{ fontFamily: "var(--font-brand)", letterSpacing: "0.06em" }}
    >
      {[...S.app.name].map((ch, i) => (
        <span
          key={i}
          className="u-letter"
          style={{ animationDelay: `${80 + i * 65}ms` }}
        >
          {ch}
        </span>
      ))}
      <ArrowUpRight className="u-mark-arrow" aria-hidden />
    </a>
  );
}

export function cn(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

/* ---------- Button ---------- */
type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "ghost";
  size?: "sm" | "md";
};

export function Button({
  variant = "primary",
  size = "md",
  className,
  ...props
}: ButtonProps) {
  return (
    <button
      className={cn(
        "u-btn",
        variant === "primary" ? "u-btn-primary" : "u-btn-ghost",
        size === "sm" ? "px-3 py-1.5 text-xs" : "px-4 py-2 text-sm",
        className,
      )}
      {...props}
    />
  );
}

/* ---------- Input / Select ---------- */
export function Input({
  className,
  ...props
}: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn("input-dark px-3 py-2 text-sm", className)}
      {...props}
    />
  );
}

/* ---------- Dropdown (hand-rolled, replacing the native select: its popup cannot be themed) ---------- */
export interface DropdownOption {
  value: string;
  label: ReactNode;
}

export function Dropdown({
  value,
  options,
  onChange,
  placeholder,
  className,
  size = "md",
  icon,
  menuLabel,
  footer,
}: {
  value: string;
  options: DropdownOption[];
  onChange: (v: string) => void;
  placeholder?: string;
  className?: string;
  size?: "sm" | "md";
  /** The semantic icon on the left of the trigger (it says "what this level is") */
  icon?: ReactNode;
  /** The small heading at the top of the popup (also used as the trigger's title hint) */
  menuLabel?: string;
  /** A fixed action area at the bottom of the popup (clicking it closes the popup) */
  footer?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = options.find((o) => o.value === value);
  const pad = size === "sm" ? "px-2.5 py-1 text-xs" : "px-3 py-1.5 text-sm";

  return (
    <div ref={rootRef} className={cn("relative", className)}>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        title={menuLabel}
        className={cn(
          "input-dark w-full flex items-center gap-2 text-left",
          pad,
        )}
      >
        {icon && <span className="shrink-0 text-neutral-500">{icon}</span>}
        <span className="flex-1 min-w-0 truncate">
          {current?.label ?? (
            <span className="text-neutral-600">{placeholder ?? ""}</span>
          )}
        </span>
        <ChevronDown
          size={12}
          className={cn(
            "shrink-0 text-neutral-500 transition-transform",
            open && "rotate-180",
          )}
        />
      </button>
      {open && (
        <div className="u-pop u-pop-in u-pop-in-tl absolute z-50 mt-1 w-full rounded-lg shadow-xl overflow-hidden">
          {menuLabel && (
            <div className="px-2.5 pt-2 pb-1 text-[9.5px] font-medium uppercase tracking-[0.1em] text-neutral-600 border-b border-white/5">
              {menuLabel}
            </div>
          )}
          {/* Option rows run flush to the panel edges (no inset): with a single option that one row fills the whole menu */}
          <div className="u-scroll max-h-60 overflow-y-auto">
            {options.map((o) => (
              <button
                key={o.value}
                type="button"
                onClick={() => {
                  onChange(o.value);
                  setOpen(false);
                }}
                className={cn(
                  "w-full flex items-center gap-2 text-left",
                  pad,
                  o.value === value
                    ? "bg-white/[0.12] text-white"
                    : "text-neutral-300 hover:bg-white/[0.06] hover:text-white",
                )}
              >
                <span className="flex-1 min-w-0 truncate">{o.label}</span>
                {o.value === value && (
                  <Check size={12} className="shrink-0 text-neutral-400" />
                )}
              </button>
            ))}
          </div>
          {footer && (
            <div
              className="border-t border-white/10"
              onClick={() => setOpen(false)}
            >
              {footer}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ---------- SearchSelect (a searchable picker, for unbounded object lists -- members,
   parent classes, data sources...)
   The trigger is itself an input: focus opens it, typing filters it; maxVisible caps how
   many get rendered, and past that it prompts you to keep typing to narrow down. Small
   bounded enums (roles / data types...) still use Dropdown -- two clicks and you are
   there, no typing needed. ---------- */
export interface SearchSelectOption {
  value: string;
  /** The primary text: the basis for filtering and for echoing the selection
      (a plain string, it cannot be a node) */
  label: string;
  /** Secondary text (email, connection summary...); it takes part in filtering too
      and is displayed de-emphasised */
  hint?: string;
  /** Hierarchy indent (a tree while browsing; flattened and aligned once you type a filter) */
  indent?: number;
}

export function SearchSelect({
  value,
  options,
  onChange,
  placeholder,
  className,
  size = "md",
  maxVisible = 8,
}: {
  value: string;
  options: SearchSelectOption[];
  onChange: (v: string) => void;
  placeholder?: string;
  className?: string;
  size?: "sm" | "md";
  maxVisible?: number;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const current = options.find((o) => o.value === value);
  const q = query.trim().toLowerCase();
  const matches = q
    ? options.filter((o) =>
        `${o.label} ${o.hint ?? ""}`.toLowerCase().includes(q),
      )
    : options;
  const visible = matches.slice(0, maxVisible);
  const hidden = matches.length - visible.length;

  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
    setQuery("");
    inputRef.current?.blur();
  };

  const pad =
    size === "sm" ? "pl-7 pr-2.5 py-1 text-xs" : "pl-8 pr-3 py-1.5 text-sm";
  const rowPad = size === "sm" ? "px-2.5 py-1 text-xs" : "px-3 py-1.5 text-sm";

  return (
    <div className={cn("relative", className)}>
      <SearchIcon
        size={size === "sm" ? 11 : 13}
        className="absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-600 pointer-events-none"
      />
      <input
        ref={inputRef}
        className={cn("input-dark w-full", pad)}
        value={open ? query : (current?.label ?? "")}
        /* Once open, the current selection moves into the placeholder: you can see the current value while typing */
        placeholder={open ? current?.label || placeholder : placeholder}
        onFocus={() => {
          setOpen(true);
          setQuery("");
          setActive(0);
        }}
        /* Option rows already preventDefault on mousedown (so focus is not stolen); any blur that reaches here is a real departure */
        onBlur={() => setOpen(false)}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            setOpen(false);
            inputRef.current?.blur();
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, visible.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter" && visible[active]) {
            e.preventDefault();
            pick(visible[active].value);
          }
        }}
      />
      {open && (
        <div className="u-pop u-pop-in u-pop-in-tl absolute z-50 mt-1 w-full rounded-lg shadow-xl overflow-hidden">
          {visible.map((o, i) => (
            <button
              key={o.value}
              type="button"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pick(o.value)}
              onMouseEnter={() => setActive(i)}
              className={cn(
                "w-full flex items-center gap-2 text-left",
                rowPad,
                i === active
                  ? "bg-white/[0.08] text-white"
                  : "text-neutral-300",
              )}
            >
              {!q && !!o.indent && (
                <span className="shrink-0" style={{ width: o.indent * 14 }} />
              )}
              <span className="min-w-0 flex-1 truncate">
                {o.label}
                {o.hint && (
                  <span className="ml-2 text-neutral-500">{o.hint}</span>
                )}
              </span>
              {o.value === value && (
                <Check size={12} className="shrink-0 text-neutral-400" />
              )}
            </button>
          ))}
          {visible.length === 0 && (
            <p className={cn(rowPad, "text-neutral-600")}>{S.ui.noMatches}</p>
          )}
          {hidden > 0 && (
            <div
              className={cn(
                rowPad,
                "border-t border-white/5 text-[11px] text-neutral-600",
              )}
            >
              {S.ui.keepTyping(hidden)}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ---------- MultiSearchSelect (the multi-select version of SearchSelect) ---------- */

/**
 * Multi-select + search. The same vocabulary and keyboard handling as [`SearchSelect`],
 * with three differences:
 *
 * - **The selected items are shown above the input**, each with its own remove button.
 *   They are not shown inside the dropdown because one close and they are invisible,
 *   while "which ones did I actually pick" is something you need to see at any time
 * - **Picking does not close it**: multi-select usually means clicking several in a row,
 *   and having to refocus every time is torture
 * - Already-selected options carry a tick in the list; clicking one again deselects it
 *
 * It stays usable when the options run into the hundreds (a large ontology is exactly
 * that order of magnitude) -- which is precisely why it replaced the wall of chips: the
 * height of a chip wall grows linearly with the number of classes, a search box does not.
 */
export function MultiSearchSelect({
  values,
  options,
  onToggle,
  placeholder,
  emptyHint,
  className,
  maxVisible = 8,
}: {
  values: string[];
  options: SearchSelectOption[];
  onToggle: (v: string) => void;
  placeholder?: string;
  /** What to show when nothing is selected at all. An empty multi-select is often
      meaningful ("no limit") rather than simply unfilled */
  emptyHint?: string;
  className?: string;
  maxVisible?: number;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const q = query.trim().toLowerCase();
  const matches = q
    ? options.filter((o) =>
        `${o.label} ${o.hint ?? ""}`.toLowerCase().includes(q),
      )
    : options;
  const visible = matches.slice(0, maxVisible);
  const hidden = matches.length - visible.length;
  const picked = values
    .map((v) => options.find((o) => o.value === v))
    .filter((o): o is SearchSelectOption => !!o);

  const toggle = (v: string) => {
    onToggle(v);
    setQuery("");
    setActive(0);
    inputRef.current?.focus();
  };

  return (
    <div className={cn("relative", className)}>
      {picked.length > 0 && (
        <div className="mb-1 flex flex-wrap gap-1">
          {picked.map((o) => (
            <button
              key={o.value}
              type="button"
              onClick={() => onToggle(o.value)}
              className="group flex items-center gap-1 rounded-full bg-white/[0.10] px-2 py-0.5 text-[11px] text-neutral-200 hover:bg-white/[0.16] transition-colors"
              title={o.hint ?? o.label}
            >
              {o.label}
              <span className="text-neutral-500 group-hover:text-neutral-200">
                ✕
              </span>
            </button>
          ))}
        </div>
      )}
      {picked.length === 0 && emptyHint && (
        <p className="mb-1 text-[11px] text-neutral-600">{emptyHint}</p>
      )}
      <SearchIcon
        size={11}
        className="absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-600 pointer-events-none"
        style={{ top: undefined }}
      />
      <input
        ref={inputRef}
        className="input-dark w-full pl-7 pr-2.5 py-1 text-xs"
        value={query}
        placeholder={placeholder}
        onFocus={() => {
          setOpen(true);
          setActive(0);
        }}
        onBlur={() => setOpen(false)}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            setOpen(false);
            inputRef.current?.blur();
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, visible.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter" && visible[active]) {
            e.preventDefault();
            toggle(visible[active].value);
          } else if (e.key === "Backspace" && !query && picked.length) {
            // Backspace on an empty input deletes the last one -- the same as every
            // other token input out there
            onToggle(picked[picked.length - 1].value);
          }
        }}
      />
      {open && (
        <div className="u-pop u-pop-in u-pop-in-tl absolute z-50 mt-1 w-full rounded-lg shadow-xl overflow-hidden">
          {visible.map((o, i) => (
            <button
              key={o.value}
              type="button"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => toggle(o.value)}
              onMouseEnter={() => setActive(i)}
              className={cn(
                "w-full flex items-center gap-2 text-left px-2.5 py-1 text-xs",
                i === active
                  ? "bg-white/[0.08] text-white"
                  : "text-neutral-300",
              )}
            >
              {!q && !!o.indent && (
                <span className="shrink-0" style={{ width: o.indent * 14 }} />
              )}
              <span className="min-w-0 flex-1 truncate">
                {o.label}
                {o.hint && (
                  <span className="ml-2 text-neutral-500">{o.hint}</span>
                )}
              </span>
              {values.includes(o.value) && (
                <Check size={12} className="shrink-0 text-neutral-400" />
              )}
            </button>
          ))}
          {visible.length === 0 && (
            <p className="px-2.5 py-1 text-xs text-neutral-600">
              {S.ui.noMatches}
            </p>
          )}
          {hidden > 0 && (
            <div className="px-2.5 py-1 text-[11px] text-neutral-600 border-t border-white/5">
              {S.ui.keepTyping(hidden)}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ---------- ColorPicker (a curated palette + a hex fallback; the full colour space is
   deliberately not opened up for entity colours) ----------
   **Change this and you have to change `crates/utopia-store/src/palette.rs`**: the
   colours picked by hand and the colours picked automatically by key must come from the
   same set, otherwise a single graph ends up with two colour schemes. There is a test
   watching over there, so missing one turns it red. */
export const ENTITY_PALETTE = [
  "#7fd0ff",
  "#5fa8ff",
  "#5fd4d0",
  "#63e2b7",
  "#4cc38a",
  "#a8d878",
  "#ffd479",
  "#f2b66d",
  "#ff9d76",
  "#ff8a9e",
  "#ff9daf",
  "#e797d8",
  "#c4a5ff",
  "#9fa8ff",
  "#8ea5bd",
  "#b3b9c4",
];

/**
 * A class key → a colour. **This must agree bit for bit with `color_for_key` in
 * `crates/utopia-store/src/palette.rs`**: when a class is created the frontend picks one
 * by key and displays it, and if the user does not change it that is what gets stored;
 * whereas the import/resolution path is computed by the backend. If the two compute
 * different things, the same key gets a different colour depending on "who created it".
 *
 * FNV-1a plus an avalanche mix. BigInt is used because JS bitwise operations are 32-bit
 * while what is wanted here is 64-bit multiplication -- doing it with Number silently
 * drops the high bits, so the result does not match Rust, and nothing reports an error.
 */
export function colorForKey(key: string): string {
  let h = 0xcbf29ce484222325n;
  const M = (1n << 64n) - 1n;
  for (const b of new TextEncoder().encode(key)) {
    h = (h ^ BigInt(b)) & M;
    h = (h * 0x100000001b3n) & M;
  }
  h = (h ^ (h >> 33n)) & M;
  h = (h * 0xff51afd7ed558ccdn) & M;
  h = (h ^ (h >> 33n)) & M;
  return ENTITY_PALETTE[Number(h % BigInt(ENTITY_PALETTE.length))];
}

export function ColorPicker({
  value,
  onChange,
  shape,
}: {
  value: string;
  onChange: (v: string) => void;
  /** When given, the colour well renders "shape + colour" rather than a solid fill
      (the square has sharp corners, matching the graph nodes) */
  shape?: "circle" | "square";
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const valid = /^#[0-9a-fA-F]{6}$/.test(value);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative inline-block">
      {/* The trigger: a swatch of the current colour (a Figma-style colour well); with shape it renders shape + colour */}
      {shape ? (
        <button
          type="button"
          title={value}
          onClick={() => setOpen(!open)}
          className="h-8 w-14 rounded-lg border border-white/15 hover:border-white/35 transition-colors bg-white/[0.04] grid place-items-center"
        >
          <span
            className={cn("h-3.5 w-3.5", shape === "circle" && "rounded-full")}
            style={{ background: valid ? value : ENTITY_PALETTE[0] }}
          />
        </button>
      ) : (
        <button
          type="button"
          title={value}
          onClick={() => setOpen(!open)}
          className="h-8 w-14 rounded-lg border border-white/15 hover:border-white/35 transition-colors"
          style={{ background: valid ? value : ENTITY_PALETTE[0] }}
        />
      )}
      {open && (
        // An explicit width: the shrink-to-fit width of an absolutely positioned element
        // gets clamped by the 56px containing block of the inline-block trigger
        <div className="u-pop u-pop-in u-pop-in-tl absolute z-50 left-0 top-full mt-2 w-56 rounded-xl p-3 shadow-xl">
          <div className="grid grid-cols-8 gap-1.5 mb-2.5">
            {ENTITY_PALETTE.map((c) => (
              <button
                key={c}
                type="button"
                title={c}
                onClick={() => {
                  onChange(c);
                  setOpen(false);
                }}
                className={cn(
                  "h-5 w-5 rounded-full transition-transform hover:scale-110",
                  value.toLowerCase() === c &&
                    "outline outline-2 outline-white/80 outline-offset-1",
                )}
                style={{ background: c }}
              />
            ))}
          </div>
          <input
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={ENTITY_PALETTE[0]}
            className={cn(
              "input-dark w-full px-2 py-1 text-xs font-mono",
              !valid && "!border-[var(--u-danger)]",
            )}
          />
        </div>
      )}
    </div>
  );
}

/* ---------- Pager (the list pagination bar: hidden automatically below one page) ---------- */
export function Pager({
  total,
  pageSize,
  page,
  onPage,
  /** Overrides the default top margin. The default `mt-3` suits following a list;
      pass `""` to drop it when putting this in a footer that already has padding */
  className = "mt-3",
}: {
  total: number;
  pageSize: number;
  page: number;
  onPage: (p: number) => void;
  className?: string;
}) {
  const pageCount = Math.max(1, Math.ceil(total / pageSize));
  const safe = Math.min(page, pageCount - 1);
  if (total <= pageSize) return null;
  return (
    <div className={cn("flex items-center justify-end gap-2 text-xs text-neutral-500", className)}>
      <span className="u-num">
        {S.library.pageOf(
          safe * pageSize + 1,
          Math.min((safe + 1) * pageSize, total),
          total,
        )}
      </span>
      <button
        onClick={() => onPage(safe - 1)}
        disabled={safe === 0}
        className="u-btn u-btn-ghost h-7 w-7 grid place-items-center rounded-lg"
      >
        <ChevronLeft size={13} />
      </button>
      <button
        onClick={() => onPage(safe + 1)}
        disabled={safe >= pageCount - 1}
        className="u-btn u-btn-ghost h-7 w-7 grid place-items-center rounded-lg"
      >
        <ChevronRight size={13} />
      </button>
    </div>
  );
}

/** Pagination slice helper: returns the current page's rows and the safe page number. */
export function pageSlice<T>(
  items: T[],
  page: number,
  pageSize: number,
): { rows: T[]; safe: number } {
  const pageCount = Math.max(1, Math.ceil(items.length / pageSize));
  const safe = Math.min(page, pageCount - 1);
  return { rows: items.slice(safe * pageSize, (safe + 1) * pageSize), safe };
}

/* ---------- DangerConfirm (confirm dialog for dangerous actions: can require typed text to unlock) ---------- */
export function DangerConfirm({
  title,
  hint,
  requireText,
  confirmLabel,
  cancelLabel,
  busy,
  onConfirm,
  onCancel,
}: {
  title: string;
  hint: string;
  /** The unlock text that has to be typed out verbatim (the resource name, say);
      when absent, confirming is available straight away */
  requireText?: string;
  confirmLabel: string;
  cancelLabel: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState("");
  const unlocked = !requireText || text === requireText;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <div
      className="u-modal-scrim fixed inset-0 z-50 grid place-items-center bg-black/80 backdrop-blur-sm"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div className="u-modal-panel u-modal-in w-[24rem] max-w-[calc(100vw-2rem)] rounded-2xl shadow-2xl p-5">
        <h2 className="text-[15px] font-semibold text-[var(--u-danger)] mb-2">
          {title}
        </h2>
        <p className="text-xs text-neutral-400 leading-relaxed mb-4">{hint}</p>
        {requireText && (
          <input
            autoFocus
            className="input-dark w-full px-3 py-2 text-sm mb-4"
            placeholder={requireText}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        )}
        <div className="flex justify-end gap-2">
          <button
            className="u-btn u-btn-ghost px-3.5 py-1.5 text-xs"
            onClick={onCancel}
          >
            {cancelLabel}
          </button>
          <button
            className="u-btn px-3.5 py-1.5 text-xs font-semibold disabled:opacity-40"
            style={{ background: "var(--u-danger-solid)", color: "#ffffff" }}
            disabled={!unlocked || busy}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ---------- Panel (glass panel) ---------- */
export function Panel({
  strong = false,
  className,
  children,
}: {
  strong?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      className={cn(strong ? "glass-strong" : "glass", "rounded-xl", className)}
    >
      {children}
    </div>
  );
}

/* ---------- Chip (status pill) ---------- */
export type ChipTone =
  "neutral" | "info" | "success" | "warn" | "danger" | "violet";

export function Chip({
  tone = "neutral",
  className,
  title,
  children,
}: {
  tone?: ChipTone;
  className?: string;
  title?: string;
  children: ReactNode;
}) {
  return (
    <span className={cn("u-chip", `u-chip-${tone}`, className)} title={title}>
      {children}
    </span>
  );
}

/* ---------- PageTitle ---------- */
export function PageTitle({
  className,
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  return <h2 className={cn("u-title text-lg", className)}>{children}</h2>;
}

/* ---------- EmptyState ---------- */
export function EmptyState({
  icon,
  children,
}: {
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="text-center">
      <div className="glass mx-auto mb-4 h-14 w-14 rounded-2xl grid place-items-center text-xl font-bold text-neutral-300">
        {icon}
      </div>
      <div className="text-sm text-neutral-500 whitespace-pre-line">
        {children}
      </div>
    </div>
  );
}

/* ---------- Loading / ErrorText ---------- */
export function Loading({ children }: { children: ReactNode }) {
  return <div className="p-8 text-sm text-neutral-500">{children}</div>;
}

export function ErrorText({ children }: { children: ReactNode }) {
  return <p className="text-sm text-rose-400">{children}</p>;
}

/* ---------- GithubMark (lucide has no brand icons, so the official mark is inlined) ---------- */
export function GithubMark({ size = 16 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden
    >
      <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  );
}

/* ---------- SectionMark (section wordmark: Docs, the account layer and so on; letters
   enter one by one, clicking returns to the app) ---------- */
import { Link as RouterLink } from "@tanstack/react-router";
export function SectionMark({ text, title }: { text: string; title: string }) {
  return (
    <RouterLink
      to="/"
      title={title}
      className="relative inline-flex text-white text-[17px]"
      style={{ fontFamily: "var(--font-brand)", letterSpacing: "0.06em" }}
    >
      {[...text].map((ch, i) => (
        <span
          key={i}
          className="u-letter"
          style={{ animationDelay: `${80 + i * 45}ms` }}
        >
          {/* inline-flex collapses a pure-space span -- swap in a non-collapsing space */}
          {ch === " " ? " " : ch}
        </span>
      ))}
    </RouterLink>
  );
}
