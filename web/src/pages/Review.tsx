import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { ArrowUpRight } from "lucide-react";
import {
  api,
  type AxiomViolation,
  type ReviewQueue,
  type OntologyDefect,
  type ConflictItem,
  type FactReviewItem,
  type MergeLog,
  type PendingFactItem,
  type ReviewHistoryEvent,
  type ReviewItem,
  type ReviewSide,
  type ViolationResolution,
} from "../api";
import { PendingFactRow, useCanDecide } from "./PendingFacts";
import { S } from "../i18n";
import { useKb, useKbId } from "../kb";
import { Chip, type ChipTone, Pager, RAIL_CLS, cn } from "../ui";

const DUP_PAGE = 6;
const FACT_PAGE = 10;
const MERGE_PAGE = 10;
const CONFLICT_PAGE = 8;

const ym = (iso: string | null) => (iso ? iso.slice(0, 7) : null);

/** `code` or `code|detail`. If it is not found, show it as-is -- legacy rows
 *  still carry the old English prose */
function escalationText(reason: string): string {
  const [code, detail] = reason.split("|");
  const worded = S.review.escalated[code];
  if (!worded) return reason;
  return detail ? S.errDetail(worded, detail) : worded;
}

function dateRange(from: string | null, to: string | null): string | null {
  if (!from && !to) return null;
  return `${ym(from) ?? "…"} → ${ym(to) ?? S.review.ongoing}`;
}

