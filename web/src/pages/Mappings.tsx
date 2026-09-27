// Data mappings: what a business concept corresponds to in the database, and how it is
// computed.
//
// **This page did not exist before**, and yet what it governs was always there: definitions
// were proposed by the exploration job, confirmed under "Review", and then sank into the
// ask-your-data system prompt -- after which nobody could see them, let alone change them.
// `mappings::revise`, along with its revision table, had zero callers from the day it was
// created.
//
// Approval stays on the same endpoint (`review/mappings/{id}`, which already writes the audit
// trail); what moved is the UI, not the logic: judging whether a definition is right takes
// seeing the table structure, and that lives on this page.
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Database, History, Pencil, Plus } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import { api, type ConceptMapping } from "../api";
import { S } from "../i18n";
import { useKb } from "../kb";
import { toast } from "../toast";
import {
  Chip,
  type ChipTone,
  EmptyState,
  ErrorText,
  Loading,
  Pager,
  PageTitle,
  SearchSelect,
  cn,
} from "../ui";

const PAGE = 25;

type StatusFilter = "all" | "proposed" | "confirmed" | "rejected";

const TONE: Record<string, ChipTone> = {
  proposed: "warn",
  confirmed: "success",
  rejected: "neutral",
};

/** "How is this number computed". Takes one of SQL / expression / table name, in that order
 *  of priority -- all three answer the same question, and SQL is the most specific while the
 *  table name is the coarsest */
const howComputed = (m: ConceptMapping) =>
  m.sql ?? m.expr ?? m.table_name ?? null;

const statusLabel = (s: string) =>
  s === "proposed"
    ? S.mapping.filterProposed
    : s === "confirmed"
      ? S.mapping.filterConfirmed
      : S.mapping.filterRejected;

export function Mappings() {
  const { kb } = useKb();
  const queryClient = useQueryClient();
  const [tab, setTab] = useState<"definitions" | "sources">("definitions");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(0);

  const data = useQuery({
    queryKey: ["mappings", kb?.id, status, q, page],
    queryFn: () =>
      api.mappings(kb!.id, {
        status: status === "all" ? undefined : status,
        q: q || undefined,
        limit: PAGE,
        offset: page * PAGE,
      }),
    enabled: !!kb,
  });

  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: ["mappings", kb?.id] });

  if (!kb) return <Loading>{S.nav.loading}</Loading>;

  const counts = data.data?.counts;
  const FILTERS: { key: StatusFilter; label: string; n?: number }[] = [
    { key: "all", label: S.mapping.filterAll },
    { key: "proposed", label: S.mapping.filterProposed, n: counts?.proposed },
    {
      key: "confirmed",
      label: S.mapping.filterConfirmed,
      n: counts?.confirmed,
    },
    { key: "rejected", label: S.mapping.filterRejected, n: counts?.rejected },
  ];

  return (
    <div className="p-6 max-w-4xl mx-auto space-y-4">
      <div>
        <PageTitle>{S.mapping.title}</PageTitle>
        <p className="mt-1 text-xs text-neutral-500">{S.mapping.hint}</p>
      </div>

      {/* The segmented control uses the site-wide pattern (`bg-white/10` for selected +
          a muted unselected state), not `u-btn-primary` -- that is the solid white of a
          primary action, and used here every tab would look like a call to action */}
      <div className="flex w-fit rounded-lg overflow-hidden border border-white/10">
        {(["definitions", "sources"] as const).map((t) => (
          <button
            key={t}
            className={cn(
              "px-3 py-1.5 text-xs transition-colors",
              tab === t
                ? "bg-white/10 text-neutral-100"
                : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300",
            )}
            onClick={() => setTab(t)}
          >
            {t === "definitions"
              ? S.mapping.tabDefinitions
              : S.mapping.tabSources}
          </button>
        ))}
      </div>

      {tab === "sources" ? (
        <DataSources kbId={kb.id} onExplored={refresh} />
      ) : (
        <>
          <div className="flex items-center gap-2 flex-wrap">
            <div className="flex rounded-lg overflow-hidden border border-white/10">
              {FILTERS.map((f) => (
                <button
                  key={f.key}
                  className={cn(
                    "px-2.5 py-1.5 text-xs transition-colors flex items-center gap-1.5",
                    status === f.key
                      ? "bg-white/10 text-neutral-100"
                      : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300",
                  )}
                  onClick={() => {
                    setStatus(f.key);
                    setPage(0);
                  }}
                >
                  {f.label}
                  {f.n != null && (
                    <span
                      className={cn(
                        "u-num",
                        status === f.key
                          ? "text-neutral-300"
                          : "text-neutral-600",
                      )}
                    >
                      {f.n}
                    </span>
                  )}
                </button>
              ))}
            </div>
            <input
              className="u-input flex-1 min-w-40 px-3 py-1 text-xs"
              placeholder={S.mapping.searchPlaceholder}
              value={q}
              onChange={(e) => {
                setQ(e.target.value);
                setPage(0);
              }}
            />
          </div>

          {status === "rejected" && (
            <p className="text-xs text-neutral-600">{S.mapping.rejectedHint}</p>
          )}

          {data.isPending ? (
            <Loading>{S.nav.loading}</Loading>
          ) : data.data && data.data.items.length === 0 ? (
            <div className="py-12">
              <EmptyState icon={<Database size={20} />}>
                {q || status !== "all"
                  ? S.mapping.emptyFiltered
                  : S.mapping.empty}
              </EmptyState>
            </div>
          ) : (
            <div className="space-y-2">
              {data.data?.items.map((m) => (
                <MappingCard
                  key={m.id}
                  kbId={kb.id}
                  mapping={m}
                  onChanged={refresh}
                />
              ))}
            </div>
          )}
          <Pager
            total={data.data?.total ?? 0}
            pageSize={PAGE}
            page={page}
            onPage={setPage}
          />
        </>
      )}
    </div>
  );
}

