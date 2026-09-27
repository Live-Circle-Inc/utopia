// Top-bar alerts (0005): bell + unread badge + popover panel.
//
// **A popover is not a page**: alerts are something you "glance at in passing", not a place you
// go out of your way to visit. Making it a page forces people away from what they are doing,
// and the price of leaving is that nobody ever goes to look.
//
// One alert = one failure; once written it never changes, and there is no "resolved".
// "Read" is per person -- one person having read it does not mean it should disappear from
// everyone else's unread.
import { type Ref, useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, Search, X } from "lucide-react";

import { api, type AlertGroup } from "../api";
import { S } from "../i18n";
import { toast } from "../toast";
import { Chip, Pager, cn } from "../ui";
import { usePopoverFlip } from "../ui/popoverFlip";

const PAGE = 8;

/** The one line a human reads in the detail list: object name — verbatim error */
function line(d: AlertGroup["lines"][number]): string | null {
  const parts = [d.name ?? d.job, d.error].filter(Boolean);
  return parts.length ? parts.join(" — ") : null;
}

/** Which alerts get a "run again": the kinds where, once the failure is fixed (topping up
 * credit, changing the endpoint), the jobs will not come back on their own */
const REQUEUE_KINDS = new Set(["llm.out_of_credit", "llm.unreachable"]);

function AlertRow({
  g,
  onRead,
  onRequeue,
  requeuing,
}: {
  g: AlertGroup;
  onRead: (g: AlertGroup) => void;
  onRequeue: (g: AlertGroup) => void;
  requeuing: boolean;
}) {
  // A kind we have never seen still has to be displayable: when a new alert source ships the
  // frontend may not have caught up yet, and "there is an alert but I do not recognise it" is
  // far better than "nothing is displayed at all"
  const worded = S.alerts.kinds[g.kind];
  const lines = g.lines.map(line).filter((l): l is string => !!l);
  // count counts the whole group, while lines only brings back the first few -- the difference
  // is the "and N more"
  const rest = g.count - lines.length;
  return (
    // div rather than button: the row also holds an action button, and a button inside a button
    // is invalid HTML
    <div
      role="button"
      tabIndex={0}
      // **A click is what counts as read**, not a hover. The mouse passing over a column of
      // alerts does not mean they were looked at, and once read has landed it never comes back
      // on its own. One click marks this whole group off
      onClick={() => {
        if (g.unread > 0) onRead(g);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" && g.unread > 0) onRead(g);
      }}
      className="w-full text-left flex gap-2.5 px-3.5 py-3 border-b border-white/[0.06] last:border-b-0 hover:bg-white/[0.03] transition-colors cursor-pointer"
    >
      {/* Unread is just a red dot. Outlining the whole row, or giving it a background colour,
          turns the panel into a wall of red once there are many alerts, whereas the dot takes up
          exactly the small amount of space it deserves and is gone once read */}
      <span
        className={cn(
          "mt-[7px] h-1.5 w-1.5 rounded-full shrink-0",
          g.unread > 0 ? "bg-rose-500" : "bg-transparent",
        )}
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5 flex-wrap">
          <span
            className={cn(
              "text-[13px]",
              g.unread > 0 ? "font-medium text-white" : "text-neutral-400",
            )}
          >
            {worded?.title ?? S.alerts.unknownKind(g.kind)}
          </span>
          {g.count > 1 && <Chip tone="neutral">{g.count}</Chip>}
          <Chip tone={g.kb_name ? "neutral" : "violet"}>
            {g.kb_name ?? S.alerts.system}
          </Chip>
        </div>
        {worded && (
          <p className="mt-0.5 text-[11.5px] text-neutral-500">{worded.hint}</p>
        )}
        {lines.length > 0 && (
          <ul className="mt-1 space-y-0.5">
            {lines.map((l, i) => (
              <li key={i} className="text-[11px] text-neutral-400 break-words">
                {l}
              </li>
            ))}
            {rest > 0 && (
              <li className="text-[11px] text-neutral-600">
                {S.alerts.andMore(rest)}
              </li>
            )}
          </ul>
        )}
        {/* The timestamp is the most recent occurrence in the group */}
        <p className="u-num mt-1.5 text-[10.5px] text-neutral-600">
          {new Date(g.latest_at).toLocaleString()}
        </p>
        {/* Carry on once it is fixed: put the jobs that failed inside this failure window back
            on the queue (#216). Running out of credit is the only kind of failure where a human
            does one concrete thing and then wants the work to continue; the action lives on the
            alert, the loop closes right here, and no separate queue page is needed */}
        {REQUEUE_KINDS.has(g.kind) && (
          <button
            type="button"
            className="u-btn u-btn-ghost mt-2 px-2.5 py-1 text-[11px]"
            disabled={requeuing}
            onClick={(e) => {
              e.stopPropagation();
              onRequeue(g);
            }}
          >
            {S.alerts.runAgain}
          </button>
        )}
      </div>
    </div>
  );
}