function SideCard({ side }: { side: ReviewSide }) {
  return (
    <div className="flex-1 min-w-0">
      <div className="flex items-center gap-2 mb-1">
        <span
          className="h-2.5 w-2.5 rounded-full shrink-0"
          style={{ backgroundColor: side.color }}
        />
        <span className="text-sm font-medium text-white truncate">
          {side.name}
        </span>
        {side.disambiguator && (
          <span className="text-xs text-neutral-500 truncate">
            · {side.disambiguator}
          </span>
        )}
      </div>
      <div className="text-xs text-neutral-500 mb-2">
        {side.type_label ?? S.graph.untyped} ·{" "}
        {S.review.factsCount(side.degree)}
      </div>
      {side.top_facts.length > 0 ? (
        <ul className="space-y-1">
          {side.top_facts.map((f, i) => (
            <li key={i} className="text-xs text-neutral-400 truncate">
              {f}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-xs text-neutral-600">{S.review.noFacts}</p>
      )}
    </div>
  );
}

function DuplicateCard({
  item,
  busy,
  onDecide,
}: {
  item: ReviewItem;
  busy: boolean;
  onDecide: (action: "merge" | "keep") => void;
}) {
  return (
    <div className="glass rounded-xl p-4">
      <div className="flex gap-4">
        <SideCard side={item.left} />
        <div className="self-center text-neutral-600 text-sm shrink-0">≟</div>
        <SideCard side={item.right} />
      </div>
      <div className="mt-3 pt-3 flex items-center gap-3 border-t border-[var(--u-line)]">
        <span
          className={`u-chip ${item.stage === "human" ? "u-chip-warn" : "u-chip-neutral"}`}
        >
          {item.stage === "human"
            ? S.review.stageHuman
            : S.review.stageAdjudicating}
        </span>
        <span className="text-xs text-neutral-500">
          {S.review.similarity(Math.round(item.score * 100))}
        </span>
        {item.reason && (
          <span className="text-xs text-neutral-600 truncate min-w-0">
            {escalationText(item.reason)}
          </span>
        )}
        <div className="ml-auto flex gap-2 shrink-0">
          <button
            className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
            disabled={busy}
            onClick={() => onDecide("keep")}
          >
            {S.review.keep}
          </button>
          <button
            className="u-btn u-btn-primary px-3 py-1.5 text-xs"
            disabled={busy}
            onClick={() => onDecide("merge")}
          >
            {S.review.merge}
          </button>
        </div>
      </div>
    </div>
  );
}

function FactRow({
  fact,
  busy,
  onConfirm,
  onReject,
}: {
  fact: FactReviewItem;
  busy: boolean;
  onConfirm: () => void;
  onReject: () => void;
}) {
  const range = dateRange(fact.valid_from, fact.valid_to);
  return (
    <div className="glass rounded-xl p-4">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm font-medium text-white">
          {fact.subject_name}
        </span>
        <span className="text-xs text-neutral-500">
          —{" "}
          <span
            className={
              fact.predicate_label === null
                ? "italic text-neutral-600"
                : undefined
            }
          >
            {fact.predicate_label ?? S.graph.unknownPredicate}
          </span>{" "}
          →
        </span>
        <span className="text-sm font-medium text-white">
          {fact.object_name ?? "?"}
        </span>
        {range && <span className="text-xs text-neutral-500">({range})</span>}
        <span className="u-chip u-chip-warn ml-auto">
          {S.review.confidence(Math.round(fact.confidence * 100))}
        </span>
      </div>
      {fact.quote && (
        <p className="mt-2 text-xs text-neutral-500 italic line-clamp-2">
          “{fact.quote}”
        </p>
      )}
      <div className="mt-3 flex gap-2 justify-end">
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
    </div>
  );
}

/** A temporal conflict row: old fact vs new fact, three actions
 *  (Close old / Keep both / Reject new). */
function ConflictRow({
  conflict,
  busy,
  onResolve,
}: {
  conflict: ConflictItem;
  busy: boolean;
  onResolve: (
    action: "close" | "keep" | "reject_new",
    closeAt?: string,
  ) => void;
}) {
  const [closeAt, setCloseAt] = useState("");
  const c = conflict;
  const needsDate = !c.new_valid_from;
  // The close_at input is day-precision, converted to RFC3339
  const closeAtIso = /^\d{4}-\d{2}-\d{2}$/.test(closeAt.trim())
    ? `${closeAt.trim()}T00:00:00Z`
    : undefined;

  return (
    <div className="glass rounded-xl p-4">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm font-medium text-white">{c.old_subject}</span>
        <span className="text-xs text-neutral-500">
          — {c.predicate_label} →
        </span>
        <span className="text-sm font-medium text-white">
          {c.old_object ?? "?"}
        </span>
        {c.old_valid_from && (
          <span className="u-num text-xs text-neutral-500">
            ({S.review.conflictSince(c.old_valid_from.slice(0, 10))})
          </span>
        )}
        <span className="text-xs text-neutral-600">{S.review.conflictVs}</span>
        <span className="text-sm font-medium text-white">{c.new_subject}</span>
        <span className="text-xs text-neutral-500">
          — {c.predicate_label} →
        </span>
        <span className="text-sm font-medium text-white">
          {c.new_object ?? "?"}
        </span>
        {c.new_valid_from && (
          <span className="u-num text-xs text-neutral-500">
            ({S.review.conflictSince(c.new_valid_from.slice(0, 10))})
          </span>
        )}
        <span className="u-chip u-chip-warn ml-auto">
          {S.review.conflictReason[c.reason] ?? c.reason}
        </span>
      </div>
      <div className="mt-3 flex items-center gap-2 justify-end">
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs text-[var(--u-danger)]"
          disabled={busy}
          onClick={() => onResolve("reject_new")}
        >
          {S.review.rejectNew}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onResolve("keep")}
        >
          {S.review.keepBoth}
        </button>
        {needsDate && (
          <input
            className="input-dark u-num w-28 px-2 py-1.5 text-xs text-center"
            placeholder={S.review.closeAtPlaceholder}
            value={closeAt}
            onChange={(e) => setCloseAt(e.target.value)}
          />
        )}
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy || (needsDate && !closeAtIso)}
          onClick={() => onResolve("close", closeAtIso)}
        >
          {c.new_valid_from
            ? S.review.closeOldAt(c.new_valid_from.slice(0, 10))
            : S.review.closeOld}
        </button>
      </div>
    </div>
  );
}

/** A fact row for "the document's new version stopped mentioning it": Reject
 *  (extraction error) or Close at date (this thing is over). */
function UnconfirmedRow({
  fact,
  busy,
  onReject,
  onClose,
}: {
  fact: FactReviewItem;
  busy: boolean;
  onReject: () => void;
  onClose: (validTo: string) => void;
}) {
  const [closeAt, setCloseAt] = useState("");
  const closeAtIso = /^\d{4}-\d{2}-\d{2}$/.test(closeAt.trim())
    ? `${closeAt.trim()}T00:00:00Z`
    : undefined;
  const range = dateRange(fact.valid_from, fact.valid_to);

  return (
    <div className="glass rounded-xl p-4">
      <div className="flex items-center gap-2 flex-wrap">
        <span className="text-sm font-medium text-white">
          {fact.subject_name}
        </span>
        <span className="text-xs text-neutral-500">
          —{" "}
          <span
            className={
              fact.predicate_label === null
                ? "italic text-neutral-600"
                : undefined
            }
          >
            {fact.predicate_label ?? S.graph.unknownPredicate}
          </span>{" "}
          →
        </span>
        <span className="text-sm font-medium text-white">
          {fact.object_name ?? "?"}
        </span>
        {range && (
          <span className="u-num text-xs text-neutral-500">({range})</span>
        )}
      </div>
      {fact.quote && (
        <p className="mt-2 text-xs text-neutral-500 italic line-clamp-2">
          “{fact.quote}”
        </p>
      )}
      <div className="mt-3 flex items-center gap-2 justify-end">
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs text-[var(--u-danger)]"
          disabled={busy}
          onClick={onReject}
        >
          {S.review.reject}
        </button>
        <input
          className="input-dark u-num w-28 px-2 py-1.5 text-xs text-center"
          placeholder={S.review.closeAtPlaceholder}
          value={closeAt}
          onChange={(e) => setCloseAt(e.target.value)}
        />
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy || !closeAtIso}
          onClick={() => closeAtIso && onClose(closeAtIso)}
        >
          {closeAt.trim()
            ? S.review.closeFactAt(closeAt.trim())
            : S.review.closeFact}
        </button>
      </div>
    </div>
  );
}

function MergeRow({
  merge,
  busy,
  onRevert,
}: {
  merge: MergeLog;
  busy: boolean;
  onRevert: () => void;
}) {
  return (
    <div className="glass rounded-xl px-4 py-3 flex items-center gap-3">
      <div className="min-w-0 flex-1">
        <div className="text-sm text-neutral-300 truncate">
          <span className="text-neutral-500">{merge.source_name}</span>
          <span className="text-neutral-600"> → </span>
          <span className="text-white">{merge.target_name}</span>
        </div>
        <div className="text-xs text-neutral-500 truncate">
          {merge.merged_by_name
            ? S.review.mergedBy(merge.merged_by_name)
            : S.review.mergedByAi}
          {" · "}
          {merge.created_at.slice(0, 10)}
          {merge.reason ? ` · ${escalationText(merge.reason)}` : ""}
        </div>
      </div>
      {merge.reverted_at ? (
        <span className="u-chip u-chip-neutral shrink-0">
          {S.review.reverted}
        </span>
      ) : (
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs shrink-0"
          disabled={busy}
          onClick={onRevert}
        >
          {S.review.revert}
        </button>
      )}
    </div>
  );
}

