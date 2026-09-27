/* The history of what we believed about an entity (the record timeline).
   Orthogonal to the Timeline view in the same panel: that axis asks "when was this true in
   the real world", this one asks "when did we come to think so, and when did we change our
   mind". The data comes from the rows in the append-only ledger that entity_detail filters
   out with invalidated_at IS NULL. */
import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { FileText, Merge, PencilLine, Tag, Undo2 } from "lucide-react";
import { api, type EntityHistoryEvent } from "../api";
import { S } from "../i18n";
import { useKbId } from "../kb";
import { Pager } from "../ui";

const PER = 20;

/** Event kind → icon and tone (semantic colour is reserved for "overturned", the rest stay
 *  neutral) */
const KIND_ICON = {
  asserted: FileText,
  corrected: PencilLine,
  rejected: Undo2,
  // A merge is not a retraction: the content went into another assertion without losing a word
  merged: Merge,
  // A retype is not a change of fact: the node on the graph changed class, not one fact moved
  retyped: Tag,
  retype_reverted: Undo2,
} as const;

const KIND_TONE: Record<string, string> = {
  asserted: "text-neutral-500",
  corrected: "text-[var(--u-warn)]",
  rejected: "text-[var(--u-danger)]",
  merged: "text-neutral-500",
  retyped: "text-neutral-500",
  retype_reverted: "text-[var(--u-warn)]",
};

/* These two functions are **deliberately different**, do not "unify" them -- they render two
   different kinds of time.

   `ymd` gives the **record instant** (when we came to think so): that is a real instant, and
   it should be displayed in the timezone of whoever is looking. This used to slice the ISO
   string here too, which amounts to displaying in UTC -- a revision made by someone at UTC+8
   before eight in the morning would show up in the history as the previous day.

   `ym` gives **world time** (when this was true): it comes from a statement in a document
   ("took office in May 2019"), it is a **calendar date, not an instant**, and it never had a
   timezone to begin with. Slicing the ISO string is exactly reading back, in UTC, the day
   that was stored; converting to local time would instead make a reader at UTC-5 see the
   previous month. */
const ymd = (iso: string) => {
  const d = new Date(iso);
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${day}`;
};
const ym = (iso: string | null) => (iso ? iso.slice(0, 7) : null);

/** The object: entity name first, otherwise the summary/value of the literal (an attribute
 *  fact) */
function objectText(e: EntityHistoryEvent): string {
  if (e.other_name) return e.other_name;
  const v = e.object_value as { summary?: unknown; value?: unknown } | null;
  const raw = v?.summary ?? v?.value;
  return raw === undefined || raw === null ? "—" : String(raw);
}

/** What this change did to the validity interval (an event on the record-time axis, changing a
 *  boundary on the validity axis) */
function intervalNote(e: EntityHistoryEvent): string | null {
  if (e.kind === "corrected") {
    return e.valid_to ? S.graph.historyClosedAt(ym(e.valid_to)!) : null;
  }
  const from = ym(e.valid_from);
  if (!from) return null;
  return e.valid_to
    ? `${from} → ${ym(e.valid_to)}`
    : `${S.graph.historyFrom(from)} · ${S.graph.historyOngoing}`;
}

function EventRow({ e }: { e: EntityHistoryEvent }) {
  const kbId = useKbId();
  const Icon = KIND_ICON[e.kind] ?? FileText;
  const note = intervalNote(e);
  return (
    <div className="flex gap-2.5 px-2 py-2">
      <Icon size={13} className={`mt-0.5 shrink-0 ${KIND_TONE[e.kind] ?? ""}`} />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-1.5 flex-wrap">
          <span className="text-[11px] font-medium text-neutral-300">
            {S.graph.historyKind[e.kind] ?? e.kind}
          </span>
          {note && <span className="u-num text-[11px] text-neutral-500">{note}</span>}
        </div>
        {/* A retype event has neither predicate nor object, so the body becomes the two ends
            of the class change. An empty start = retyped from "untyped", the most common
            kind after 0009 */}
        {e.kind === "retyped" || e.kind === "retype_reverted" ? (
          <div className="mt-0.5 text-[12.5px] text-neutral-400 truncate">
            <span className="text-neutral-500">
              {e.from_type_label ?? S.graph.untyped} →{" "}
            </span>
            <span className="text-neutral-200">{e.to_type_label}</span>
          </div>
        ) : (
          <div className="mt-0.5 text-[12.5px] text-neutral-400 truncate">
            <span className="text-neutral-500">
              {e.direction === "in" ? "← " : ""}
              <span className={e.predicate_label === null ? "italic text-neutral-600" : undefined}>
                {e.predicate_label ?? S.graph.unknownPredicate}
              </span>
              {e.direction === "in" ? "" : " →"}
            </span>{" "}
            <span className="text-neutral-200">{objectText(e)}</span>
          </div>
        )}
        <div className="mt-0.5 flex items-center gap-1.5 text-[10.5px] text-neutral-600">
          <span className="u-num">{ymd(e.at)}</span>
          <span>·</span>
          {/* Attribution: a person's name, or the engine (written by extraction / closed
              automatically by temporal reconciliation) */}
          <span>{e.actor_name ?? S.graph.historyEngine}</span>
          {e.filename && e.document_id && (
            <>
              <span>·</span>
              <Link
                to="/kb/$kbId/doc/$docId"
                params={{ kbId, docId: e.document_id }}
                search={{}}
                className="truncate hover:text-neutral-300"
                title={e.quote ?? e.filename}
              >
                {e.filename}
              </Link>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

export function EntityHistory({ kbId, entityId }: { kbId: string; entityId: string }) {
  const [page, setPage] = useState(0);
  useEffect(() => setPage(0), [entityId]);
  const q = useQuery({
    queryKey: ["entityHistory", kbId, entityId, page],
    queryFn: () => api.entityHistory(kbId, entityId, page, PER),
  });

  const total = q.data?.total ?? 0;
  if (q.isPending) return <p className="p-2 text-sm text-neutral-500">{S.nav.loading}</p>;
  // Only "not a single one" counts as empty. On the record-time axis the first assertion is itself
  // an event -- "when, and from which document, did we learn this" is half of what this axis
  // is there to answer
  if (total === 0) return <p className="p-2 text-xs text-neutral-500">{S.graph.historyEmpty}</p>;

  return (
    <div>
      <p className="px-2 pb-1.5 text-[11px] text-neutral-600">{S.graph.historyHint}</p>
      <div className="divide-y divide-white/[0.06]">
        {/* fact_id ?? at in the key: a retype event has no fact_id */}
        {(q.data?.events ?? []).map((e) => (
          <EventRow key={`${e.fact_id ?? e.at}-${e.kind}`} e={e} />
        ))}
      </div>
      <Pager total={total} pageSize={PER} page={page} onPage={setPage} />
    </div>
  );
}
