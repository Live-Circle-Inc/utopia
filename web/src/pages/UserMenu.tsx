/* User menu: the avatar pill at the right of the top bar + a popover panel (profile / system
   administration / sign out). Shared by Shell (the KB workspace) and AccountShell (the account
   layer). */
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import {
  BookMarked,
  Check,
  Languages,
  LogOut,
  ShieldCheck,
  UserRound,
} from "lucide-react";
import { api, type User } from "../api";
import { usePopoverFlip } from "../ui/popoverFlip";
import { LANGS, LANG_NAMES, S, lang, setLang } from "../i18n";

/** Initials avatar: neutral grey ground (zero colour cast in the chrome); Latin takes the first
 *  letter of the first two words, CJK the first two characters. */
export function Avatar({ name, size = 24 }: { name: string; size?: number }) {
  const trimmed = name.trim();
  const words = trimmed.split(/\s+/).filter(Boolean);
  const initials =
    words.length >= 2
      ? (words[0][0] + words[1][0]).toUpperCase()
      : [...trimmed].slice(0, 2).join("").toUpperCase();
  return (
    <span
      className="inline-grid place-items-center rounded-full bg-white/[0.09] border border-white/10 text-neutral-200 select-none shrink-0"
      style={{ width: size, height: size, fontSize: Math.round(size * 0.38) }}
    >
      {initials}
    </span>
  );
}

export function UserMenu({ user }: { user: User }) {
  // Morph in place (FLIP): the pill "grows into" the panel. Shared implementation, see
  // ui/popoverFlip -- the alert bell sits right next to it, and writing it out twice would
  // sooner or later drift apart by a hair
  const { open, setOpen, close, rootRef, anchorRef, panelRef } =
    usePopoverFlip<HTMLButtonElement, HTMLDivElement>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const go = (to: string) => {
    setOpen(false);
    navigate({ to });
  };

  const logout = async () => {
    await api.logout();
    queryClient.clear();
    navigate({ to: "/login" });
  };

  // Rows run all the way to the panel edge (same vocabulary as Dropdown): no padding on the
  // container, the row itself sets the height
  const item =
    "w-full flex items-center gap-2.5 px-3.5 py-2.5 text-[13px] text-neutral-300 hover:bg-white/[0.06] hover:text-white transition-colors";

  return (
    <div ref={rootRef} className="relative">
      <button
        ref={anchorRef}
        onClick={() => setOpen((v) => !v)}
        className={`flex items-center gap-2 rounded-full py-1 pl-1 pr-3 transition-[background-color,opacity] duration-150 ${
          open ? "opacity-0" : "hover:bg-white/[0.06]"
        }`}
      >
        <Avatar name={user.display_name} size={24} />
        <span className="text-sm text-neutral-300">{user.display_name}</span>
      </button>

      {open && (
        <div
          ref={panelRef}
          className="u-menu-glass absolute right-0 top-0 w-64 rounded-xl shadow-2xl z-50 overflow-hidden"
        >
          {/* Identity header: click it again to shrink back into the pill */}
          <div
            onClick={close}
            className="flex items-center gap-3 px-3.5 py-3 border-b border-white/10 cursor-pointer hover:bg-white/[0.04] transition-colors"
          >
            <Avatar name={user.display_name} size={32} />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="truncate text-[13px] font-medium text-neutral-100">
                  {user.display_name}
                </span>
                {user.is_admin && (
                  <span className="u-chip u-chip-neutral !text-[10px] !px-1.5">
                    {S.account.adminChip}
                  </span>
                )}
              </div>
              <div className="truncate text-[11px] text-neutral-500">
                {user.email}
              </div>
            </div>
          </div>

          <div>
            <button onClick={() => go("/account")} className={item}>
              <UserRound size={13} className="text-neutral-500" />
              {S.account.profile}
            </button>
            {/* Everyone gets to see this: every visible KB + my own role in each of them */}
            <button onClick={() => go("/account/kbs")} className={item}>
              <BookMarked size={13} className="text-neutral-500" />
              {S.account.kbsNav}
            </button>
            {user.is_admin && (
              <button onClick={() => go("/admin")} className={item}>
                <ShieldCheck size={13} className="text-neutral-500" />
                {S.account.administration}
              </button>
            )}
          </div>

          {/* UI language: whoever is looking decides it, the backend is not involved
              (docs/decisions/0004). Each option is written **in its own language** -- it is
              precisely the people who cannot read English who need to recognise "中文" */}
          <div className="border-t border-white/10">
            <div className="flex items-center gap-2.5 px-3.5 pt-2.5 pb-1 text-[11px] text-neutral-500">
              <Languages size={13} className="text-neutral-500" />
              {S.account.language}
            </div>
            {LANGS.map((l) => (
              <button key={l} onClick={() => setLang(l)} className={item}>
                <span className="w-[13px] shrink-0">
                  {l === lang && (
                    <Check size={13} className="text-neutral-400" />
                  )}
                </span>
                {LANG_NAMES[l]}
              </button>
            ))}
          </div>

          <div className="border-t border-white/10">
            <button
              onClick={logout}
              className={`${item} !text-[var(--u-danger)]`}
            >
              <LogOut size={13} />
              {S.nav.signOut}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
