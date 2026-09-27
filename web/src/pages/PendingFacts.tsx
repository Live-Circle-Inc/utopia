/* Facts waiting for a human nod (docs/decisions/0015).
   The triples one remember extracts land in the pending queue first, not on the graph; only
   once a human nods here do they go into the ledger.
   **Original sentence on top, triple underneath**: listing only the triple asks people to
   judge it out of thin air -- in practice, for that `Acme --?--> Shenzhen` row, one look at
   the original sentence was enough to know it should be rejected.
   Two places share this one row component: the "pending" bucket on the Review page, and the
   card that follows the remember step in Chat. */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type PendingFactItem } from "../api";
import { S } from "../i18n";
import { toast } from "../toast";

function ym(iso: string | null): string | null {
  return iso ? iso.slice(0, 7) : null;
}

/** A persisted memory carries `[YYYY-MM-DD HH:MM] ` in front of the body (the timestamp
 *  `memory::append_episode` adds).
 *  The card wants the sentence itself; the timestamp is there for indexing, so strip it */
function sentence(quote: string): string {
  return quote.replace(/^\[\d{4}-\d{2}-\d{2}(?: \d{2}:\d{2})?\]\s*/, "");
}

function objectText(f: PendingFactItem): string {
  if (f.object_name) return f.object_name;
  const v = f.object_value;
  if (!v) return "?";
  if (v.summary) return v.summary;
  const val = v.value === undefined || v.value === null ? "?" : String(v.value);
  return v.unit ? `${val} ${v.unit}` : val;
}

/** Nodding writes to the graph, so it starts at Editor -- the same bar as the server's
 *  `require_kb(Role::Editor)`.
 *  A Viewer sees the proposals but not the buttons: a button that is lit but dead to the
 *  click amounts to making people guess whether they have permission.
 *  The query key is shared with the Library page (`kbOne`), so this costs no extra call */
export function useCanDecide(kbId: string | undefined): boolean {
  const kbDetail = useQuery({
    queryKey: ["kbOne", kbId],
    queryFn: () => api.kbDetail(kbId!),
    enabled: !!kbId,
  });
  return ["editor", "admin", "owner"].includes(kbDetail.data?.my_role ?? "");
}

export function PendingFactRow({
  fact,
  busy,
  canDecide,
  onConfirm,
  onReject,
}: {
  fact: PendingFactItem;
  busy: boolean;
  canDecide: boolean;
  onConfirm: () => void;
  onReject: () => void;
}) {
  const from = ym(fact.valid_from);
  const to = ym(fact.valid_to);
  const range = from || to ? `${from ?? "…"} → ${to ?? S.review.ongoing}` : null;
  return (
    <div className="glass rounded-xl p-4">
      {/* Original sentence first. They said it themselves, and it is what the call rests on */}
      <p className="text-xs text-neutral-400 italic">“{sentence(fact.quote)}”</p>
      <div className="mt-2.5 flex items-center gap-2 flex-wrap">
        <span className="text-sm font-medium text-white">{fact.subject_name}</span>
        <span className="text-xs text-neutral-500">
          —{" "}
          {fact.predicate_label ? (
            <span>{fact.predicate_label}</span>
          ) : (
            /* The ontology has no such relation: show the raw wording, in italics to mark
               that it is not a term from the vocabulary (0010) */
            <span
              className="italic text-neutral-600"
              title={S.review.pendingNoPredicate}
            >
              {fact.proposed_predicate ?? S.graph.unknownPredicate}
            </span>
          )}{" "}
          →
        </span>
        <span className="text-sm font-medium text-white">{objectText(fact)}</span>
        {range && <span className="text-xs text-neutral-500">({range})</span>}
        {!fact.predicate_label && (
          <span className="u-chip u-chip-warn ml-auto">{S.review.pendingNoPredicateChip}</span>
        )}
      </div>
      <div className="mt-3 flex items-center gap-2">
        {fact.proposed_by_name && (
          <span className="text-[11px] text-neutral-600">
            {S.review.pendingSaidBy(fact.proposed_by_name)}
          </span>
        )}
        {canDecide && (
          <div className="ml-auto flex gap-2">
            <button
              className="u-btn u-btn-ghost px-3 py-1.5 text-xs text-[var(--u-danger)]"
              disabled={busy}
              onClick={onReject}
            >
              {S.review.reject}
            </button>
            <button
              className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
              disabled={busy}
              onClick={onConfirm}
            >
              {S.review.confirm}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** The confirmation card that follows the remember step.
 *  Extraction is async, so the card only grows in when the job finishes (the SSE `pending`
 *  event invalidates the query and it refetches);
 *  replaying an old session fetches per chunk the same way -- anything not yet nodded at
 *  still shows, and once it is all dealt with the card takes up no room. */
export function NodCard({ kbId, chunkId }: { kbId: string; chunkId: string }) {
  const queryClient = useQueryClient();
  const canDecide = useCanDecide(kbId);
  const q = useQuery({
    queryKey: ["pending", kbId, chunkId],
    queryFn: () => api.pendingForChunk(kbId, chunkId),
  });
  const decide = useMutation({
    mutationFn: ({ id, action }: { id: string; action: "confirm" | "reject" }) =>
      api.decidePending(kbId, id, action),
    onError: (e: Error) => toast.error(e.message),
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: ["pending", kbId] });
      queryClient.invalidateQueries({ queryKey: ["review", kbId] });
    },
  });
  const items = q.data?.items ?? [];
  if (items.length === 0) return null;
  return (
    <div className="my-2 space-y-2">
      <div className="text-xs text-neutral-500">{S.review.nodCardTitle(items.length)}</div>
      {items.map((f) => (
        <PendingFactRow
          key={f.id}
          fact={f}
          busy={decide.isPending && decide.variables?.id === f.id}
          canDecide={canDecide}
          onConfirm={() => decide.mutate({ id: f.id, action: "confirm" })}
          onReject={() => decide.mutate({ id: f.id, action: "reject" })}
        />
      ))}
    </div>
  );
}