/* ---------- Decision ledger rows ---------- */

const DECISION_TONE: Record<string, ChipTone> = {
  "review.merge": "violet",
  "merge.manual": "violet",
  "review.keep": "neutral",
  "fact.confirm": "success",
  "fact.reject": "danger",
  "conflict.reject_new": "danger",
  "fact.close": "info",
  "conflict.close_old": "info",
  "conflict.keep_both": "neutral",
  "merge.revert": "warn",
};

function DecisionRow({ e }: { e: ReviewHistoryEvent }) {
  // detail is a self-contained snapshot taken at decision time -- no join against live
  // data, so the ledger stays complete even once the fact is deleted
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const d = e.detail as any;
  let text: string;
  if (e.action.startsWith("review.")) text = `${d.left} ≟ ${d.right}`;
  else if (e.action.startsWith("fact."))
    text = `${d.subject} — ${d.predicate ?? "?"} → ${d.object ?? "?"}`;
  else if (e.action.startsWith("conflict."))
    text = `${d.old_subject} — ${d.predicate} → ${d.old_object ?? "?"} · vs · ${
      d.new_object ?? d.new_subject
    }`;
  else text = `${d.source} → ${d.target}`;

  return (
    <div className="glass rounded-xl px-4 py-3 flex items-center gap-3">
      <Chip tone={DECISION_TONE[e.action] ?? "neutral"}>
        {S.review.decisionActions[e.action] ?? e.action}
      </Chip>
      <span className="text-sm text-neutral-300 truncate min-w-0">{text}</span>
      {typeof d.confidence === "number" && (
        <span className="u-num text-xs text-neutral-600 shrink-0">
          {Math.round(d.confidence * 100)}%
        </span>
      )}
      {typeof d.valid_to === "string" && (
        <span className="u-num text-xs text-neutral-600 shrink-0">
          → {d.valid_to.slice(0, 10)}
        </span>
      )}
      <span className="ml-auto shrink-0 text-xs text-neutral-500">
        {e.actor_name ?? S.review.aiActor}
        {" · "}
        <span className="u-num">{e.created_at.slice(0, 10)}</span>
      </span>
    </div>
  );
}

/** One data-mapping definition awaiting a verdict (0011).
 *
 * What it puts front and centre is **"how this number is computed"** -- SQL / expression /
 * table name, taken in that order of priority, because judging whether that is right is
 * exactly the human's job. The concept name and the source are identity; unit is the
 * dimension the answer has to carry. */
/** One place where the ontology contradicts itself. **Two buttons rather than three** --
 *  this tier never looked at the data at all, so there is no "the data is wrong" way out;
 *  it can only be "I went and changed the ontology" or "leave it for now". */
function DefectRow({
  defect: d,
  busy,
  onDecide,
}: {
  defect: OntologyDefect;
  busy: boolean;
  onDecide: (resolution: "fixed" | "accepted") => void;
}) {
  const what = {
    symmetric_and_asymmetric: S.review.defectSymAsym,
    transitive_and_functional: S.review.defectTransFunc,
    subclass_cycle: S.review.defectCycle,
    disjoint_with_ancestor: S.review.defectDisjointAncestor,
    inherits_disjoint: S.review.defectInheritsDisjoint,
    inverse_of_itself: S.review.defectInverseSelf,
    inverse_not_mutual: S.review.defectInverseNotMutual,
    sub_property_cycle: S.review.defectSubPropertyCycle,
    rules_disagree: S.review.defectRulesDisagree,
  }[d.kind];
  const rules = d.kind === "rules_disagree" ? (d.detail.rules ?? []) : [];
  // The consequence of the last two kinds is worth spelling out: an unsatisfiable class
  // raises no error, it just stays empty forever
  const unsatisfiable =
    d.kind === "disjoint_with_ancestor" || d.kind === "inherits_disjoint";
  return (
    <div className="glass rounded-xl p-3">
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-sm text-[var(--u-danger)]">{what}</span>
        {d.subject_label && (
          <span className="text-xs text-neutral-300">{d.subject_label}</span>
        )}
        {d.other_label && (
          <span className="text-xs text-neutral-500">↔ {d.other_label}</span>
        )}
      </div>
      {d.path_labels.length > 0 && (
        <div className="mt-1 text-xs text-neutral-400">
          {d.path_labels.join(" → ")} → {d.path_labels[0]}
        </div>
      )}
      {unsatisfiable && (
        <p className="mt-1 text-xs text-neutral-500">
          {S.review.defectNeverInstantiable}
        </p>
      )}
      {rules.length > 0 && (
        <div className="mt-1 space-y-1 text-xs text-neutral-400">
          <div>{S.review.rulesDisagreeCount(d.detail.count ?? 0)}</div>
          {rules.map((r, i) => (
            <div key={i}>
              <div className="text-neutral-500">
                {S.review.rulesDisagreeRule(r.rule_a, r.via_a, r.rule_b, r.via_b, r.axiom)}
              </div>
              {r.examples.map(([x, y], j) => (
                <div key={j} className="pl-3 text-neutral-400">
                  {x} <span className="text-neutral-600">·</span> {y}
                </div>
              ))}
            </div>
          ))}
        </div>
      )}
      <div className="mt-2 flex gap-1.5">
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("accepted")}
        >
          {S.review.defectAccepted}
        </button>
        <button
          className="u-btn u-btn-primary px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("fixed")}
        >
          {S.review.defectFixed}
        </button>
      </div>
    </div>
  );
}