function Panel({ panelRef }: { panelRef: Ref<HTMLDivElement> }) {
  const [q, setQ] = useState("");
  const [page, setPage] = useState(0);
  const qc = useQueryClient();

  // Back to the first page after a search: sitting on page 4 while looking at a result that
  // only has 2 pages shows an empty panel, and a human reads that as "there are no alerts"
  useEffect(() => {
    setPage(0);
  }, [q]);

  const list = useQuery({
    queryKey: ["alerts", "list", q, page],
    queryFn: () => api.alerts({ q, limit: PAGE, offset: page * PAGE }),
    // Keep the previous page while paging, so the panel height does not collapse and spring back
    placeholderData: (prev) => prev,
  });

  const read = useMutation({
    mutationFn: (g: AlertGroup) =>
      api.alertReadGroup({
        kb_id: g.kb_id,
        kind: g.kind,
        from: g.earliest_at,
        to: g.latest_at,
      }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["alerts"] }),
  });
  const readAll = useMutation({
    mutationFn: () => api.alertsReadAll(),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["alerts"] }),
  });
  // The window starts at this group's earliest failure -- what failed before that is not part
  // of this one
  const requeue = useMutation({
    mutationFn: (g: AlertGroup) =>
      api.requeueJobs(g.kb_id, { failed_since: g.earliest_at }),
    onSuccess: (r) => {
      toast.success(S.alerts.requeued(r.requeued));
      qc.invalidateQueries({ queryKey: ["jobs"] });
    },
    onError: (e) => toast.error(String(e)),
  });

  const groups = list.data?.items ?? [];
  const total = list.data?.total ?? 0;

  return (
    // top-0 rather than top-9: the panel has to grow out of the bell's **own position**, aligned
    // at the top right
    <div
      ref={panelRef}
      className="u-menu-glass absolute right-0 top-0 w-[420px] rounded-xl shadow-2xl z-50 overflow-hidden"
    >
      <div className="flex items-center gap-2 pl-3.5 pr-10 py-2.5 border-b border-white/10">
        <span className="text-[13px] font-medium text-neutral-100">
          {S.alerts.title}
        </span>
      </div>

      {/* The same kit as the library's filter box: input-dark + icon on the left + a clear
          button on the right when there is a value, and Esc to empty it */}
      <div className="px-3.5 py-2.5 border-b border-white/[0.06]">
        <div className="relative">
          <Search
            size={13}
            className="absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-500 pointer-events-none"
          />
          <input
            className="input-dark w-full pl-8 pr-7 py-1.5 text-[13px]"
            placeholder={S.alerts.searchPlaceholder}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setQ("")}
          />
          {q && (
            <button
              onClick={() => setQ("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-neutral-500 hover:text-neutral-200"
            >
              <X size={12} />
            </button>
          )}
        </div>
      </div>

      <div className="max-h-[420px] overflow-y-auto">
        {groups.length === 0 ? (
          <div className="px-3.5 py-8 text-center">
            <p className="text-[13px] text-neutral-300">
              {q ? S.alerts.noMatch : S.alerts.empty}
            </p>
            {!q && (
              <p className="mt-1 text-[11.5px] text-neutral-500">
                {S.alerts.emptyHint}
              </p>
            )}
          </div>
        ) : (
          groups.map((g) => (
            <AlertRow
              key={`${g.kb_id ?? "system"}|${g.kind}|${g.latest_at}`}
              g={g}
              onRead={(x) => read.mutate(x)}
              onRequeue={(x) => requeue.mutate(x)}
              requeuing={requeue.isPending}
            />
          ))
        )}
      </div>

      {/* Footer: whole-list-level actions go with the pager, as far from the cursor as possible */}
      {groups.length > 0 && (
        <div className="flex items-center gap-3 px-3.5 py-2 border-t border-white/[0.06]">
          {groups.some((g) => g.unread > 0) && (
            <button
              className="text-[11.5px] text-neutral-500 hover:text-neutral-200 transition-colors"
              onClick={() => readAll.mutate()}
            >
              {S.alerts.markAllRead}
            </button>
          )}
          <Pager
            className="ml-auto"
            total={total}
            pageSize={PAGE}
            page={page}
            onPage={setPage}
          />
        </div>
      )}
    </div>
  );
}