/** One definition. **Only the ones nobody has ruled on get the confirm/reject buttons** --
 *  the ones already ruled on get "edit", because changing a definition and calling it for the
 *  first time are two different things: the former has to leave a trace (revisions), the
 *  latter does not. */
function MappingCard({
  kbId,
  mapping: m,
  onChanged,
}: {
  kbId: string;
  mapping: ConceptMapping;
  onChanged: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [showHistory, setShowHistory] = useState(false);

  const decide = useMutation({
    mutationFn: (s: "confirmed" | "rejected") =>
      api.decideMapping(kbId, m.id, s),
    onSuccess: onChanged,
    onError: (e: unknown) => toast.error((e as Error).message),
  });

  const how = howComputed(m);
  return (
    <div className="glass rounded-xl p-3">
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-sm text-neutral-200">{m.concept_name}</span>
        <span className="text-[11px] text-neutral-500">{m.source}</span>
        {m.unit && (
          <span className="text-[11px] text-neutral-500">[{m.unit}]</span>
        )}
        {m.derived && (
          <Chip tone="warn" className="text-[11px]">
            {S.mapping.derivedBadge}
          </Chip>
        )}
        <span className="flex-1" />
        <Chip tone={TONE[m.status]} className="text-[11px]">
          {statusLabel(m.status)}
        </Chip>
      </div>

      <div
        className={cn(
          "mt-1 u-num text-xs break-all",
          how ? "text-neutral-400" : "text-neutral-600",
        )}
      >
        {how ?? S.mapping.noDefinition}
      </div>
      {m.summary && (
        <p className="mt-1 text-xs text-neutral-500">{m.summary}</p>
      )}

      {editing ? (
        <EditForm
          kbId={kbId}
          mapping={m}
          onDone={() => {
            setEditing(false);
            onChanged();
          }}
          onCancel={() => setEditing(false)}
        />
      ) : (
        <div className="mt-2 flex items-center gap-1.5 flex-wrap">
          {m.status === "proposed" && (
            <>
              <button
                className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
                disabled={decide.isPending}
                onClick={() => decide.mutate("rejected")}
              >
                {S.mapping.reject}
              </button>
              <button
                className="u-btn u-btn-primary px-3 py-1.5 text-xs"
                disabled={decide.isPending}
                onClick={() => decide.mutate("confirmed")}
              >
                {S.mapping.approve}
              </button>
            </>
          )}
          <button
            className="u-btn u-btn-ghost px-2 py-1 text-xs flex items-center gap-1"
            onClick={() => setEditing(true)}
          >
            <Pencil size={11} />
            {S.mapping.edit}
          </button>
          <button
            className="u-btn u-btn-ghost px-2 py-1 text-xs flex items-center gap-1"
            onClick={() => setShowHistory((v) => !v)}
          >
            <History size={11} />
            {S.mapping.history}
          </button>
        </div>
      )}

      {showHistory && <RevisionList kbId={kbId} mappingId={m.id} />}
    </div>
  );
}

function EditForm({
  kbId,
  mapping: m,
  onDone,
  onCancel,
}: {
  kbId: string;
  mapping: ConceptMapping;
  onDone: () => void;
  onCancel: () => void;
}) {
  const [table, setTable] = useState(m.table_name ?? "");
  const [expr, setExpr] = useState(m.expr ?? "");
  const [sql, setSql] = useState(m.sql ?? "");
  const [unit, setUnit] = useState(m.unit ?? "");
  const [summary, setSummary] = useState(m.summary ?? "");
  const [derived, setDerived] = useState(m.derived);

  const save = useMutation({
    mutationFn: () =>
      api.reviseMapping(kbId, m.id, {
        table_name: table,
        expr,
        sql,
        unit,
        summary,
        derived,
      }),
    onSuccess: onDone,
    onError: (e: unknown) => toast.error((e as Error).message),
  });

  const nothing = !table.trim() && !expr.trim() && !sql.trim();
  const field = (
    label: string,
    value: string,
    set: (v: string) => void,
    mono = false,
  ) => (
    <label className="block">
      <span className="text-[11px] text-neutral-500">{label}</span>
      <input
        className={cn(
          "u-input w-full px-2 py-1 text-xs mt-0.5",
          mono && "u-num",
        )}
        value={value}
        onChange={(e) => set(e.target.value)}
      />
    </label>
  );

  return (
    <div className="mt-3 space-y-2 border-t border-white/5 pt-3">
      <p className="text-[11px] text-neutral-500">{S.mapping.editTitle}</p>
      <div className="grid grid-cols-2 gap-2">
        {field(S.mapping.fieldTable, table, setTable, true)}
        {field(S.mapping.fieldUnit, unit, setUnit)}
      </div>
      {field(S.mapping.fieldExpr, expr, setExpr, true)}
      {field(S.mapping.fieldSql, sql, setSql, true)}
      {field(S.mapping.fieldSummary, summary, setSummary)}
      <label className="flex items-center gap-2 text-xs text-neutral-400">
        <input
          type="checkbox"
          checked={derived}
          onChange={(e) => setDerived(e.target.checked)}
        />
        {S.mapping.fieldDerived}
      </label>
      {nothing && (
        <p className="text-xs text-[var(--u-danger)]">{S.mapping.needOne}</p>
      )}
      <div className="flex gap-1.5">
        <button
          className="u-btn u-btn-ghost px-3 py-1.5 text-xs"
          onClick={onCancel}
        >
          {S.mapping.cancel}
        </button>
        <button
          className="u-btn u-btn-primary px-3 py-1.5 text-xs"
          disabled={nothing || save.isPending}
          onClick={() => save.mutate()}
        >
          {S.mapping.save}
        </button>
      </div>
    </div>
  );
}

/** Revision history. **Nobody had read the revision table since the day it was created** --
 *  0006 said it was kept so that "how was this number computed last quarter" could be
 *  answered, and this is where that sentence is made good. */
function RevisionList({
  kbId,
  mappingId,
}: {
  kbId: string;
  mappingId: string;
}) {
  const revs = useQuery({
    queryKey: ["mappingRevisions", kbId, mappingId],
    queryFn: () => api.mappingRevisions(kbId, mappingId),
  });

  if (revs.isPending) return <Loading>{S.nav.loading}</Loading>;
  // **A failed fetch has to say it failed.** `?? []` would paint a 500 as "never edited" --
  // and a definition that was edited being reported as never edited is far harder to track
  // down than an error message
  if (revs.isError)
    return (
      <div className="mt-3 border-t border-white/5 pt-3">
        <ErrorText>{(revs.error as Error).message}</ErrorText>
      </div>
    );
  const list = revs.data?.revisions ?? [];
  return (
    <div className="mt-3 border-t border-white/5 pt-3 space-y-2">
      <p className="text-[11px] text-neutral-500">{S.mapping.historyHint}</p>
      {list.length === 0 ? (
        <p className="text-xs text-neutral-600">{S.mapping.historyEmpty}</p>
      ) : (
        list.map((r) => {
          const b = r.before;
          const was = (b.sql ?? b.expr ?? b.table_name) as string | null;
          return (
            <div key={r.id} className="text-xs">
              <div className="text-neutral-500">
                {S.mapping.historyBy(
                  r.changed_by_name ?? S.mapping.historyUnknown,
                )}
                <span className="u-num ml-2 text-neutral-600">
                  {r.changed_at.slice(0, 16).replace("T", " ")}
                </span>
              </div>
              <div className="u-num text-neutral-400 break-all">
                {was ?? S.mapping.noDefinition}
              </div>
            </div>
          );
        })
      )}
    </div>
  );
}

/** Data source mounting at the knowledge-base level. **Moved here from KB settings** -- which
 *  database is mounted and how a definition is written are two halves of one thing: without
 *  knowing which tables exist you cannot judge whether a definition is right.
 *  Registering a new connection is still a deployment-level action: admins get a direct link,
 *  everyone else gets pointed at an admin. */
function DataSources({
  kbId,
  onExplored,
}: {
  kbId: string;
  onExplored: () => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const me = useQuery({ queryKey: ["me"], queryFn: api.me });
  const mounted = useQuery({
    queryKey: ["kbDataSources", kbId],
    queryFn: () => api.kbDataSources(kbId),
  });
  const available = useQuery({
    queryKey: ["kbDataSourcesAvail", kbId],
    queryFn: () => api.kbDataSourcesAvailable(kbId),
  });
  const [picked, setPicked] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  // Kept apart from notice: one says "it worked", the other says "it half worked", and their
  // colours differ too
  const [warning, setWarning] = useState<string | null>(null);
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["kbDataSources", kbId] });

  const mount = useMutation({
    mutationFn: (dsId: string) => api.mountDataSource(kbId, dsId),
    // **"The mount worked" and "the schema did not" are two different things.** The server
    // returns ok here (the source really did get mounted), so we must not follow
    // `schema_tables: 0` and say "ingested 0 tables" -- that amounts to claiming success.
    // Say which half is the half that worked; the same thing also goes into the alert centre
    onSuccess: (r) => {
      setPicked("");
      setNotice(null);
      setWarning(r.schema_error ? S.mapping.schemaFailed : null);
      if (!r.schema_error) setNotice(S.mapping.schemaSynced(r.schema_tables));
      invalidate();
    },
    onError: (e: unknown) => toast.error((e as Error).message),
  });
  const unmount = useMutation({
    mutationFn: (dsId: string) => api.unmountDataSource(kbId, dsId),
    onSettled: invalidate,
  });
  const sync = useMutation({
    mutationFn: (dsId: string) => api.syncDataSourceSchema(kbId, dsId),
    onSuccess: (r) => setNotice(S.mapping.schemaSynced(r.schema_tables)),
    onError: (e: unknown) => toast.error((e as Error).message),
  });
  const explore = useMutation({
    mutationFn: () => api.exploreMappings(kbId),
    onSuccess: () => {
      setNotice(S.mapping.exploreQueued);
      onExplored();
    },
    onError: (e: unknown) => toast.error((e as Error).message),
  });

  const mountedIds = new Set(
    (mounted.data?.data_sources ?? []).map((d) => d.id),
  );
  const mountable = (available.data?.data_sources ?? []).filter(
    (d) => !mountedIds.has(d.id),
  );
  const hasMounted = (mounted.data?.data_sources.length ?? 0) > 0;

  return (
    <div className="space-y-4">
      <p className="text-xs text-neutral-500">{S.mapping.sourcesHint}</p>

      <div className="glass rounded-xl divide-y divide-white/5">
        {(mounted.data?.data_sources ?? []).map((d) => (
          <div key={d.id} className="px-4 py-3 flex items-center gap-3">
            <div className="min-w-0 flex-1">
              <div className="text-sm text-neutral-200">{d.name}</div>
              <div className="text-xs text-neutral-500 u-num truncate">
                {d.summary}
              </div>
            </div>
            <button
              className="u-btn u-btn-ghost px-2.5 py-1 text-xs shrink-0"
              disabled={sync.isPending}
              onClick={() => sync.mutate(d.id)}
            >
              {S.mapping.syncSchema}
            </button>
            <button
              className="text-xs text-neutral-500 hover:text-[var(--u-danger)] shrink-0"
              disabled={unmount.isPending}
              onClick={() => unmount.mutate(d.id)}
            >
              {S.mapping.unmount}
            </button>
          </div>
        ))}
        {!hasMounted && (
          <p className="px-4 py-6 text-sm text-neutral-500">
            {S.mapping.sourcesEmpty}
          </p>
        )}
      </div>

      {mountable.length > 0 && (
        <div className="flex items-center gap-2">
          <SearchSelect
            className="flex-1"
            value={picked}
            options={mountable.map((d) => ({
              value: d.id,
              label: d.name,
              hint: d.summary,
            }))}
            onChange={setPicked}
            placeholder={S.mapping.mount + "…"}
          />
          <button
            className="u-btn u-btn-primary px-3.5 py-1.5 text-xs shrink-0"
            disabled={!picked || mount.isPending}
            onClick={() => mount.mutate(picked)}
          >
            {S.mapping.mount}
          </button>
        </div>
      )}

      {me.data?.is_admin ? (
        <button
          className="flex items-center gap-1.5 text-xs text-neutral-500 hover:text-neutral-300 transition-colors"
          onClick={() =>
            navigate({ to: "/admin", search: { tab: "datasources" } })
          }
        >
          <Plus size={12} />
          {S.mapping.newConn}
        </button>
      ) : (
        mountable.length === 0 &&
        available.data &&
        !hasMounted && (
          <p className="text-xs text-neutral-600">
            {S.mapping.sourcesNoneAvailable}
          </p>
        )
      )}

      {hasMounted && (
        <div className="glass rounded-xl px-4 py-3 flex items-center gap-3">
          <p className="text-xs text-neutral-500 flex-1">
            {S.mapping.exploreHint}
          </p>
          <button
            className="u-btn u-btn-ghost px-2.5 py-1 text-xs shrink-0"
            disabled={explore.isPending}
            onClick={() => explore.mutate()}
          >
            {S.mapping.explore}
          </button>
        </div>
      )}
      {notice && <p className="text-xs text-[var(--u-ok)]">{notice}</p>}
      {warning && <p className="text-xs text-[var(--u-warn)]">{warning}</p>}
    </div>
  );
}