/** One axiom violation. **Three buttons rather than two** -- the third is a way out unique
 *  to this tier: the contradiction may lie in the definition (the ontology the user imported
 *  declares some property asymmetric, while in their corpus that relation actually runs both
 *  ways), and then what should change is the ontology, not twenty facts. */
/** Extra parameters for a verdict: the close date (fact_closed), which one to retract
 *  (fact_retracted) */
type DecideOpts = { closeAt?: string; factId?: string };

function ViolationRow({
  violation: v,
  busy,
  onDecide,
  onDuplicates,
  onOntology,
}: {
  violation: AxiomViolation;
  busy: boolean;
  onDecide: (resolution: ViolationResolution, opts?: DecideOpts) => void;
  onDuplicates: () => void;
  onOntology: () => void;
}) {
  const what = {
    self_loop: S.review.violationSelfLoop,
    asymmetry: S.review.violationAsymmetry,
    cycle: S.review.violationCycle,
    functional: S.review.violationFunctional,
    signature: S.review.violationSignature,
    derived_contradiction: S.review.violationDerived,
  }[v.kind];
  if (v.kind === "derived_contradiction") {
    return <ContradictionRow {...{ v, what, busy, onDecide, onDuplicates, onOntology }} />;
  }
  // In the reflexive kind the two facts are one and the same -- showing it once is enough,
  // showing it twice looks like a bug
  const single = v.left_fact === v.right_fact;
  // "The data is wrong" has to retract one specific fact: a cycle is listed item by item, a
  // two-fact violation gets one button each, and a single-fact one needs no asking (#202)
  const facts =
    v.path.length > 0
      ? v.path
      : single
        ? [{ id: v.left_fact, text: v.left_text }]
        : [
            { id: v.left_fact, text: v.left_text },
            { id: v.right_fact, text: v.right_text },
          ];
  return (
    <div className="glass rounded-xl p-3">
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-sm text-[var(--u-warn)]">{what}</span>
        {v.predicate && (
          <span className="text-[11px] text-neutral-500">
            {S.review.violationVia(v.predicate)}
          </span>
        )}
        {v.path_len > 0 && (
          <span className="text-[11px] text-neutral-500">
            {S.review.violationPath(v.path_len)}
          </span>
        )}
      </div>
      <div className="mt-1.5 space-y-1">
        {facts.map((f) => (
          <div key={f.id} className="flex items-center gap-2">
            <span className="text-xs text-neutral-300 min-w-0 flex-1">{f.text}</span>
            {!single && (
              <button
                className="u-btn u-btn-ghost shrink-0 px-2 py-1 text-[11px]"
                disabled={busy}
                title={S.review.retractThisHint}
                onClick={() => onDecide("fact_retracted", { factId: f.id })}
              >
                {S.review.retractThis}
              </button>
            )}
          </div>
        ))}
      </div>
      <div className="mt-2 flex gap-1.5 flex-wrap">
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("accepted")}
        >
          {S.review.acceptBoth}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("axiom_relaxed")}
        >
          {S.review.relaxAxiom}
        </button>
        {single && (
          <button
            className="u-btn u-btn-primary px-3 py-1.5 text-xs"
            disabled={busy}
            onClick={() => onDecide("fact_retracted", { factId: v.left_fact })}
          >
            {S.review.retractFact}
          </button>
        )}
      </div>
    </div>
  );
}

/**
 * A derivation colliding with an assertion (0017): the card is one review, and the clue
 * points at a mistake upstream -- an old assertion that should have been closed, two
 * same-named entities that are really one, an extraction that was never sure in the first
 * place. The fix is right there on the card, and the endpoint carries it out for the human.
 */
function ContradictionRow({
  v,
  what,
  busy,
  onDecide,
  onDuplicates,
  onOntology,
}: {
  v: AxiomViolation;
  what: string;
  busy: boolean;
  onDecide: (resolution: ViolationResolution, opts?: DecideOpts) => void;
  onDuplicates: () => void;
  onOntology: () => void;
}) {
  const [closeAt, setCloseAt] = useState("");
  const d = v.detail;
  const hint =
    v.hint === "stale"
      ? S.review.hintStale
      : v.hint === "duplicate"
        ? S.review.hintDuplicate
        : v.hint === "unsure"
          ? S.review.hintUnsure
          : S.review.hintReadBoth;
  return (
    <div className="glass rounded-xl p-3 border border-[color-mix(in_srgb,var(--u-contest)_35%,transparent)]">
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-sm text-[var(--u-contest)]">{what}</span>
        {v.predicate && (
          <span className="text-[11px] text-neutral-500">
            {S.review.violationVia(v.predicate)}
          </span>
        )}
      </div>
      <div className="mt-1.5 space-y-1">
        <div className="text-xs text-neutral-300">
          {S.review.derivedLine(d.subject ?? "?", d.predicate ?? "?", d.object ?? "?")}
          {d.rule && d.via_label && (
            <span className="ml-1.5 text-neutral-500">
              {S.review.derivedBy(d.rule, d.via_label)}
            </span>
          )}
        </div>
        <div className="text-xs text-neutral-300">
          {S.review.assertedLine(v.left_text)}
        </div>
      </div>
      <p className="mt-1.5 text-xs text-neutral-500">{hint}</p>
      <div className="mt-2 flex gap-1.5 flex-wrap items-center">
        <input
          type="date"
          className="input-dark px-2 py-1 text-xs u-num"
          value={closeAt}
          title={S.review.closeAssertion}
          onChange={(e) => setCloseAt(e.target.value)}
        />
        <button
          className="u-btn u-btn-primary px-3 py-1.5 text-xs"
          disabled={busy || !closeAt}
          onClick={() =>
            onDecide("fact_closed", { closeAt: new Date(closeAt).toISOString() })
          }
        >
          {S.review.closeAssertion}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("fact_retracted")}
        >
          {S.review.retractAssertion}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={onDuplicates}
        >
          {S.review.seeDuplicates}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={onOntology}
        >
          {S.review.openOntology}
        </button>
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          disabled={busy}
          onClick={() => onDecide("accepted")}
        >
          {S.review.letBothStand}
        </button>
      </div>
    </div>
  );
}