export function AlertBell() {
  // The same in-place transform as the user menu: the two panels sit right next to each other,
  // and if the animation is even slightly off you can see it by clicking back and forth twice
  const { open, setOpen, close, rootRef, anchorRef, panelRef } =
    usePopoverFlip<HTMLButtonElement, HTMLDivElement>();
  const unread = useQuery({
    queryKey: ["alerts", "unread"],
    queryFn: () => api.alertsUnread(),
    // Push is the main path; this is only the fallback for when the stream drops
    refetchInterval: 120_000,
  });
  const n = unread.data?.unread ?? 0;

  return (
    <div ref={rootRef} className="relative">
      <button
        ref={anchorRef}
        onClick={() => (open ? close() : setOpen(true))}
        title={S.alerts.badgeLabel}
        aria-label={S.alerts.badgeLabel}
        aria-expanded={open}
        // h-7 w-7 square: a button holding nothing but an icon should not be a rectangle.
        // The close button is absolutely positioned at the panel's right-0 top-0 with the same
        // dimensions, so the two line up exactly
        className={cn(
          "relative grid h-7 w-7 place-items-center rounded-lg transition-colors",
          open
            ? "text-neutral-200 bg-white/[0.06]"
            : "text-neutral-500 hover:text-neutral-200 hover:bg-white/[0.05]",
        )}
      >
        <Bell size={15} />
        {/* The badge is a dot too, not a number. "Something happened and I have not looked" is
            binary, and how many there are you find out by opening it; a number would also climb
            with every retry, and once it hits three digits it stretches the bell out of shape */}
        {n > 0 && (
          <span className="absolute top-1 right-1 h-1.5 w-1.5 rounded-full bg-rose-500" />
        )}
      </button>
      {open && (
        <>
          <Panel panelRef={panelRef} />
          {/* The close button is the panel's **sibling**, not its child: put it inside and
              `right-0 top-0` is relative to the panel's padding box, while u-menu-glass has a
              0.667px hairline border (one physical pixel at DPR 1.5), so it is always off by
              that much. Out here, the positioning ancestor is this div wrapping the bell, the
              same box as the bell itself -- the overlap is constructed rather than hoped for.

              After clicking the panel open the cursor is sitting exactly at this spot, so this
              has to be "click again to close". Putting "mark all as read" here would make a
              misclick the default action, and that one clears every alert in every database */}
          <button
            onClick={close}
            title={S.alerts.close}
            aria-label={S.alerts.close}
            className="absolute right-0 top-0 z-[60] grid h-7 w-7 place-items-center rounded-lg text-neutral-500 hover:text-neutral-200 transition-colors"
          >
            <X size={15} />
          </button>
        </>
      )}
    </div>
  );
}