/* ---------- The page: category rail on the left + one category's content ---------- */

type Sel =
  // Facts extracted from memory, waiting for a human nod (0015). First in the order: these
  // are the human's own words, and what sits in this tier is **not in the graph yet** --
  // every other tier reviews things that are already on the graph
  | "pending"
  | "duplicates"
  | "conflicts"
  | "unconfirmed"
  | "lowconf"
  // Axiom violations (0002 R0). **Kept apart from conflicts**: that tier asks "which one is
  // right", while this one may also answer "the axiom is wrong" -- the ways out differ
  | "violations"
  // The ontology contradicting itself. **Kept apart from violations**: that tier looks at
  // facts, this one looks only at definitions
  | "defects"
  | "decisions"
  | "merges";

/** The tiers that page on the server (the decision ledger has an endpoint of its own) */
const QUEUE_FETCHED: ReviewQueue[] = [
  "pending",
  "duplicates",
  "conflicts",
  "unconfirmed",
  "lowconf",
  "violations",
  "defects",
  "merges",
];

const QUEUE_ORDER: Sel[] = [
  "pending",
  "duplicates",
  "conflicts",
  "unconfirmed",
  "lowconf",
  "violations",
  "defects",
];
const PAGE_SIZE: Record<Sel, number> = {
  pending: FACT_PAGE,
  duplicates: DUP_PAGE,
  conflicts: CONFLICT_PAGE,
  unconfirmed: FACT_PAGE,
  lowconf: FACT_PAGE,
  violations: FACT_PAGE,
  defects: FACT_PAGE,
  merges: MERGE_PAGE,
  decisions: 20,
};

function RailHeader({ label }: { label: string }) {
  return (
    <div className="px-4 pt-4 pb-1.5 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-500">
      {label}
    </div>
  );
}

function RailItem({
  active,
  label,
  count,
  onClick,
  external,
}: {
  active: boolean;
  label: string;
  count: number | null;
  onClick: () => void;
  /** This tier is not handled on this page -- add a "goes elsewhere" marker, so clicking it
   *  does not look like the page ignored you */
  external?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "w-full flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13px] transition-colors",
        active
          ? "u-nav-active"
          : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200",
      )}
    >
      <span className="truncate">{label}</span>
      {external && <ArrowUpRight size={11} className="shrink-0 opacity-50" />}
      {count !== null && (
        <span
          className={cn(
            "ml-auto shrink-0 u-num text-[10.5px]",
            count > 0 ? "text-neutral-400" : "text-neutral-700",
          )}
        >
          {count}
        </span>
      )}
    </button>
  );
}

export function Review() {
  const kbId = useKbId();
  const { kb } = useKb();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  // The panel's contested chip arrives carrying queue / item: land on that tier first, then
  // light up that card
  const search = useSearch({ from: "/app/kb/$kbId/review" });
  const [sel, setSel] = useState<Sel | null>(
    QUEUE_ORDER.includes(search.queue as Sel) ? (search.queue as Sel) : null,
  );
  const [page, setPage] = useState(0);

  // Queue changes are pushed over the SSE event stream (useKbEvents is mounted in Shell), so
  // no polling.
  //
  // **Fetched by tier + page number**: this used to bring all eight queues back at once, 100
  // rows per tier, paginated on the client, so the badges in the left rail were the truncated
  // numbers and anything past page eleven did not exist in the UI. Now the counts come back
  // every time (a server-side COUNT, unaffected by how many rows a page holds) and the
  // content is only the one page of the current tier.
  const queueSel: ReviewQueue = QUEUE_FETCHED.includes(
    (sel ?? "duplicates") as ReviewQueue,
  )
    ? ((sel ?? "duplicates") as ReviewQueue)
    : "duplicates";
  const review = useQuery({
    queryKey: ["review", kb?.id, queueSel, page],
    queryFn: () =>
      api.review(
        kb!.id,
        queueSel,
        PAGE_SIZE[queueSel as Sel],
        page * PAGE_SIZE[queueSel as Sel],
      ),
    enabled: !!kb,
    // Do not flash the previous page to blank while paging -- the counts and the skeleton
    // are still there, only the items change
    placeholderData: (prev) => prev,
  });
  // The decision ledger: paged on the server, fetched only while selected
  const history = useQuery({
    queryKey: ["reviewHistory", kb?.id, page],
    queryFn: () => api.reviewHistory(kb!.id, page),
    enabled: !!kb && sel === "decisions",
  });

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["review", kb?.id] });
    queryClient.invalidateQueries({ queryKey: ["reviewHistory", kb?.id] });
    queryClient.invalidateQueries({ queryKey: ["graph"] });
  };

  const decide = useMutation({
    mutationFn: ({ id, action }: { id: string; action: "merge" | "keep" }) =>
      api.decideReview(kb!.id, id, action),
    onSettled: invalidate,
  });
  const factAction = useMutation({
    mutationFn: ({
      id,
      action,
    }: {
      id: string;
      action: "confirm" | "reject";
    }) =>
      action === "confirm"
        ? api.confirmFact(kb!.id, id)
        : api.rejectFact(kb!.id, id),
    onSettled: invalidate,
  });
  // Facts waiting for a human nod (0015): confirming enters the ledger, rejecting is recorded
  // into rejected_facts
  // A nod is an act of writing to the graph: a Viewer sees the proposals but not the buttons
  // (the same bar as the server's Editor check)
  const canDecidePending = useCanDecide(kb?.id);
  const pendingAction = useMutation({
    mutationFn: ({
      id,
      action,
    }: {
      id: string;
      action: "confirm" | "reject";
    }) => api.decidePending(kb!.id, id, action),
    onSettled: () => {
      invalidate();
      queryClient.invalidateQueries({ queryKey: ["pending", kb?.id] });
    },
  });
  const defectAction = useMutation({
    mutationFn: ({
      id,
      resolution,
    }: {
      id: string;
      resolution: "fixed" | "accepted";
    }) => api.decideDefect(kb!.id, id, resolution),
    onSettled: invalidate,
  });
  const violationAction = useMutation({
    mutationFn: ({
      id,
      resolution,
      opts,
    }: {
      id: string;
      resolution: ViolationResolution;
      opts?: DecideOpts;
    }) => api.decideViolation(kb!.id, id, resolution, opts),
    onSettled: invalidate,
  });
  // The check is synchronous pure computation, so mutate directly without queueing. When it
  // is done the report stays next to the button -- **zero and zero are not the same**: with
  // no axioms it has to say "nothing to judge from", not "no contradictions found"
  const runCheck = useMutation({
    mutationFn: () => api.runConsistencyCheck(kb!.id),
    onSettled: invalidate,
  });
  const revert = useMutation({
    mutationFn: (mergeId: string) => api.revertMerge(kb!.id, mergeId),
    onSettled: invalidate,
  });
  const conflictAction = useMutation({
    mutationFn: ({
      id,
      action,
      closeAt,
    }: {
      id: string;
      action: "close" | "keep" | "reject_new";
      closeAt?: string;
    }) => api.resolveConflict(kb!.id, id, { action, close_at: closeAt }),
    onSettled: invalidate,
  });

  const closeFactAction = useMutation({
    mutationFn: ({ id, validTo }: { id: string; validTo: string }) =>
      api.closeFact(kb!.id, id, validTo),
    onSettled: invalidate,
  });

  // **The badges read the server's COUNT, not the length of the list.** This was the root of
  // the old "164 in the database, 100 in the UI": an array length reflects how many rows a
  // page holds, not how many rows the database holds.
  const c = review.data?.counts;
  // mappings is not a tier on this page (the approving happens on the "Data mappings" page),
  // but the count is taken all the same: an inbox should say "how many are waiting for you"
  const counts: Record<Sel | "mappings", number> = {
    pending: c?.pending ?? 0,
    duplicates: c?.duplicates ?? 0,
    conflicts: c?.conflicts ?? 0,
    unconfirmed: c?.unconfirmed ?? 0,
    lowconf: c?.lowconf ?? 0,
    mappings: c?.mappings ?? 0,
    violations: c?.violations ?? 0,
    defects: c?.defects ?? 0,
    merges: c?.merges ?? 0,
    decisions: history.data?.total ?? 0,
  };
  // The one page of the current tier. **The server has already sliced it**; all that happens
  // here is narrowing the type by tier -- narrow it wrongly and it shows at render time,
  // rather than quietly displaying an empty list
  const rows = review.data?.queue === queueSel ? (review.data.items ?? []) : [];
  const asPending = () => rows as PendingFactItem[];
  const asDuplicates = () => rows as ReviewItem[];
  const asFacts = () => rows as FactReviewItem[];
  const asConflicts = () => rows as ConflictItem[];
  const asViolations = () => rows as AxiomViolation[];
  const asDefects = () => rows as OntologyDefect[];
  const asMerges = () => rows as MergeLog[];
  const queueEmpty = QUEUE_ORDER.every((k) => counts[k] === 0);

  // First batch of data arrives: settle on the first non-empty queue (all-empty lands on
  // duplicates and shows the "clean" copy)
  useEffect(() => {
    if (sel === null && c)
      setSel(QUEUE_ORDER.find((k) => counts[k] > 0) ?? "duplicates");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [c]);

  const select = (s: Sel) => {
    setSel(s);
    setPage(0);
  };

  const active = sel ?? "duplicates";
  const isQueueSel = QUEUE_ORDER.includes(active);

  const SECTION: Record<Sel, { title: string; hint: string | null }> = {
    pending: { title: S.review.pending, hint: S.review.pendingHint },
    duplicates: { title: S.review.duplicates, hint: S.review.duplicatesHint },
    conflicts: { title: S.review.conflicts, hint: S.review.conflictsHint },
    unconfirmed: {
      title: S.review.unconfirmed,
      hint: S.review.unconfirmedHint,
    },
    lowconf: {
      title: S.review.lowConfidence,
      hint: S.review.lowConfidenceHint,
    },
    violations: {
      title: S.review.violations,
      hint: S.review.violationsHint,
    },
    defects: { title: S.review.defects, hint: S.review.defectsHint },
    decisions: { title: S.review.decisionsTitle, hint: S.review.decisionsHint },
    merges: { title: S.review.mergeHistory, hint: null },
  };

  return (
    <div className="h-full flex">
      {/* Left rail: queue categories + history, each with a live count (refreshed by SSE) */}
      {/* `overflow-y-auto`: in a short window this rail's content is taller than the rail,
          and the row at the bottom is the exit to "handle it elsewhere" -- without scrolling
          it gets clipped and is out of reach. `mt-auto` only pushes it to the bottom when
          there is spare space, so the two have to be given together */}
      <aside className={`${RAIL_CLS} flex flex-col overflow-y-auto u-scroll`}>
        <RailHeader label={S.review.tabQueue} />
        <div className="px-2 space-y-0.5">
          <RailItem
            active={active === "pending"}
            label={S.review.railPending}
            count={counts.pending}
            onClick={() => select("pending")}
          />
          <RailItem
            active={active === "duplicates"}
            label={S.review.railDuplicates}
            count={counts.duplicates}
            onClick={() => select("duplicates")}
          />
          <RailItem
            active={active === "conflicts"}
            label={S.review.railConflicts}
            count={counts.conflicts}
            onClick={() => select("conflicts")}
          />
          <RailItem
            active={active === "unconfirmed"}
            label={S.review.railUnconfirmed}
            count={counts.unconfirmed}
            onClick={() => select("unconfirmed")}
          />
          <RailItem
            active={active === "lowconf"}
            label={S.review.railLowConfidence}
            count={counts.lowconf}
            onClick={() => select("lowconf")}
          />
          <RailItem
            active={active === "violations"}
            label={S.review.railViolations}
            count={counts.violations}
            onClick={() => select("violations")}
          />
          <RailItem
            active={active === "defects"}
            label={S.review.railDefects}
            count={counts.defects}
            onClick={() => select("defects")}
          />
        </div>
        <RailHeader label={S.review.tabHistory} />
        <div className="px-2 space-y-0.5">
          <RailItem
            active={active === "decisions"}
            label={S.review.railDecisions}
            count={null}
            onClick={() => select("decisions")}
          />
          <RailItem
            active={active === "merges"}
            label={S.review.railMerges}
            count={counts.merges}
            onClick={() => select("merges")}
          />
        </div>

        {/* Data mappings: **it belongs to neither group, so it sits alone at the bottom.**
            The seven tiers above all ask "is this piece of knowledge right", while a mapping
            definition asks "how is this number computed" (0011 already split it out at the
            data layer); the two tiers below are the stream of what has been done on this
            page, and a mapping decision never enters `review_history` (that only scoops up
            review./fact./conflict./merge., while a mapping records mapping.decided).
            **The count stays** -- an inbox should say "how many are waiting for you", but the
            work itself happens on the page that has the context */}
        <div className="mt-auto border-t border-white/5 px-2 py-2">
          <RailItem
            active={false}
            label={S.review.railMappings}
            count={counts.mappings}
            onClick={() =>
              navigate({ to: "/kb/$kbId/mappings", params: { kbId } })
            }
            external
          />
        </div>
      </aside>

      {/* Right side: only the selected category at a time, a single pager */}
      <div className="flex-1 min-w-0 overflow-y-auto u-scroll px-8 py-6">
        <div className="max-w-4xl">
          {review.isPending && (
            <p className="text-sm text-neutral-500">{S.nav.loading}</p>
          )}
          {review.isError && (
            <p className="text-sm text-rose-400">
              {(review.error as Error).message}
            </p>
          )}

          {review.data && (
            <section>
              {/* Page-level heading: same tier as Library/KB Settings (text-lg), not a card head */}
              <h2 className="u-title text-lg mb-1">{SECTION[active].title}</h2>
              {SECTION[active].hint && (
                <p className="text-xs text-neutral-500 mb-3">
                  {SECTION[active].hint}
                </p>
              )}

              {/* Empty state: the entire backlog cleared vs one category cleared. **Except
                  the axiom tier** -- its own sentence has to separate "checked, no
                  contradictions" from "not checked yet", and the generic empty state cannot
                  say that difference */}
              {isQueueSel &&
                active !== "violations" &&
                active !== "defects" &&
                counts[active] === 0 && (
                  <div className="glass rounded-xl p-10 text-center text-sm text-neutral-500">
                    {queueEmpty ? S.review.empty : S.review.categoryEmpty}
                  </div>
                )}

              {active === "duplicates" && counts.duplicates > 0 && (
                <div className="space-y-3">
                  {asDuplicates().map((item) => (
                    <DuplicateCard
                      key={item.id}
                      item={item}
                      busy={
                        decide.isPending && decide.variables?.id === item.id
                      }
                      onDecide={(action) =>
                        decide.mutate({ id: item.id, action })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "conflicts" && counts.conflicts > 0 && (
                <div className="space-y-3">
                  {asConflicts().map((c) => (
                    <ConflictRow
                      key={c.id}
                      conflict={c}
                      busy={
                        conflictAction.isPending &&
                        conflictAction.variables?.id === c.id
                      }
                      onResolve={(action, closeAt) =>
                        conflictAction.mutate({ id: c.id, action, closeAt })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "unconfirmed" && counts.unconfirmed > 0 && (
                <div className="space-y-3">
                  {asFacts().map((fact) => (
                    <UnconfirmedRow
                      key={fact.id}
                      fact={fact}
                      busy={
                        (factAction.isPending &&
                          factAction.variables?.id === fact.id) ||
                        (closeFactAction.isPending &&
                          closeFactAction.variables?.id === fact.id)
                      }
                      onReject={() =>
                        factAction.mutate({ id: fact.id, action: "reject" })
                      }
                      onClose={(validTo) =>
                        closeFactAction.mutate({ id: fact.id, validTo })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "pending" && counts.pending > 0 && (
                <div className="space-y-3">
                  {asPending().map((fact) => (
                    <PendingFactRow
                      key={fact.id}
                      fact={fact}
                      canDecide={canDecidePending}
                      busy={
                        pendingAction.isPending &&
                        pendingAction.variables?.id === fact.id
                      }
                      onConfirm={() =>
                        pendingAction.mutate({ id: fact.id, action: "confirm" })
                      }
                      onReject={() =>
                        pendingAction.mutate({ id: fact.id, action: "reject" })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "lowconf" && counts.lowconf > 0 && (
                <div className="space-y-3">
                  {asFacts().map((fact) => (
                    <FactRow
                      key={fact.id}
                      fact={fact}
                      busy={
                        factAction.isPending &&
                        factAction.variables?.id === fact.id
                      }
                      onConfirm={() =>
                        factAction.mutate({ id: fact.id, action: "confirm" })
                      }
                      onReject={() =>
                        factAction.mutate({ id: fact.id, action: "reject" })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "defects" && (
                <div className="space-y-3">
                  {counts.defects === 0 && (
                    <div className="glass rounded-xl p-10 text-center text-sm text-neutral-500">
                      {S.review.categoryEmpty}
                    </div>
                  )}
                  {asDefects().map((d) => (
                    <DefectRow
                      key={d.id}
                      defect={d}
                      busy={
                        defectAction.isPending &&
                        defectAction.variables?.id === d.id
                      }
                      onDecide={(resolution) =>
                        defectAction.mutate({ id: d.id, resolution })
                      }
                    />
                  ))}
                </div>
              )}

              {active === "violations" && (
                <div className="space-y-3">
                  {/* The button lives inside this tier, not in the page header: only someone
                      looking at this tier wants to rerun it. The report stays next to the
                      button -- an empty result has to say whether it means "no
                      contradictions" or "nothing to judge from" */}
                  <div className="flex items-center gap-3">
                    {/* ghost rather than solid white: this is the same kind of thing as
                        "probe mappings" -- manually triggering one analysis, not this page's
                        primary action. Keep the one solid white for the real decisions
                        (confirm / merge) */}
                    <button
                      className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
                      disabled={runCheck.isPending}
                      onClick={() => runCheck.mutate()}
                    >
                      {runCheck.isPending
                        ? S.review.checking
                        : S.review.runCheck}
                    </button>
                    {runCheck.data && (
                      <span className="text-xs text-neutral-500">
                        {/* Three outcomes, three sentences. **`found` is not the number to
                            report**: a rerun recomputes the ones already adjudicated, so
                            saying "3 contradictions" while the list has only one left looks
                            like the UI dropped something */}
                        {runCheck.data.predicates_with_axioms === 0
                          ? S.review.checkNoAxioms
                          : runCheck.data.inserted > 0
                            ? S.review.checkFound(runCheck.data.inserted)
                            : runCheck.data.found > 0
                              ? S.review.checkNothingNew
                              : S.review.checkClean(runCheck.data.edges)}
                      </span>
                    )}
                  </div>
                  {counts.violations === 0 && !runCheck.data && (
                    <div className="glass rounded-xl p-10 text-center text-sm text-neutral-500">
                      {S.review.checkNeverRun}
                    </div>
                  )}
                  {asViolations().map((v) => (
                    <div
                      key={v.id}
                      className={
                        v.id === search.item
                          ? "rounded-xl ring-1 ring-[var(--u-contest)]"
                          : undefined
                      }
                    >
                    <ViolationRow
                      violation={v}
                      busy={
                        violationAction.isPending &&
                        violationAction.variables?.id === v.id
                      }
                      onDecide={(resolution, opts) =>
                        violationAction.mutate({ id: v.id, resolution, opts })
                      }
                      onDuplicates={() => select("duplicates")}
                      onOntology={() => navigate({ to: "/ontology" })}
                    />
                    </div>
                  ))}
                </div>
              )}

              {active === "merges" &&
                (counts.merges === 0 ? (
                  <div className="glass rounded-xl p-10 text-center text-sm text-neutral-500">
                    {S.review.historyEmpty}
                  </div>
                ) : (
                  <div className="space-y-2">
                    {asMerges().map((m) => (
                      <MergeRow
                        key={m.id}
                        merge={m}
                        busy={revert.isPending && revert.variables === m.id}
                        onRevert={() => revert.mutate(m.id)}
                      />
                    ))}
                  </div>
                ))}

              {active === "decisions" &&
                (history.isPending ? (
                  <p className="text-sm text-neutral-500">{S.nav.loading}</p>
                ) : (history.data?.total ?? 0) === 0 ? (
                  <div className="glass rounded-xl p-10 text-center text-sm text-neutral-500">
                    {S.review.decisionsEmpty}
                  </div>
                ) : (
                  <div className="space-y-2">
                    {(history.data?.events ?? []).map((e) => (
                      <DecisionRow key={e.id} e={e} />
                    ))}
                  </div>
                ))}

              {/* A single pager: queue/merges slice on the client, decisions page on the server */}
              {active !== "decisions" && (
                <Pager
                  total={counts[active]}
                  pageSize={PAGE_SIZE[active]}
                  page={page}
                  onPage={setPage}
                />
              )}
              {active === "decisions" && (
                <Pager
                  total={history.data?.total ?? 0}
                  pageSize={PAGE_SIZE.decisions}
                  page={page}
                  onPage={setPage}
                />
              )}
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
