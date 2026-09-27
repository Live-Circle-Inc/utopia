// Ontology editor: a master-detail two-column layout (the same shape as Library's SourcesRail).
// Left column = filter + the Classes/Properties sections + the Unmatched entry at the bottom;
// Right side = the selected item's form / the unmatched-signal panel / the overview.
import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  ChevronRight,
  Inbox,
  Plus,
  Search,
  Upload,
  Wand2,
} from "lucide-react";
import {
  api,
  type EntityTypeView,
  type ImportPlan,
  type OntologyMiss,
  type PlannedItem,
  type OntologyProposals,
  type ResolutionOutcome,
  type TypeSuggestion,
  type RelationTypeView,
} from "../api";
import { S } from "../i18n";
import { useKb } from "../kb";
import { toast } from "../toast";
import {
  Button,
  Chip,
  ColorPicker,
  colorForKey,
  DangerConfirm,
  Dropdown,
  Input,
  Loading,
  MultiSearchSelect,
  Pager,
  PageTitle,
  RAIL_CLS,
  SearchSelect,
  cn,
  pageSlice,
} from "../ui";

/** Left-column row height (py-1.5 + 13px text + the space-y gap) and the reserve at the bottom
 *  (the new-item row + the pager) */
const RAIL_ROW_H = 34;
const RAIL_RESERVED = 80;
/** Fallback rows per page (used on the first frame, before the height has been measured) */
const RAIL_PAGE = 14;
/** Rows per section when filter mode interleaves the two sections */
const RAIL_PAGE_MIXED = 6;

/** What the detail area on the right is currently showing */
type Sel =
  | { kind: "class"; id: string }
  | { kind: "relation"; id: string }
  | { kind: "new-class"; parentId: string | null }
  | { kind: "new-relation" }
  | { kind: "misses" }
  // Type resolution: swap a class that is "roughly right" for the more specific one
  | { kind: "refine" }
  | { kind: "import" }
  | null;

export function Ontology() {
  const { kb } = useKb();
  const queryClient = useQueryClient();
  const [sel, setSel] = useState<Sel>(null);
  const [railTab, setRailTab] = useState<"classes" | "properties">("classes");
  const [filter, setFilter] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // Rows per page are computed from the list area's actual height: fill however tall the window
  // is, with neither scrolling nor a big empty gap
  const listRef = useRef<HTMLDivElement>(null);
  const [railRows, setRailRows] = useState(RAIL_PAGE);
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setRailRows(
        Math.max(5, Math.floor((el.clientHeight - RAIL_RESERVED) / RAIL_ROW_H)),
      );
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const data = useQuery({
    queryKey: ["ontology", kb?.id],
    queryFn: () => api.ontology(kb!.id),
    enabled: !!kb,
  });

  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: ["ontology", kb?.id] });
  // Errors all go through the global toast; no more inline error rows on the page
  const onError = (e: unknown) => toast.error((e as Error).message);

  if (!kb) return <Loading>{S.nav.loading}</Loading>;
  if (data.isPending) return <Loading>{S.nav.loading}</Loading>;
  if (data.isError) return <Loading>{(data.error as Error).message}</Loading>;

  const { entity_types, relation_types, misses, dismissed_misses } = data.data;
  // Attributes do not go in the Properties list: they hang off a class, and are edited in the
  // class detail area
  const relations = relation_types.filter((r) => r.kind !== "attribute");
  const selectedClass =
    sel?.kind === "class"
      ? (entity_types.find((t) => t.id === sel.id) ?? null)
      : null;
  const selectedProp =
    sel?.kind === "relation"
      ? (relation_types.find((r) => r.id === sel.id) ?? null)
      : null;

  return (
    <div className="h-full flex">
      {/* Left column: filter + the two sections + Unmatched */}
      <aside className={`${RAIL_CLS} flex flex-col`}>
        <div className="px-3 pt-3 pb-2.5">
          <div className="relative">
            <Search
              size={12}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-neutral-600"
            />
            <input
              className="input-dark w-full pl-7 pr-2 py-1.5 text-xs"
              placeholder={S.ontology.filter}
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
        </div>
        {/* Segmented switch: the same vocabulary as the login page's mode switch and the
            schedule picker (a bg-white/5 container + the active one inverted); the list is the
            exception while filtering: the two sections interleave so hits from both show up */}
        <div className="mx-3 mb-1 flex gap-1 rounded-lg bg-white/5 p-1">
          {(
            [
              ["classes", S.ontology.tabClasses],
              ["properties", S.ontology.tabProperties],
            ] as const
          ).map(([k, label]) => (
            <button
              key={k}
              onClick={() => setRailTab(k)}
              className={cn(
                "flex-1 rounded-md py-1 text-[12px] font-medium text-center transition-colors",
                railTab === k
                  ? "bg-white/10 text-neutral-100"
                  : "text-neutral-500 hover:text-neutral-300",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <div
          ref={listRef}
          className="flex-1 min-h-0 overflow-hidden px-2 pt-1.5 pb-2 flex flex-col"
        >
          {/* The new-item row goes on top: it creates a class or a relation, following the
              current segment */}
          {!filter.trim() && (
            <button
              onClick={() =>
                railTab === "classes"
                  ? setSel({ kind: "new-class", parentId: null })
                  : setSel({ kind: "new-relation" })
              }
              className="w-full flex items-center gap-1.5 rounded-lg px-2 py-2 mb-0.5 text-[13px] text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-200 transition-colors"
            >
              <Plus size={13} />
              {railTab === "classes"
                ? S.ontology.newClass
                : S.ontology.newProperty}
            </button>
          )}
          {filter.trim() ? (
            <>
              <div className="px-2 pt-2 pb-1 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-600">
                {S.ontology.tabClasses}
              </div>
              <ClassTree
                types={entity_types}
                filter={filter}
                collapsed={collapsed}
                onToggle={() => {}}
                selectedId={selectedClass?.id ?? null}
                onSelect={(id) => setSel({ kind: "class", id })}
                pageSize={RAIL_PAGE_MIXED}
              />
              <div className="px-2 pt-3 pb-1 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-600">
                {S.ontology.tabProperties}
              </div>
              <PropertyList
                relations={relations}
                filter={filter}
                selectedId={selectedProp?.id ?? null}
                onSelect={(id) => setSel({ kind: "relation", id })}
                pageSize={RAIL_PAGE_MIXED}
              />
            </>
          ) : railTab === "classes" ? (
            <ClassTree
              types={entity_types}
              filter={filter}
              collapsed={collapsed}
              onToggle={(id) => {
                const next = new Set(collapsed);
                if (next.has(id)) next.delete(id);
                else next.add(id);
                setCollapsed(next);
              }}
              selectedId={selectedClass?.id ?? null}
              onSelect={(id) => setSel({ kind: "class", id })}
              pageSize={railRows}
            />
          ) : (
            <PropertyList
              relations={relations}
              filter={filter}
              selectedId={selectedProp?.id ?? null}
              onSelect={(id) => setSel({ kind: "relation", id })}
              pageSize={railRows}
            />
          )}
        </div>
        {/* Permanent at the bottom: the two "about the ontology" entries -- take an ontology
            from outside, or look at the signals extraction pushed back */}
        <button
          onClick={() => setSel({ kind: "import" })}
          className={cn(
            "shrink-0 border-t border-white/10 px-4 py-2.5 flex items-center gap-2 text-[13px] transition-colors",
            sel?.kind === "import"
              ? "u-nav-active"
              : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200",
          )}
        >
          <Upload size={14} className="text-neutral-500" />
          <span>{S.ontology.importShort}</span>
        </button>
        {/* Type resolution: swap a class that is "roughly right" for the more specific one.
            **Right next to Unmatched** -- both of them deal with "the ontology and the data do
            not line up", only from opposite directions: that one is the ontology missing
            something, this one is the ontology having a better option that went unused */}
        <button
          onClick={() => setSel({ kind: "refine" })}
          className={cn(
            "shrink-0 border-t border-white/10 px-4 py-2.5 flex items-center gap-2 text-[13px] transition-colors",
            sel?.kind === "refine"
              ? "u-nav-active"
              : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200",
          )}
        >
          <Wand2 size={14} className="text-neutral-500" />
          <span>{S.ontology.refineShort}</span>
        </button>
        {/* Permanent at the bottom: the unmatched signals from extraction (with a count badge
            when there are any on hand) */}
        <button
          onClick={() => setSel({ kind: "misses" })}
          className={cn(
            "shrink-0 border-t border-white/10 px-4 py-2.5 flex items-center gap-2 text-[13px] transition-colors",
            sel?.kind === "misses"
              ? "u-nav-active"
              : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200",
          )}
        >
          <Inbox size={14} className="text-neutral-500" />
          <span>{S.ontology.missesShort}</span>
          {misses.length > 0 && (
            <span className="ml-auto u-num text-[10.5px] text-neutral-500 bg-white/[0.08] rounded-full px-1.5 py-px">
              {misses.length}
            </span>
          )}
        </button>
      </aside>

      {/* Right side: the detail. With a class selected, the form + the instance list spread out
          over two columns, to make better use of a wide screen */}
      <div className="flex-1 min-w-0 overflow-y-auto u-scroll px-8 py-6">
        {/* Widened to 6xl so three columns can spread out; misses/relations/overview each carry
            their own max-w-xl lining and are unaffected */}
        <div className="max-w-6xl">
          {sel?.kind === "import" ? (
            <div className="max-w-xl">
              <ImportPanel kbId={kb.id} onChanged={refresh} onError={onError} />
            </div>
          ) : sel?.kind === "refine" ? (
            <div className="max-w-2xl">
              <RefinePanel kbId={kb.id} onChanged={refresh} onError={onError} />
            </div>
          ) : sel?.kind === "misses" ? (
            <div className="max-w-xl">
              <MissesPanel
                kbId={kb.id}
                misses={misses}
                dismissedMisses={dismissed_misses ?? []}
                onChanged={refresh}
                onError={onError}
              />
            </div>
          ) : sel?.kind === "new-class" || selectedClass ? (
            /* lg: two columns (form | attributes+instances stacked); xl: three side by side (the
               wrapper's xl:contents dissolves it into the grid) */
            <div className="grid gap-4 items-start lg:grid-cols-[minmax(0,24rem)_minmax(0,1fr)] xl:grid-cols-[minmax(0,22rem)_minmax(0,1fr)_minmax(0,1fr)]">
              <div className="glass rounded-xl p-4">
                <ClassForm
                  key={
                    selectedClass?.id ??
                    `new-${sel?.kind === "new-class" ? sel.parentId : "root"}`
                  }
                  kbId={kb.id}
                  existing={selectedClass}
                  parentId={
                    sel?.kind === "new-class"
                      ? sel.parentId
                      : (selectedClass?.primary_parent ?? null)
                  }
                  allTypes={entity_types}
                  onNewSub={
                    selectedClass
                      ? () =>
                          setSel({
                            kind: "new-class",
                            parentId: selectedClass.id,
                          })
                      : undefined
                  }
                  onDone={(createdId) => {
                    // Select it the moment it is created: you can see it and go on editing it
                    // straight away
                    if (sel?.kind === "new-class")
                      setSel(
                        createdId ? { kind: "class", id: createdId } : null,
                      );
                    refresh();
                  }}
                  onError={onError}
                />
              </div>
              {/* lg: the right column stacks attributes+instances; xl: it dissolves into two
                  independent grid columns */}
              {selectedClass && (
                <div className="grid gap-4 items-start xl:contents">
                  <AttributesCard
                    kbId={kb.id}
                    type={selectedClass}
                    attributes={relation_types.filter(
                      (r) =>
                        r.kind === "attribute" &&
                        r.domains.includes(selectedClass.id),
                    )}
                    onChanged={refresh}
                    onError={onError}
                  />
                  <InstancesCard kbId={kb.id} type={selectedClass} />
                </div>
              )}
            </div>
          ) : sel?.kind === "new-relation" || selectedProp ? (
            <div className="glass rounded-xl p-4 max-w-xl">
              <PropertyForm
                key={selectedProp?.id ?? "new"}
                kbId={kb.id}
                existing={selectedProp}
                allTypes={entity_types}
                allRelations={relations}
                onDone={(createdId) => {
                  if (sel?.kind === "new-relation")
                    setSel(
                      createdId ? { kind: "relation", id: createdId } : null,
                    );
                  refresh();
                }}
                onError={onError}
              />
            </div>
          ) : (
            /* Overview: nothing selected */
            <div className="glass rounded-xl p-6 max-w-xl">
              <PageTitle className="mb-1">{S.ontology.title}</PageTitle>
              <p className="text-xs text-neutral-500 u-num">
                {S.ontology.overviewStats(
                  entity_types.length,
                  relations.length,
                )}
              </p>
              <p className="mt-3 text-sm text-neutral-400">
                {S.ontology.overviewHint}
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/* ---------- Instances: the selected class's entities (server-paged, click into graph) ---------- */

function InstancesCard({ kbId, type }: { kbId: string; type: EntityTypeView }) {
  const PER = 12;
  const [page, setPage] = useState(0);
  useEffect(() => setPage(0), [type.id]);
  const q = useQuery({
    queryKey: ["type-entities", kbId, type.id, page],
    queryFn: () => api.typeEntities(kbId, type.id, page, PER),
  });
  const total = q.data?.total ?? 0;
  const rows = q.data?.entities ?? [];
  if (!q.isPending && total === 0) return null; // Take up no space when there are none

  return (
    <div className="glass rounded-xl p-4">
      <div className="mb-1.5 flex items-baseline gap-2">
        <h3 className="text-sm font-bold text-neutral-200">
          {S.ontology.instances}
        </h3>
        <span className="u-num text-xs text-neutral-500">{total}</span>
      </div>
      <div className="divide-y divide-white/[0.06]">
        {rows.map((e) => (
          <Link
            key={e.id}
            to="/kb/$kbId/graph"
            params={{ kbId }}
            search={{ entity: e.id }}
            className="flex items-center gap-2 py-1.5 text-sm text-neutral-300 hover:text-white"
          >
            <span
              className={`h-2 w-2 shrink-0 ${type.shape === "square" ? "" : "rounded-full"}`}
              style={{ background: type.color }}
            />
            <span className="truncate">{e.name}</span>
            <span className="ml-auto shrink-0 u-num text-[10.5px] text-neutral-600">
              {S.ontology.instanceFacts(e.fact_count)}
            </span>
          </Link>
        ))}
      </div>
      <Pager total={total} pageSize={PER} page={page} onPage={setPage} />
    </div>
  );
}

/* ---------- Attributes card: the class's literal fields (inline add/edit/delete) ---------- */

function AttributesCard({
  kbId,
  type,
  attributes,
  onChanged,
  onError,
}: {
  kbId: string;
  type: EntityTypeView;
  attributes: RelationTypeView[];
  onChanged: () => void;
  onError: (e: unknown) => void;
}) {
  // Inline editing: only one row is open at a time (an attribute id, or "new")
  const [editing, setEditing] = useState<string | null>(null);
  useEffect(() => setEditing(null), [type.id]);

  return (
    <div className="glass rounded-xl p-4">
      <div className="mb-1 flex items-baseline gap-2">
        <h3 className="text-sm font-bold text-neutral-200">
          {S.ontology.attributes}
        </h3>
        {attributes.length > 0 && (
          <span className="u-num text-xs text-neutral-500">
            {attributes.length}
          </span>
        )}
      </div>
      <p className="text-xs text-neutral-500 mb-2">
        {S.ontology.attributesHint}
      </p>
      <div className="divide-y divide-white/[0.06]">
        {attributes.map((a) =>
          editing === a.id ? (
            <AttributeForm
              key={a.id}
              kbId={kbId}
              typeId={type.id}
              existing={a}
              onDone={() => {
                setEditing(null);
                onChanged();
              }}
              onCancel={() => setEditing(null)}
              onError={onError}
            />
          ) : (
            <button
              key={a.id}
              onClick={() => setEditing(a.id)}
              className="w-full flex items-center gap-2 py-1.5 text-sm text-left text-neutral-300 hover:text-white"
            >
              <span className="truncate">{a.label}</span>
              <Chip tone="neutral">
                {S.ontology.datatypeNames[a.datatype ?? "text"]}
              </Chip>
              {a.unit && (
                <span className="text-xs text-neutral-500 shrink-0">
                  {a.unit}
                </span>
              )}
              {a.functional && <Chip tone="info">1:1</Chip>}
              <span className="ml-auto shrink-0 u-num text-[10.5px] text-neutral-600">
                {S.ontology.usage(a.usage)}
              </span>
            </button>
          ),
        )}
      </div>
      {editing === "new" ? (
        <div className="pt-2">
          <AttributeForm
            kbId={kbId}
            typeId={type.id}
            existing={null}
            onDone={() => {
              setEditing(null);
              onChanged();
            }}
            onCancel={() => setEditing(null)}
            onError={onError}
          />
        </div>
      ) : (
        <button
          onClick={() => setEditing("new")}
          className="mt-1.5 flex items-center gap-1.5 text-[13px] text-neutral-500 hover:text-neutral-200 transition-colors"
        >
          <Plus size={13} />
          {S.ontology.newAttribute}
        </button>
      )}
    </div>
  );
}

function AttributeForm({
  kbId,
  typeId,
  existing,
  onDone,
  onCancel,
  onError,
}: {
  kbId: string;
  typeId: string;
  existing: RelationTypeView | null;
  onDone: () => void;
  onCancel: () => void;
  onError: (e: unknown) => void;
}) {
  const [key, setKey] = useState(existing?.key ?? "");
  const [label, setLabel] = useState(existing?.label ?? "");
  const [datatype, setDatatype] = useState(existing?.datatype ?? "text");
  const [unit, setUnit] = useState(existing?.unit ?? "");
  // Single-valued = functional: a new value closes the old one off through the temporal engine
  // (which is where attribute history comes from). Most attributes are, so it is on by default
  const [single, setSingle] = useState(existing?.functional ?? true);
  const [description, setDescription] = useState(existing?.description ?? "");

  const save = useMutation({
    mutationFn: async (): Promise<unknown> =>
      existing
        ? api.updateRelationType(kbId, existing.id, {
            label,
            temporal: existing.temporal,
            functional: single,
            inverse_functional: false,
            description,
            datatype,
            unit,
          })
        : api.createRelationType(kbId, {
            key,
            label,
            kind: "attribute",
            domains: [typeId],
            temporal: "state",
            functional: single,
            inverse_functional: false,
            description,
            datatype,
            unit,
          }),
    onSuccess: () => {
      toast.success(existing ? S.toast.saved : S.toast.created);
      onDone();
    },
    onError,
  });
  const remove = useMutation({
    mutationFn: () => api.deleteRelationType(kbId, existing!.id),
    onSuccess: () => {
      toast.success(S.toast.deleted);
      onDone();
    },
    onError,
  });

  const lbl = "block text-xs font-medium text-neutral-500 mb-1";
  return (
    <div className="py-2.5 space-y-2.5">
      {!existing && (
        <div className="flex gap-2">
          <div className="flex-1">
            <label className={lbl}>{S.ontology.key}</label>
            <Input
              value={key}
              onChange={(e) => setKey(e.target.value)}
              className="w-full"
              placeholder="salary"
            />
          </div>
          <div className="flex-1">
            <label className={lbl}>{S.ontology.label}</label>
            <Input
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              className="w-full"
            />
          </div>
        </div>
      )}
      {existing && (
        <div>
          <label className={lbl}>{S.ontology.label}</label>
          <Input
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            className="w-full"
          />
        </div>
      )}
      <div className="flex gap-2">
        <div className="flex-1">
          <label className={lbl}>{S.ontology.attrDatatype}</label>
          <Dropdown
            value={datatype}
            onChange={(v) => setDatatype(v as typeof datatype)}
            className="w-full"
            options={(["text", "number", "date", "bool"] as const).map((d) => ({
              value: d,
              label: S.ontology.datatypeNames[d],
            }))}
          />
        </div>
        <div className="flex-1">
          <label className={lbl}>
            {S.ontology.attrUnit}{" "}
            <span className="text-neutral-600">
              ({S.ontology.attrUnitHint})
            </span>
          </label>
          <Input
            value={unit}
            onChange={(e) => setUnit(e.target.value)}
            className="w-full"
          />
        </div>
      </div>
      <label className="flex items-center gap-2 text-[13px] text-neutral-300">
        <input
          type="checkbox"
          checked={single}
          onChange={(e) => setSingle(e.target.checked)}
        />
        {S.ontology.attrSingle}
      </label>
      <div>
        <label className={lbl}>{S.ontology.description}</label>
        <textarea
          className="input-dark w-full px-3 py-2 text-sm min-h-[3.5rem] resize-y"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
      </div>
      <div className="flex gap-2">
        <Button
          size="sm"
          onClick={() => save.mutate()}
          disabled={
            save.isPending || !label.trim() || (!existing && !key.trim())
          }
        >
          {S.ontology.save}
        </Button>
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {S.ontology.cancel}
        </Button>
        {existing && (
          <Button
            size="sm"
            variant="ghost"
            className="ml-auto"
            disabled={existing.usage > 0}
            title={existing.usage > 0 ? S.ontology.deleteBlocked : undefined}
            onClick={() => remove.mutate()}
          >
            {S.ontology.delete}
          </Button>
        )}
      </div>
    </div>
  );
}

/* ---------- Left-column section header ---------- */

/* ---------- Class hierarchy tree (collapsible; flattened while filtering) ---------- */

function ClassTree({
  types,
  filter,
  collapsed,
  onToggle,
  selectedId,
  onSelect,
  pageSize,
}: {
  types: EntityTypeView[];
  filter: string;
  collapsed: Set<string>;
  onToggle: (id: string) => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
  pageSize: number;
}) {
  const rows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (q) {
      // Filter mode: flatten the hits (both label and key take part in the matching)
      return types
        .filter(
          (t) =>
            t.label.toLowerCase().includes(q) ||
            t.key.toLowerCase().includes(q),
        )
        .map((t) => ({ t, depth: 0, hasChildren: false }));
    }
    const children = new Map<string | null, EntityTypeView[]>();
    for (const t of types) {
      const p = t.primary_parent ?? null;
      if (!children.has(p)) children.set(p, []);
      children.get(p)!.push(t);
    }
    const out: { t: EntityTypeView; depth: number; hasChildren: boolean }[] =
      [];
    const walk = (parent: string | null, depth: number) => {
      for (const t of children.get(parent) ?? []) {
        const kids = children.get(t.id) ?? [];
        out.push({ t, depth, hasChildren: kids.length > 0 });
        if (!collapsed.has(t.id)) walk(t.id, depth + 1);
      }
    };
    walk(null, 0);
    return out;
  }, [types, filter, collapsed]);

  // Half-screen paging: a change in the filter goes back to the first page
  const [page, setPage] = useState(0);
  useEffect(() => setPage(0), [filter]);
  const { rows: paged, safe } = pageSlice(rows, page, pageSize);

  return (
    <div className="space-y-0.5">
      {paged.map(({ t, depth, hasChildren }) => (
        <button
          key={t.id}
          onClick={() => onSelect(t.id)}
          style={{ paddingLeft: `${6 + depth * 14}px` }}
          className={cn(
            "w-full text-left rounded-lg py-1.5 pr-2 text-[13px] flex items-center gap-1.5",
            selectedId === t.id
              ? "u-nav-active"
              : "hover:bg-white/[0.05] text-neutral-400 hover:text-neutral-200",
          )}
        >
          {/* Collapse handle: rendered only when there are subclasses, and clicking it does
              not select */}
          {hasChildren ? (
            <span
              onClick={(e) => {
                e.stopPropagation();
                onToggle(t.id);
              }}
              className="shrink-0 text-neutral-600 hover:text-neutral-300"
            >
              <ChevronRight
                size={12}
                className={cn(
                  "transition-transform",
                  !collapsed.has(t.id) && "rotate-90",
                )}
              />
            </span>
          ) : (
            <span className="w-3 shrink-0" />
          )}
          {/* A square has sharp corners: that is what tells it apart from a circle (same for
              the graph nodes) */}
          <span
            className={`h-2.5 w-2.5 shrink-0 ${t.shape === "square" ? "" : "rounded-full"}`}
            style={{ background: t.color }}
          />
          {/* No per-item usage readout in the list: once the counts get large, both the
              tallying and the rendering are a burden -- usage is on the form */}
          <span className="truncate">{t.label}</span>
        </button>
      ))}
      <Pager
        total={rows.length}
        pageSize={pageSize}
        page={safe}
        onPage={setPage}
      />
    </div>
  );
}

/* ---------- Relation list ---------- */

function PropertyList({
  relations,
  filter,
  selectedId,
  onSelect,
  pageSize,
}: {
  relations: RelationTypeView[];
  filter: string;
  selectedId: string | null;
  onSelect: (id: string) => void;
  pageSize: number;
}) {
  const q = filter.trim().toLowerCase();
  const rows = q
    ? relations.filter(
        (r) =>
          r.label.toLowerCase().includes(q) || r.key.toLowerCase().includes(q),
      )
    : relations;
  // Half-screen paging: a change in the filter goes back to the first page
  const [page, setPage] = useState(0);
  useEffect(() => setPage(0), [filter]);
  const { rows: paged, safe } = pageSlice(rows, page, pageSize);
  return (
    <div className="space-y-0.5">
      {paged.map((r) => (
        <button
          key={r.id}
          onClick={() => onSelect(r.id)}
          style={{ paddingLeft: "6px" }}
          className={cn(
            "w-full text-left rounded-lg py-1.5 pr-2 text-[13px] flex items-center gap-1.5",
            selectedId === r.id
              ? "u-nav-active"
              : "hover:bg-white/[0.05] text-neutral-400 hover:text-neutral-200",
          )}
        >
          {/* The lead-in keeps only the collapse-handle slot: the text starts flush with the
              left edge of the "marker dot" on a class row */}
          <span className="w-3 shrink-0" />
          <span className="truncate">{r.label}</span>
          {r.functional && <Chip tone="info">1:1</Chip>}
        </button>
      ))}
      <Pager
        total={rows.length}
        pageSize={pageSize}
        page={safe}
        onPage={setPage}
      />
    </div>
  );
}

/* ---------- Class form ---------- */

/** Candidates for the parent dropdown: tree order + indentation (so the hierarchy is visible),
 *  excluding itself and every one of its descendants (to prevent cycles). */
function parentOptions(
  allTypes: EntityTypeView[],
  selfId: string | undefined,
): { value: string; label: string; indent: number }[] {
  const excluded = new Set<string>();
  if (selfId) {
    excluded.add(selfId);
    // Collect the descendants: scan over and over until it converges (the number of classes is
    // tiny, so O(n²) does not matter)
    let grew = true;
    while (grew) {
      grew = false;
      for (const t of allTypes) {
        if (t.parents.some((p) => excluded.has(p)) && !excluded.has(t.id)) {
          excluded.add(t.id);
          grew = true;
        }
      }
    }
  }
  const children = new Map<string | null, EntityTypeView[]>();
  for (const t of allTypes) {
    const p = t.primary_parent ?? null;
    if (!children.has(p)) children.set(p, []);
    children.get(p)!.push(t);
  }
  const out: { value: string; label: string; indent: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const t of children.get(parent) ?? []) {
      if (excluded.has(t.id)) continue;
      out.push({ value: t.id, label: t.label, indent: depth });
      walk(t.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

function ClassForm({
  kbId,
  existing,
  parentId,
  allTypes,
  onNewSub,
  onDone,
  onError,
}: {
  kbId: string;
  existing: EntityTypeView | null;
  parentId: string | null;
  allTypes: EntityTypeView[];
  /** Provided when editing an existing class: create a subclass under the current class */
  onNewSub?: () => void;
  /** Carries the new id on a successful create; undefined on a successful edit */
  onDone: (createdId?: string) => void;
  onError: (e: unknown) => void;
}) {
  const [key, setKey] = useState(existing?.key ?? "");
  const [label, setLabel] = useState(existing?.label ?? "");
  // On create the color follows the key (the same rule as the backend's color_for_key), rather
  // than being one fixed default. The user can of course change it; but **leave it alone and the
  // classes built by hand share one color scheme with the classes built by import**
  const [color, setColor] = useState(
    existing?.color ?? colorForKey(existing?.key ?? ""),
  );
  const [colorTouched, setColorTouched] = useState(Boolean(existing?.color));
  const [shape, setShape] = useState<"circle" | "square">(
    existing?.shape ?? "circle",
  );
  const [parents, setParents] = useState<string[]>(
    existing?.parents ?? (parentId ? [parentId] : []),
  );
  const [description, setDescription] = useState(existing?.description ?? "");
  // Disjointness: a declaration of "cannot be both at once". The consistency check reports
  // unsatisfiable classes from it (0002) -- a class that inherits two disjoint ancestors can
  // never have an instance, and it raises no error, it just stays empty forever
  const [disjoint, setDisjoint] = useState<string[]>(existing?.disjoint ?? []);

  // Coming in from "+ subclass" in the left column prefills that parent. With multiple parents
  // it is the first one, which is to say the primary parent
  useEffect(() => setParents(parentId ? [parentId] : []), [parentId]);

  const save = useMutation({
    mutationFn: async (): Promise<unknown> =>
      existing
        ? api.updateEntityType(kbId, existing.id, {
            label,
            color,
            shape,
            parents,
            disjoint,
            description,
          })
        : api.createEntityType(kbId, {
            key,
            label,
            color,
            shape,
            parents,
            disjoint,
            description,
          }),
    onSuccess: (res) => {
      toast.success(existing ? S.toast.saved : S.toast.created);
      onDone(existing ? undefined : (res as { id?: string })?.id);
    },
    onError,
  });
  const remove = useMutation({
    mutationFn: () => api.deleteEntityType(kbId, existing!.id),
    onSuccess: () => {
      toast.success(S.toast.deleted);
      onDone();
    },
    onError,
  });

  const lbl = "block text-xs font-medium text-neutral-500 mb-1";
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <span
          className={`h-3 w-3 ${shape === "square" ? "" : "rounded-full"}`}
          style={{ background: color }}
        />
        <span className="font-bold text-neutral-100">
          {existing?.label ?? S.ontology.newClass}
        </span>
        {/* The key is a purely technical identifier: once it exists, simply do not show it --
            it is only typed in at creation */}
        {existing?.builtin && <Chip tone="neutral">{S.ontology.builtin}</Chip>}
        {existing && (
          <span className="ml-auto text-xs text-neutral-500">
            {S.ontology.usage(existing.usage)}
          </span>
        )}
      </div>
      {!existing && (
        <div>
          <label className={lbl}>
            {S.ontology.key}{" "}
            <span className="text-neutral-600">({S.ontology.keyHint})</span>
          </label>
          <Input
            value={key}
            onChange={(e) => {
              setKey(e.target.value);
              // The user has not picked a color himself, so let the color follow the key --
              // the same rule as the backend's
              if (!colorTouched) setColor(colorForKey(e.target.value));
            }}
            className="w-full"
            placeholder="contract"
          />
        </div>
      )}
      <div>
        <label className={lbl}>{S.ontology.label}</label>
        <Input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          className="w-full"
        />
      </div>
      <div>
        <label className={lbl}>{S.ontology.shapeColor}</label>
        <div className="flex items-center gap-2">
          <ColorPicker value={color} onChange={(c: string) => { setColor(c); setColorTouched(true); }} shape={shape} />
          {/* Shape: one-to-one with how the graph renders nodes (circle = four-layer circle /
              square = four-layer square) */}
          <div className="flex rounded-lg overflow-hidden border border-white/10">
            {(["circle", "square"] as const).map((sh) => (
              <button
                key={sh}
                onClick={() => setShape(sh)}
                title={sh}
                className={`h-8 w-10 grid place-items-center transition-colors ${
                  shape === sh
                    ? "bg-white/[0.12] text-white"
                    : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300"
                }`}
              >
                <span
                  className={`h-3 w-3 border-[1.5px] border-current ${
                    sh === "circle" ? "rounded-full" : ""
                  }`}
                />
              </button>
            ))}
          </div>
        </div>
      </div>
      {/* Multiple parents: there can be more than one subClassOf. The left column draws a tree,
          in which a class can only appear once -- so the first one counts as the primary parent.
          The UI says so, rather than adding another "pick the primary parent" control */}
      <div>
        <label className={lbl}>{S.ontology.parent}</label>
        <MultiSearchSelect
          values={parents}
          options={parentOptions(allTypes, existing?.id)}
          onToggle={(id) =>
            setParents((v) =>
              v.includes(id) ? v.filter((x) => x !== id) : [...v, id],
            )
          }
          placeholder={S.ontology.searchTypes}
          emptyHint={S.ontology.noParent}
        />
        {parents.length > 1 && (
          <p className="mt-1 text-[11px] text-neutral-600">
            {S.ontology.primaryParentHint}
          </p>
        )}
      </div>
      {/* Disjointness: **a declaration of "cannot be both at once"**. Right next to the parents,
          because the two are two sides of the same thing -- a parent says "is also this",
          disjointness says "cannot be both at once", and what the consistency check reports when
          those two fight is "this class can never have an instance" */}
      <div>
        <label className={lbl}>{S.ontology.disjoint}</label>
        <p className="text-[11px] leading-relaxed text-neutral-600 mb-1.5">
          {S.ontology.disjointHint}
        </p>
        <MultiSearchSelect
          values={disjoint}
          options={parentOptions(allTypes, existing?.id)}
          onToggle={(id) =>
            setDisjoint((v) =>
              v.includes(id) ? v.filter((x) => x !== id) : [...v, id],
            )
          }
          placeholder={S.ontology.searchTypes}
          emptyHint={S.ontology.noDisjoint}
        />
        {/* Disjoint with its own parent = this class can never have an instance. Say so on the
            spot; that beats making someone run the consistency check to find out */}
        {disjoint.some((d) => parents.includes(d)) && (
          <p className="mt-1.5 text-[11px] text-[var(--u-danger)]">
            {S.ontology.disjointWithParent}
          </p>
        )}
      </div>
      <div>
        <label className={lbl}>{S.ontology.description}</label>
        {/* Semantic guidance: the whole paragraph is injected into the extraction prompt, and
            directly affects how well extraction classifies */}
        <textarea
          className="input-dark w-full px-3 py-2 text-sm min-h-[4.5rem] resize-y"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
        <p className="mt-1 text-[10.5px] text-neutral-600">
          {S.ontology.descriptionHint}
        </p>
      </div>
      <div className="flex gap-2 pt-1">
        <Button
          size="sm"
          onClick={() => save.mutate()}
          disabled={save.isPending || !label.trim()}
        >
          {S.ontology.save}
        </Button>
        {onNewSub && (
          <Button size="sm" variant="ghost" onClick={onNewSub}>
            {S.ontology.newSubClass}
          </Button>
        )}
        {existing && !existing.builtin && (
          <Button
            size="sm"
            variant="ghost"
            disabled={existing.usage > 0}
            title={existing.usage > 0 ? S.ontology.deleteBlocked : undefined}
            onClick={() => remove.mutate()}
          >
            {S.ontology.delete}
          </Button>
        )}
      </div>
    </div>
  );
}

/* ---------- Relation form ---------- */

function PropertyForm({
  kbId,
  existing,
  allTypes,
  allRelations,
  onDone,
  onError,
}: {
  kbId: string;
  existing: RelationTypeView | null;
  allTypes: EntityTypeView[];
  /** This knowledge base's relations (attributes excluded). The inverse and sub-property
   *  dropdowns are taken from here -- **attributes are not among them**: their object is a
   *  literal, so there is no such thing as the other way round */
  allRelations: RelationTypeView[];
  onDone: (createdId?: string) => void;
  onError: (e: unknown) => void;
}) {
  const [key, setKey] = useState(existing?.key ?? "");
  const [label, setLabel] = useState(existing?.label ?? "");
  const [temporal, setTemporal] = useState(existing?.temporal ?? "state");
  const [functional, setFunctional] = useState(existing?.functional ?? false);
  const [inverseFunctional, setInverseFunctional] = useState(
    existing?.inverse_functional ?? false,
  );
  // The other four OWL axioms. **Everything the reasoner judges by is here** -- they used to be
  // reachable only by importing OWL, so anyone building an ontology by hand in the UI could
  // never start that machine (0002)
  const [transitive, setTransitive] = useState(existing?.is_transitive ?? false);
  const [symmetric, setSymmetric] = useState(existing?.is_symmetric ?? false);
  const [asymmetric, setAsymmetric] = useState(existing?.is_asymmetric ?? false);
  const [irreflexive, setIrreflexive] = useState(
    existing?.is_irreflexive ?? false,
  );
  // The last two of the same family, different in shape: they point at another relation. Empty
  // string = not declared
  const [inverseOf, setInverseOf] = useState(existing?.inverse_of ?? "");
  const [subPropertyOf, setSubPropertyOf] = useState(
    existing?.sub_property_of ?? "",
  );
  const [description, setDescription] = useState(existing?.description ?? "");
  const [domains, setDomains] = useState<string[]>(existing?.domains ?? []);
  const [ranges, setRanges] = useState<string[]>(existing?.ranges ?? []);
  // Show the label, not the key. **The key that goes into the prompt is taken from the database
  // by the server**, which has nothing to do with what the UI displays; and the class tree and
  // the attribute list show labels too, so there is no reason for this to be the exception --
  // in a Chinese knowledge base the user should see "发票记录" and not invoice_record
  const typeOpts = useMemo(
    () => parentOptions(allTypes, undefined),
    [allTypes],
  );
  // The options for the two dropdowns: the other relations in this knowledge base.
  //
  // **Itself never goes in the list**, for either of them. A sub-property pointing at itself is
  // refused outright by the database (that is a cycle); an inverse pointing at itself is
  // semantically legal -- but it is the same as `symmetric`, whose checkbox is right above, and
  // offering a second route to it here only gets people writing what R0 will report.
  //
  // The one exception is **when the current value is itself**: an OWL import can come in looking
  // like that, and leaving it out of the list makes the dropdown show a blank -- and one save of
  // a blank wipes out what was declared
  const linkOptions = (current: string) => [
    { value: "", label: S.ontology.noLink },
    // When the current value is itself, put itself back in the list; otherwise the dropdown
    // shows a blank, and one save of a blank wipes out what was declared
    ...(existing && current === existing.id
      ? [{ value: existing.id, label: existing.label, hint: existing.key }]
      : []),
    ...allRelations
      .filter((r) => r.id !== existing?.id)
      .map((r) => ({ value: r.id, label: r.label, hint: r.key })),
  ];
  /** The display name of the entry selected in the dropdown. Not found falls back to the id --
   *  rather ugly than blank */
  const nameOf = (id: string) =>
    allRelations.find((r) => r.id === id)?.label ?? id;
  const toggle = (
    set: React.Dispatch<React.SetStateAction<string[]>>,
    id: string,
  ) => set((v) => (v.includes(id) ? v.filter((x) => x !== id) : [...v, id]));

  const save = useMutation({
    mutationFn: async (): Promise<unknown> =>
      existing
        ? api.updateRelationType(kbId, existing.id, {
            label,
            temporal,
            functional,
            inverse_functional: inverseFunctional,
            is_transitive: transitive,
            is_symmetric: symmetric,
            is_asymmetric: asymmetric,
            is_irreflexive: irreflexive,
            // An empty string has to become null before it is sent -- the server takes an
            // `Option<Uuid>`, and `""` does not parse as a UUID, so it would be a 422 rather
            // than a "clear it"
            inverse_of: inverseOf || null,
            sub_property_of: subPropertyOf || null,
            description,
            domains,
            ranges,
          })
        : api.createRelationType(kbId, {
            key,
            label,
            temporal,
            functional,
            inverse_functional: inverseFunctional,
            is_transitive: transitive,
            is_symmetric: symmetric,
            is_asymmetric: asymmetric,
            is_irreflexive: irreflexive,
            // An empty string has to become null before it is sent -- the server takes an
            // `Option<Uuid>`, and `""` does not parse as a UUID, so it would be a 422 rather
            // than a "clear it"
            inverse_of: inverseOf || null,
            sub_property_of: subPropertyOf || null,
            description,
            domains,
            ranges,
          }),
    onSuccess: (res) => {
      toast.success(existing ? S.toast.saved : S.toast.created);
      onDone(existing ? undefined : (res as { id?: string })?.id);
    },
    onError,
  });
  const remove = useMutation({
    mutationFn: () => api.deleteRelationType(kbId, existing!.id),
    onSuccess: () => {
      toast.success(S.toast.deleted);
      onDone();
    },
    onError,
  });

  const lbl = "block text-xs font-medium text-neutral-500 mb-1";
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <span className="font-bold text-neutral-100">
          {existing?.label ?? S.ontology.newProperty}
        </span>
        {existing?.builtin && <Chip tone="neutral">{S.ontology.builtin}</Chip>}
        {existing && (
          <span className="ml-auto text-xs text-neutral-500">
            {S.ontology.usage(existing.usage)}
          </span>
        )}
      </div>
      {!existing && (
        <div>
          <label className={lbl}>
            {S.ontology.key}{" "}
            <span className="text-neutral-600">({S.ontology.keyHint})</span>
          </label>
          <Input
            value={key}
            onChange={(e) => setKey(e.target.value)}
            className="w-full"
            placeholder="signed_with"
          />
        </div>
      )}
      <div>
        <label className={lbl}>{S.ontology.label}</label>
        <Input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          className="w-full"
        />
      </div>
      {/* The type signature. The UI shows labels, while what goes into the prompt is the key --
          that step happens on the server and has nothing to do with what is displayed here
          (docs/decisions/0004 is what settles that the prompt must use keys) */}
      <div>
        <label className={lbl}>{S.ontology.signature}</label>
        <p className="text-[11px] leading-relaxed text-neutral-600 mb-1.5">
          {S.ontology.signatureHint}
        </p>
        <div className="grid gap-2 sm:grid-cols-2">
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-[0.08em] text-neutral-600 mb-1">
              {S.ontology.domainLabel}
            </div>
            <MultiSearchSelect
              values={domains}
              options={typeOpts}
              onToggle={(id) => toggle(setDomains, id)}
              placeholder={S.ontology.searchTypes}
              emptyHint={S.ontology.anyType}
            />
          </div>
          <div className="min-w-0">
            <div className="text-[10px] uppercase tracking-[0.08em] text-neutral-600 mb-1">
              {S.ontology.rangeLabel}
            </div>
            <MultiSearchSelect
              values={ranges}
              options={typeOpts}
              onToggle={(id) => toggle(setRanges, id)}
              placeholder={S.ontology.searchTypes}
              emptyHint={S.ontology.anyType}
            />
          </div>
        </div>
      </div>
      <div>
        <label className={lbl}>{S.ontology.temporal}</label>
        <Dropdown
          value={temporal}
          onChange={setTemporal}
          className="w-full"
          options={[
            { value: "state", label: S.ontology.temporalState },
            { value: "event", label: S.ontology.temporalEvent },
            { value: "eternal", label: S.ontology.temporalEternal },
          ]}
        />
      </div>
      {/* The six axioms are merged into one group. **They were always the same family** -- the
          reasoner (0002) takes them as its criteria, and scattering them around the form makes
          people believe the first two and the last four are two different things.
          Under each one, spell out "what happens if you tick this": these switches are not
          descriptions, they are declarations that change how the system behaves -- `functional`
          makes the temporal engine close old values off automatically, `transitive` makes the
          reasoner add edges to the graph. A switch whose consequences are invisible only ever
          gets ticked on instinct. */}
      <div>
        <label className={lbl}>{S.ontology.axioms}</label>
        <p className="text-[11px] leading-relaxed text-neutral-600 mb-1.5">
          {S.ontology.axiomsHint}
        </p>
        <div className="space-y-1.5">
          {(
            [
              [functional, setFunctional, S.ontology.functional, S.ontology.functionalHint],
              [
                inverseFunctional,
                setInverseFunctional,
                S.ontology.inverseFunctional,
                S.ontology.inverseFunctionalHint,
              ],
              [transitive, setTransitive, S.ontology.transitive, S.ontology.transitiveHint],
              [symmetric, setSymmetric, S.ontology.symmetric, S.ontology.symmetricHint],
              [asymmetric, setAsymmetric, S.ontology.asymmetric, S.ontology.asymmetricHint],
              [
                irreflexive,
                setIrreflexive,
                S.ontology.irreflexive,
                S.ontology.irreflexiveHint,
              ],
            ] as const
          ).map(([on, set, title, hint], i) => (
            <label key={i} className="flex items-start gap-2 cursor-pointer">
              <input
                type="checkbox"
                className="mt-0.5 accent-[var(--u-accent)]"
                checked={on}
                onChange={(e) => set(e.target.checked)}
              />
              <span className="min-w-0">
                <span className="block text-[13px] text-neutral-200">{title}</span>
                <span className="block text-[11px] leading-relaxed text-neutral-500">
                  {hint}
                </span>
              </span>
            </label>
          ))}
        </div>
        {/* Ticking symmetric and asymmetric together is self-contradictory (it holds only for
            the empty relation). The ontology self-consistency check does report it, but a word
            said on the spot here beats making someone run the check to find out */}
        {symmetric && asymmetric && (
          <p className="mt-1.5 text-[11px] text-[var(--u-danger)]">
            {S.ontology.axiomConflict}
          </p>
        )}
        {/* The last two of the same group, differing only in shape: they point at **another
            relation**, so they are dropdowns and not checkboxes. They go here rather than in a
            section of their own -- of the reasoner's four rule sources, two are the ticks above
            and two are the selects below, and separating them makes people believe they are two
            different things (0002) */}
        <div className="mt-3 space-y-2.5 border-t border-white/5 pt-3">
          {(
            [
              [
                inverseOf,
                setInverseOf,
                S.ontology.inverseOf,
                S.ontology.inverseOfHint,
                linkOptions(inverseOf),
              ],
              [
                subPropertyOf,
                setSubPropertyOf,
                S.ontology.subPropertyOf,
                S.ontology.subPropertyOfHint,
                linkOptions(subPropertyOf),
              ],
            ] as const
          ).map(([value, set, title, hint, options], i) => (
            <div key={i}>
              <div className="text-[13px] text-neutral-200">{title}</div>
              <p className="text-[11px] leading-relaxed text-neutral-500 mb-1">
                {hint}
              </p>
              <SearchSelect
                value={value}
                onChange={set}
                options={options}
                size="sm"
                className="w-full"
                placeholder={S.ontology.noLink}
              />
            </div>
          ))}
          {/* Once one is picked, say the whole thing on the spot. **The facts these two derive
              do not necessarily run in the same direction** -- the inverse swaps subject and
              object, the sub-property does not, the names alone do not tell them apart, and
              spelling it out does */}
          {(inverseOf || subPropertyOf) && (
            <div className="text-[11px] leading-relaxed text-neutral-500 space-y-0.5">
              {inverseOf && (
                <div>
                  {S.ontology.linkMeansInverse(
                    label.trim() || key || "?",
                    nameOf(inverseOf),
                  )}
                </div>
              )}
              {subPropertyOf && (
                <div>
                  {S.ontology.linkMeansSuper(
                    label.trim() || key || "?",
                    nameOf(subPropertyOf),
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
      <div>
        <label className={lbl}>{S.ontology.description}</label>
        <textarea
          className="input-dark w-full px-3 py-2 text-sm min-h-[4.5rem] resize-y"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
        <p className="mt-1 text-[10.5px] text-neutral-600">
          {S.ontology.descriptionHint}
        </p>
      </div>
      <div className="flex gap-2 pt-1">
        <Button
          size="sm"
          onClick={() => save.mutate()}
          disabled={save.isPending || !label.trim()}
        >
          {S.ontology.save}
        </Button>
        {existing && !existing.builtin && (
          <Button
            size="sm"
            variant="ghost"
            disabled={existing.usage > 0}
            title={existing.usage > 0 ? S.ontology.deleteBlocked : undefined}
            onClick={() => remove.mutate()}
          >
            {S.ontology.delete}
          </Button>
        )}
      </div>
    </div>
  );
}

/* ---------- Unmatched signals + AI suggestions ---------- */


/** Type resolution: swap a class that is "roughly right" for the more specific one.
 *
 * **Two steps, the same shape as the ontology import**: look once at what will happen, then
 * decide whether to commit. There is one more reason here -- a retype does not go on the
 * timeline, and unlike a fact rewrite it does not show itself in the entity history, so
 * "look first" is the only chance there is to see it.
 *
 * A run splits into three buckets, each with its own handling: the ones changed automatically
 * (undoable as a batch), the ones that crossed a classification axis and are left to a person,
 * and the ones the adjudication said "none of these" about. **That last bucket comes with its
 * reasons** -- this step is betting on "choosing none of these is a respectable answer", and
 * with no reasons recorded the largest bucket is the opaque one.
 */
function RefinePanel({
  kbId,
  onChanged,
  onError,
}: {
  kbId: string;
  onChanged: () => void;
  onError: (e: Error) => void;
}) {
  const [preview, setPreview] = useState<TypeSuggestion[] | null>(null);
  const [outcome, setOutcome] = useState<ResolutionOutcome | null>(null);

  const look = useMutation({
    mutationFn: () => api.typeResolutionPreview(kbId),
    onSuccess: (d) => {
      setPreview(d.items);
      setOutcome(null);
    },
    onError,
  });
  const run = useMutation({
    mutationFn: () => api.typeResolutionApply(kbId),
    onSuccess: (d) => {
      setOutcome(d);
      setPreview(null);
      onChanged();
    },
    onError,
  });
  const approve = useMutation({
    mutationFn: (v: {
      from_type_id: string;
      to_type_id: string;
      entity_ids: string[];
    }) => api.approveRefinement(kbId, v),
    onSuccess: () => {
      toast.success(S.toast.saved);
      onChanged();
    },
    onError,
  });
  const undo = useMutation({
    mutationFn: (batch: string) => api.typeResolutionUndo(kbId, batch),
    onSuccess: (d) => {
      toast.success(S.ontology.refineUndone(d.reverted));
      setOutcome(null);
      onChanged();
    },
    onError,
  });

  const busy = look.isPending || run.isPending;

  return (
    <div className="space-y-4">
      <div>
        <h3 className="u-title text-lg mb-1">{S.ontology.refineTitle}</h3>
        <p className="text-xs leading-relaxed text-neutral-500 max-w-xl">
          {S.ontology.refineHint}
        </p>
      </div>

      <div className="flex gap-2">
        <button
          className="u-btn text-xs"
          disabled={busy}
          onClick={() => look.mutate()}
        >
          {look.isPending ? S.ontology.refineLooking : S.ontology.refinePreview}
        </button>
        <button
          className="u-btn u-btn-primary text-xs"
          disabled={busy}
          onClick={() => run.mutate()}
        >
          {run.isPending ? S.ontology.refineRunning : S.ontology.refineRun}
        </button>
      </div>

      {/* ---- The compute-but-do-not-write step */}
      {preview && (
        <div className="space-y-2">
          <p className="text-xs text-neutral-500">
            {preview.length === 0
              ? S.ontology.refineNothing
              : S.ontology.refineCandidates(preview.length)}
          </p>
          {preview.map((s) => (
            <div key={s.entity_id} className="glass rounded-xl p-3">
              <div className="flex items-baseline gap-2 flex-wrap">
                <span className="text-sm text-neutral-100">{s.name}</span>
                <span className="text-[11px] text-neutral-500">
                  {s.coarse ?? S.graph.untyped}
                </span>
                {s.specific_type && (
                  <span className="text-[11px] text-[var(--u-warn)]">
                    {S.ontology.refineModelSays(s.specific_type)}
                  </span>
                )}
                <span className="ml-auto u-num text-[10.5px] text-neutral-600">
                  {S.review.factsCount(s.fact_count)}
                </span>
              </div>
              {/* **Show the piece of text that was sent to retrieval**: when nothing is found,
                  the first thing to look at is what we went looking with, not a guess at whether
                  the problem is the profile or the class descriptions */}
              <p className="mt-1 text-[11px] text-neutral-500 line-clamp-2">
                {s.profile}
              </p>
              <div className="mt-1.5 flex flex-wrap gap-1">
                {s.candidates.slice(0, 6).map((c) => (
                  <span
                    key={c.id}
                    title={c.description}
                    className="u-chip u-chip-neutral u-num text-[10.5px]"
                  >
                    {c.label} {c.distance.toFixed(2)}
                  </span>
                ))}
                {s.candidates.length === 0 && (
                  <span className="text-[11px] text-neutral-600">
                    {S.ontology.refineNoCandidates}
                  </span>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ---- The three buckets after it has been written */}
      {outcome && (
        <div className="space-y-3">
          <div className="flex items-center gap-3 flex-wrap">
            <span className="text-xs text-neutral-300">
              {S.ontology.refineRetyped(outcome.retyped)}
            </span>
            {outcome.batch && outcome.retyped > 0 && (
              <button
                className="u-btn u-btn-ghost text-xs"
                disabled={undo.isPending}
                onClick={() => undo.mutate(outcome.batch!)}
              >
                {S.ontology.refineUndo}
              </button>
            )}
          </div>

          {outcome.for_review.length > 0 && (
            <div className="space-y-2">
              <p className="text-xs text-neutral-500">
                {S.ontology.refineForReview(outcome.for_review.length)}
              </p>
              {outcome.for_review.map((r) => (
                <div key={r.entity_id} className="glass rounded-xl p-3">
                  <div className="flex items-baseline gap-2 flex-wrap">
                    <span className="text-sm text-neutral-100">{r.name}</span>
                    <span className="text-[11px] text-neutral-500">
                      {r.coarse ?? S.graph.untyped} → {r.choice}
                    </span>
                    {r.crosses_axis && (
                      <span className="u-chip u-chip-warn text-[10.5px]">
                        {S.ontology.refineCrossesAxis}
                      </span>
                    )}
                    <span className="ml-auto u-num text-[10.5px] text-neutral-600">
                      {Math.round(r.confidence * 100)}%
                    </span>
                  </div>
                  {r.reason && (
                    <p className="mt-1 text-[11px] text-neutral-500">
                      {r.reason}
                    </p>
                  )}
                  {/* **What gets approved is this pair of classes, not this one entity.**
                      Approve once and the same pair stops coming to a person -- which is exactly
                      what most of the entries in this bucket are caused by */}
                  {r.from_type_id && (
                    <button
                      className="u-btn u-btn-primary mt-2 text-xs"
                      disabled={approve.isPending}
                      onClick={() =>
                        approve.mutate({
                          from_type_id: r.from_type_id!,
                          to_type_id: r.to_type_id,
                          entity_ids: [r.entity_id],
                        })
                      }
                    >
                      {S.ontology.refineApprovePair}
                    </button>
                  )}
                </div>
              ))}
            </div>
          )}

          {outcome.left_alone.length > 0 && (
            <div className="space-y-1.5">
              <p className="text-xs text-neutral-500">
                {S.ontology.refineLeftAlone(outcome.left_alone.length)}
              </p>
              {outcome.left_alone.map((d, i) => (
                <div key={i} className="glass rounded-xl px-3 py-2">
                  <div className="flex items-baseline gap-2 flex-wrap">
                    <span className="text-[13px] text-neutral-200">
                      {d.name}
                    </span>
                    <span className="text-[11px] text-neutral-500">
                      {d.coarse ?? S.graph.untyped}
                    </span>
                  </div>
                  {/* The reason comes together with the top candidate: when the reason does not
                      hold up, the candidate tells you whether retrieval failed to find it or
                      the adjudication passed it over */}
                  {d.reason && (
                    <p className="mt-0.5 text-[11px] text-neutral-500">
                      {d.reason}
                    </p>
                  )}
                  {d.top_candidate && (
                    <p className="mt-0.5 text-[11px] text-neutral-600">
                      {S.ontology.refineTopCandidate(d.top_candidate)}
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
function MissesPanel({
  kbId,
  misses,
  dismissedMisses,
  onChanged,
  onError,
}: {
  kbId: string;
  misses: OntologyMiss[];
  dismissedMisses: OntologyMiss[];
  onChanged: () => void;
  onError: (e: unknown) => void;
}) {
  // Collapsed by default: the dismissed ones are **background information** and should not be
  // crowded in with the pending ones, competing for attention
  const [showDismissed, setShowDismissed] = useState(false);
  const [proposals, setProposals] = useState<OntologyProposals | null>(null);
  // The ones computed last time that nobody has ruled on yet: fetched back out of the database
  // after a page refresh (0049).
  //
  // This used to be nothing but the useState above -- one refresh, one navigation away, and the
  // whole batch of suggestions was gone; seeing it again meant rerunning the model, and a rerun
  // does not necessarily give the same set of merges. Which wordings got merged is the only
  // thing there is for verifying a merge (0003's optimized_for → runs_on was caught exactly
  // that way)
  const storedProposals = useQuery({
    queryKey: ["storedProposals", kbId],
    queryFn: () => api.storedProposals(kbId),
  });
  useEffect(() => {
    // Backfill only while there is no local result yet. The batch from a just-clicked Suggest is
    // the fresher one and should not be overwritten
    if (proposals === null && storedProposals.data) {
      const d = storedProposals.data;
      const empty =
        !d.entity_types?.length &&
        !d.relation_types?.length &&
        !d.attribute_types?.length;
      if (!empty) setProposals(d);
    }
  }, [storedProposals.data, proposals]);
  // The most recent adoption, for undoing. Only the most recent is kept -- undoing an older
  // batch goes through the audit ledger, which already records which relation each adoption
  // touched and how many rows
  const [lastAdopt, setLastAdopt] = useState<{
    batches: string[];
    key: string;
    moved: number;
  } | null>(null);
  // Undoing takes a second confirmation: it changes facts back by the batch
  const [confirmUndo, setConfirmUndo] = useState<{
    batches: string[];
    moved: number;
  } | null>(null);
  // Has the system extended the ontology on its own -- the banner shows on the strength of this,
  // and once it has been undone cleanly the backend returns null
  const autoRun = useQuery({
    queryKey: ["auto-extension", kbId],
    queryFn: () => api.lastAutoExtension(kbId),
  });
  // The surface predicates waiting to be claimed: a proposal's blast radius ("will rewrite 57
  // of them") is computed from here
  const surface = useQuery({
    queryKey: ["proposed-predicates", kbId],
    queryFn: () => api.proposedPredicates(kbId),
  });
  const factsWaiting = (forms: string[]) => {
    const byForm = new Map(
      (surface.data?.forms ?? []).map((f) => [f.form, f.fact_count]),
    );
    return forms.reduce((n, f) => n + (byForm.get(f) ?? 0), 0);
  };

  const suggest = useMutation({
    mutationFn: () => api.suggestOntology(kbId),
    onSuccess: setProposals,
    onError,
  });
  const dismiss = useMutation({
    mutationFn: ({ kind, key }: { kind: string; key: string }) =>
      api.dismissMiss(kbId, kind, key),
    onSuccess: onChanged,
    onError,
  });
  const restore = useMutation({
    mutationFn: ({ kind, key }: { kind: string; key: string }) =>
      api.restoreMiss(kbId, kind, key),
    onSuccess: onChanged,
    onError,
  });
  const approveEntity = useMutation({
    mutationFn: (p: { key: string; label: string; description?: string }) =>
      api.createEntityType(kbId, {
        key: p.key,
        label: p.label,
        description: p.description,
      }),
    onSuccess: (_data, p) => {
      // Adopted: remove it from the proposal list, and clear the matching unmatched-count chip
      // along the way (the ontology covers it now)
      toast.success(S.toast.added);
      setProposals(
        (prev) =>
          prev && {
            ...prev,
            entity_types: prev.entity_types.filter((x) => x.key !== p.key),
          },
      );
      api.dismissMiss(kbId, "entity_type", p.key).catch(() => {});
      // Store the ruling on the proposal (0049): the next round of Suggest will not push it back
      // into the waiting list
      api.decideProposal(kbId, "entity_types", p.key, "adopted").catch(() => {});
      onChanged();
    },
    onError,
  });
  const approveRelation = useMutation({
    // A proposal that carries forms goes through adopt: creating the relation also claims the
    // predicate-less facts waiting on it. Create the relation only and the ontology has grown
    // while the graph is no better -- those facts go on saying "is related to"
    mutationFn: (p: {
      key: string;
      label: string;
      temporal?: string;
      functional?: boolean;
      description?: string;
      forms?: string[];
    }) =>
      p.forms?.length
        ? api.adoptPredicate(kbId, {
            key: p.key,
            label: p.label,
            temporal: p.temporal ?? "state",
            functional: p.functional ?? false,
            description: p.description,
            forms: p.forms,
          })
        : api.createRelationType(kbId, {
            key: p.key,
            label: p.label,
            temporal: p.temporal ?? "state",
            functional: p.functional ?? false,
            description: p.description,
          }),
    onSuccess: (data, p) => {
      const d = data as { remapped?: number; batch?: string };
      const moved = d.remapped ?? 0;
      toast.success(moved > 0 ? S.ontology.adopted(moved) : S.toast.added);
      // The handle for undoing: an adoption rewrites facts by the batch, and with no way back
      // nobody dares click the first time
      if (moved > 0 && d.batch)
        setLastAdopt({ batches: [d.batch], key: p.key, moved });
      setProposals(
        (prev) =>
          prev && {
            ...prev,
            relation_types: prev.relation_types.filter((x) => x.key !== p.key),
          },
      );
      api.dismissMiss(kbId, "relation_type", p.key).catch(() => {});
      api.decideProposal(kbId, "relation_types", p.key, "adopted").catch(() => {});
      onChanged();
    },
    onError,
  });

  // One at a time in series rather than adding a bulk endpoint: every predicate has its own
  // batch and its own undo granularity, and a partial failure can be reported honestly
  // ("5 succeeded, 1 key already exists") instead of rolling the whole batch back
  // Attribute proposals: the ones whose object is a literal. They go through the same adoption
  // entry point, but the value has to be converted per the datatype and what will not convert
  // is not rewritten -- which is why the unconvertible count in the response has to be said
  const approveAttribute = useMutation({
    mutationFn: (p: {
      key: string;
      label: string;
      datatype?: string;
      unit?: string;
      description?: string;
      forms?: string[];
    }) =>
      api.adoptPredicate(kbId, {
        key: p.key,
        kind: "attribute",
        label: p.label,
        datatype: p.datatype ?? "text",
        unit: p.unit,
        description: p.description,
        forms: p.forms ?? [],
      }),
    onSuccess: (data, p) => {
      const moved = data.remapped ?? 0;
      const left = data.unconvertible ?? 0;
      toast.success(
        left > 0
          ? S.ontology.adoptedPartly(moved, left)
          : moved > 0
            ? S.ontology.adopted(moved)
            : S.toast.added,
      );
      if (moved > 0 && data.batch)
        setLastAdopt({ batches: [data.batch], key: p.key, moved });
      setProposals(
        (prev) =>
          prev && {
            ...prev,
            attribute_types: (prev.attribute_types ?? []).filter(
              (x) => x.key !== p.key,
            ),
          },
      );
      for (const form of p.forms ?? [])
        api.dismissMiss(kbId, "attribute_type", form).catch(() => {});
      api.decideProposal(kbId, "attribute_types", p.key, "adopted").catch(() => {});
      onChanged();
    },
    onError,
  });
  // Map onto an existing type: create nothing, just move these wordings' facts across.
  // It goes through the same adoption entry point as a create, because what it does to the graph
  // is exactly the same -- and therefore it is just as undoable
  const approveMapping = useMutation({
    mutationFn: (p: { key: string; kind?: string; forms?: string[] }) =>
      api.adoptPredicate(kbId, {
        key: p.key,
        existing: true,
        // When the target is an attribute the value has to be converted per its datatype, and
        // this is what the server routes on
        kind: p.kind === "attribute" ? "attribute" : "relation",
        forms: p.forms ?? [],
      }),
    onSuccess: (data, p) => {
      const moved = data.remapped ?? 0;
      const left = data.unconvertible ?? 0;
      toast.success(
        left > 0
          ? S.ontology.adoptedPartly(moved, left)
          : moved > 0
            ? S.ontology.adopted(moved)
            : S.toast.saved,
      );
      if (moved > 0 && data.batch)
        setLastAdopt({ batches: [data.batch], key: p.key, moved });
      setProposals(
        (prev) =>
          prev && {
            ...prev,
            map_to: (prev.map_to ?? []).filter((x) => x.key !== p.key),
          },
      );
      for (const form of p.forms ?? [])
        api.dismissMiss(kbId, "relation_type", form).catch(() => {});
      onChanged();
    },
    onError,
  });
  const addAll = useMutation({
    mutationFn: async (all: OntologyProposals) => {
      const batches: string[] = [];
      let moved = 0;
      const failed: string[] = [];
      for (const p of all.entity_types) {
        try {
          await api.createEntityType(kbId, { key: p.key, label: p.label });
        } catch {
          failed.push(p.key);
        }
      }
      for (const p of all.relation_types) {
        try {
          if (p.forms?.length) {
            const r = await api.adoptPredicate(kbId, {
              key: p.key,
              label: p.label,
              temporal: p.temporal ?? "state",
              functional: p.functional ?? false,
              description: p.description,
              forms: p.forms,
            });
            moved += r.remapped;
            if (r.remapped > 0) batches.push(r.batch);
          } else {
            await api.createRelationType(kbId, {
              key: p.key,
              label: p.label,
              temporal: p.temporal ?? "state",
              functional: p.functional ?? false,
              description: p.description,
            });
          }
        } catch {
          failed.push(p.key);
        }
      }
      for (const p of all.attribute_types ?? []) {
        if (!p.forms?.length) continue;
        try {
          const r = await api.adoptPredicate(kbId, {
            key: p.key,
            kind: "attribute",
            label: p.label,
            datatype: p.datatype ?? "text",
            unit: p.unit,
            description: p.description,
            forms: p.forms,
          });
          moved += r.remapped;
          if (r.remapped > 0) batches.push(r.batch);
        } catch {
          failed.push(p.key);
        }
      }
      for (const p of all.map_to ?? []) {
        if (!p.forms?.length) continue;
        try {
          const r = await api.adoptPredicate(kbId, {
            key: p.key,
            existing: true,
            kind: p.kind === "attribute" ? "attribute" : "relation",
            forms: p.forms,
          });
          moved += r.remapped;
          if (r.remapped > 0) batches.push(r.batch);
        } catch {
          failed.push(p.key);
        }
      }
      return { batches, moved, failed };
    },
    onSuccess: (r) => {
      if (r.failed.length) toast.error(S.ontology.addAllPartial(r.failed));
      else toast.success(S.ontology.adopted(r.moved));
      if (r.batches.length)
        setLastAdopt({
          batches: r.batches,
          key: S.ontology.addAllLabel,
          moved: r.moved,
        });
      setProposals(null);
      onChanged();
    },
    onError,
  });

  const unadopt = useMutation({
    mutationFn: async (batches: string[]) => {
      let reverted = 0;
      for (const b of batches)
        reverted += (await api.unadoptPredicate(kbId, b)).reverted;
      return { reverted };
    },
    onSuccess: (r) => {
      toast.success(S.ontology.reverted(r.reverted));
      setLastAdopt(null);
      setConfirmUndo(null);
      autoRun.refetch();
      onChanged();
    },
    onError,
  });

  return (
    <div className="glass rounded-xl p-4">
      <div className="flex items-center gap-3 mb-1">
        <h3 className="text-sm font-bold text-neutral-200">
          {S.ontology.misses}
        </h3>
        {misses.length > 0 && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => suggest.mutate()}
            disabled={suggest.isPending}
          >
            {suggest.isPending ? S.ontology.suggesting : S.ontology.suggest}
          </Button>
        )}
      </div>
      <p className="text-xs text-neutral-500 mb-3">{S.ontology.missesHint}</p>

      {misses.length === 0 ? (
        <p className="text-sm text-neutral-500">{S.ontology.noMisses}</p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {misses.map((m) => (
            <span
              key={`${m.kind}:${m.key}`}
              className="glass rounded-full px-2.5 py-1 text-xs flex items-center gap-1.5"
              title={m.example ?? ""}
            >
              <Chip tone={m.kind === "entity_type" ? "info" : "violet"}>
                {m.kind === "entity_type" ? "C" : "P"}
              </Chip>
              <span className="font-mono text-neutral-300">{m.key}</span>
              <span className="text-neutral-500">×{m.count}</span>
              <button
                onClick={() => dismiss.mutate({ kind: m.kind, key: m.key })}
                className="text-neutral-600 hover:text-neutral-300"
              >
                ✕
              </button>
            </span>
          ))}
        </div>
      )}
      {dismissedMisses.length > 0 && (
        <div className="mt-3 border-t border-white/5 pt-3">
          <button
            onClick={() => setShowDismissed((v) => !v)}
            className="text-xs text-neutral-500 hover:text-neutral-300"
          >
            {showDismissed ? "▾" : "▸"} {S.ontology.dismissed(dismissedMisses.length)}
          </button>
          {showDismissed && (
            <>
              <p className="text-xs text-neutral-600 mt-1.5 mb-2">
                {S.ontology.dismissedHint}
              </p>
              <div className="flex flex-wrap gap-1.5">
                {dismissedMisses.map((m) => (
                  <span
                    key={`d:${m.kind}:${m.key}`}
                    className="glass rounded-full px-2.5 py-1 text-xs flex items-center gap-1.5 opacity-60"
                    title={m.example ?? ""}
                  >
                    <Chip tone={m.kind === "entity_type" ? "info" : "violet"}>
                      {m.kind === "entity_type" ? "C" : "P"}
                    </Chip>
                    <span className="font-mono text-neutral-400 line-through">
                      {m.key}
                    </span>
                    <span className="text-neutral-500">×{m.count}</span>
                    <button
                      onClick={() => restore.mutate({ kind: m.kind, key: m.key })}
                      className="text-neutral-600 hover:text-neutral-200"
                      title={S.ontology.restore}
                    >
                      ↺
                    </button>
                  </span>
                ))}
              </div>
            </>
          )}
        </div>
      )}
      {/* The system touched the ontology by itself, and somebody has to see that -- recorded in
          the audit ledger and nowhere else does not count as visible. Being on by default is
          conditional on its actions being visible and reversible, and this banner is the
          "visible" half */}
      {autoRun.data?.run && !lastAdopt && (
        <div className="mt-3 rounded-lg border border-[var(--u-accent)]/25 bg-[var(--u-accent)]/[0.06] px-3 py-2.5">
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <p className="text-xs text-neutral-200">
                {S.ontology.autoRanTitle}
              </p>
              <p className="mt-0.5 text-[11px] text-neutral-400">
                {S.ontology.autoRanBody(
                  autoRun.data.run.relations ?? [],
                  autoRun.data.run.facts_remapped ?? 0,
                )}
              </p>
              <p className="mt-0.5 text-[11px] text-neutral-600">
                {S.ontology.autoRanOff}
              </p>
            </div>
            <Button
              size="sm"
              variant="ghost"
              disabled={unadopt.isPending}
              onClick={() =>
                setConfirmUndo({
                  batches: autoRun.data!.run!.batches,
                  moved: autoRun.data!.run!.facts_remapped ?? 0,
                })
              }
            >
              {S.ontology.undoAdoptBtn}
            </Button>
          </div>
        </div>
      )}

      {/* An adoption rewrites facts by the batch -- with no way back nobody dares click the
          first time */}
      {lastAdopt && (
        <div className="mt-3 flex items-center gap-2 rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2">
          <span className="text-xs text-neutral-300">
            {S.ontology.undoAdopt(lastAdopt.key, lastAdopt.moved)}
          </span>
          <span className="text-[11px] text-neutral-600">
            {S.ontology.undoKeepsRelation}
          </span>
          <Button
            size="sm"
            variant="ghost"
            className="ml-auto"
            disabled={unadopt.isPending}
            onClick={() =>
              setConfirmUndo({
                batches: lastAdopt.batches,
                moved: lastAdopt.moved,
              })
            }
          >
            {S.ontology.undoAdoptBtn}
          </Button>
        </div>
      )}

      {/* One undo changes facts back by the batch: a light confirmation -- the undo is itself
          reversible, so there is no typing to unlock */}
      {confirmUndo && (
        <DangerConfirm
          title={S.ontology.undoTitle}
          hint={S.ontology.undoHint(confirmUndo.moved)}
          confirmLabel={S.ontology.undoConfirm}
          cancelLabel={S.ontology.undoCancel}
          busy={unadopt.isPending}
          onConfirm={() => unadopt.mutate(confirmUndo.batches)}
          onCancel={() => setConfirmUndo(null)}
        />
      )}
      {proposals && (
        <div className="mt-4 border-t border-white/10 pt-3">
          <div className="mb-2 flex items-center gap-2">
            <h4 className="text-xs font-bold text-neutral-400">
              {S.ontology.proposals}
            </h4>
            {/* The common case is "these are all right" -- clicking them one by one splits one
                decision into eight */}
            {proposals.relation_types.length +
              proposals.entity_types.length +
              (proposals.attribute_types?.length ?? 0) +
              (proposals.map_to?.length ?? 0) >
              1 && (
              <Button
                size="sm"
                variant="ghost"
                className="ml-auto"
                disabled={addAll.isPending}
                onClick={() => addAll.mutate(proposals)}
              >
                {addAll.isPending
                  ? S.ontology.addingAll
                  : S.ontology.addAll(
                      proposals.relation_types.length +
                        proposals.entity_types.length +
                        (proposals.attribute_types?.length ?? 0) +
                        (proposals.map_to?.length ?? 0),
                    )}
              </Button>
            )}
          </div>
          <div className="space-y-1.5">
            {/* First in the order: what it says is "the ontology already has this", and that is
                exactly the sentence that most needs to be seen first. Put it after the creates
                and someone clicking straight down the list will have built the duplicate */}
            {(proposals.map_to ?? []).map((p) => (
              <div key={`map-${p.key}`} className="flex items-center gap-2 text-sm">
                <Chip tone="success">=</Chip>
                <span className="font-mono text-neutral-300">{p.key}</span>
                {!!p.forms?.length && (
                  <span
                    className="text-xs text-neutral-400 truncate"
                    title={p.forms.join(" · ")}
                  >
                    {p.forms.join(" · ")}
                  </span>
                )}
                {!!p.forms?.length && (
                  <span className="text-xs text-[var(--u-accent)]">
                    {S.ontology.willRemap(factsWaiting(p.forms))}
                  </span>
                )}
                {p.reason && (
                  <span className="text-xs text-neutral-500 truncate">
                    {p.reason}
                  </span>
                )}
                <Button
                  size="sm"
                  className="ml-auto"
                  onClick={() => approveMapping.mutate(p)}
                  disabled={approveMapping.isPending}
                >
                  {S.ontology.mapOver}
                </Button>
              </div>
            ))}
            {proposals.entity_types.map((p) => (
              <div key={p.key} className="flex items-center gap-2 text-sm">
                <Chip tone="info">C</Chip>
                <span className="font-mono text-neutral-300">{p.key}</span>
                <span className="text-neutral-200">{p.label}</span>
                {p.reason && (
                  <span className="text-xs text-neutral-500 truncate">
                    {p.reason}
                  </span>
                )}
                <Button
                  size="sm"
                  className="ml-auto"
                  onClick={() => approveEntity.mutate(p)}
                  disabled={approveEntity.isPending}
                >
                  {S.ontology.approve}
                </Button>
              </div>
            ))}
            {proposals.relation_types.map((p) => (
              <div key={p.key} className="flex items-center gap-2 text-sm">
                <Chip tone="violet">P</Chip>
                <span className="font-mono text-neutral-300">{p.key}</span>
                <span className="text-neutral-200">{p.label}</span>
                {p.temporal && <Chip tone="neutral">{p.temporal}</Chip>}
                {/* The blast radius: how many get rewritten on adoption, and which spellings it
                    merged. Without this, "approve" is just one more empty relation out of thin
                    air */}
                {!!p.forms?.length && (
                  <span
                    className="text-xs text-[var(--u-accent)]"
                    title={p.forms.join(" · ")}
                  >
                    {S.ontology.willRemap(factsWaiting(p.forms))}
                  </span>
                )}
                {p.reason && (
                  <span className="text-xs text-neutral-500 truncate">
                    {p.reason}
                  </span>
                )}
                <Button
                  size="sm"
                  className="ml-auto"
                  onClick={() => approveRelation.mutate(p)}
                  disabled={approveRelation.isPending}
                >
                  {S.ontology.approve}
                </Button>
              </div>
            ))}
            {(proposals.attribute_types ?? []).map((p) => (
              <div key={`attr-${p.key}`} className="flex items-center gap-2 text-sm">
                {/* A and not P: the literal-value bucket is a different thing from a relation,
                    and the UI keeps them apart */}
                <Chip tone="warn">A</Chip>
                <span className="font-mono text-neutral-300">{p.key}</span>
                <span className="text-neutral-200">{p.label}</span>
                <Chip tone="neutral">{p.datatype ?? "text"}</Chip>
                {p.unit && <Chip tone="neutral">{p.unit}</Chip>}
                {!!p.forms?.length && (
                  <span
                    className="text-xs text-[var(--u-accent)]"
                    title={p.forms.join(" · ")}
                  >
                    {S.ontology.willRemap(factsWaiting(p.forms))}
                  </span>
                )}
                {p.reason && (
                  <span className="text-xs text-neutral-500 truncate">
                    {p.reason}
                  </span>
                )}
                <Button
                  size="sm"
                  className="ml-auto"
                  onClick={() => approveAttribute.mutate(p)}
                  disabled={approveAttribute.isPending}
                >
                  {S.ontology.approve}
                </Button>
              </div>
            ))}
            {proposals.entity_types.length === 0 &&
              proposals.relation_types.length === 0 &&
              !proposals.attribute_types?.length &&
              !proposals.map_to?.length && (
                <p className="text-sm text-neutral-500">—</p>
              )}
          </div>
        </div>
      )}
    </div>
  );
}

/* ---------- Ontology import: upload → preview the plan → confirm and write ---------- */
/* Preview and write share one and the same plan on the server. This panel's entire job is to
   put **the three things in the plan that bite** in front of a person before he clicks confirm:
   functional relations (a wrong uniqueness declaration manufactures false conflicts in droves),
   classes with no description (the description goes into the extraction prompt verbatim, and a
   missing one silently degrades extraction), and key collisions (reported, not resolved --
   renaming automatically would leave the next re-import unable to recognize which entry it
   created last time). */

function ImportPanel({
  kbId,
  onChanged,
  onError,
}: {
  kbId: string;
  onChanged: () => void;
  onError: (e: unknown) => void;
}) {
  const [file, setFile] = useState<File | null>(null);
  const pick = useRef<HTMLInputElement>(null);
  const queryClient = useQueryClient();

  const history = useQuery({
    queryKey: ["ontology-imports", kbId],
    queryFn: () => api.ontologyImports(kbId),
  });

  const preview = useMutation({
    mutationFn: (f: File) => api.previewOntologyImport(kbId, f),
    onError: (e) => {
      setFile(null);
      onError(e);
    },
  });

  const apply = useMutation({
    mutationFn: (f: File) => api.applyOntologyImport(kbId, f),
    onSuccess: (res) => {
      const p = res.plan;
      toast.success(
        S.ontology.importDone(
          p.classes.filter((c) => c.disposition === "create").length,
          p.classes.filter((c) => c.disposition === "update").length,
        ),
      );
      setFile(null);
      preview.reset();
      queryClient.invalidateQueries({ queryKey: ["ontology-imports", kbId] });
      onChanged();
    },
    onError,
  });

  const choose = (f: File | undefined) => {
    if (!f) return;
    setFile(f);
    preview.mutate(f);
  };

  const plan = preview.data?.plan ?? null;
  const busy = preview.isPending || apply.isPending;
  const empty =
    plan &&
    plan.classes.length === 0 &&
    plan.relations.length === 0 &&
    plan.attributes.length === 0;

  return (
    <div className="glass rounded-xl p-4">
      <h3 className="text-sm font-bold text-neutral-200 mb-1">
        {S.ontology.importTitle}
      </h3>
      <p className="text-xs text-neutral-500 mb-3">{S.ontology.importHint}</p>

      <input
        ref={pick}
        type="file"
        accept=".owl,.rdf,.ttl,.xml,.n3"
        className="hidden"
        onChange={(e) => {
          choose(e.target.files?.[0]);
          e.target.value = "";
        }}
      />
      <div className="flex items-center gap-2">
        <Button
          size="sm"
          variant="ghost"
          disabled={busy}
          onClick={() => pick.current?.click()}
        >
          {file ? S.ontology.importChange : S.ontology.importPick}
        </Button>
        {file && (
          <span className="text-xs text-neutral-400 truncate">
            <span className="font-mono">{file.name}</span>
            <span className="text-neutral-600">
              {" "}
              · {S.ontology.importSize(file.size)}
            </span>
          </span>
        )}
        {preview.isPending && (
          <span className="text-xs text-neutral-500">
            {S.ontology.importReading}
          </span>
        )}
      </div>

      {plan && (
        <div className="mt-4">
          <p className="text-[11px] uppercase tracking-[0.08em] text-neutral-600 u-num">
            {S.ontology.importParsed(plan.format, plan.triples)}
          </p>

          {empty ? (
            <p className="mt-2 text-sm text-neutral-500">
              {S.ontology.importNothing}
            </p>
          ) : (
            <>
              {/* The three warnings come before the counts: people only read the first screen */}
              <Warning
                show={plan.functional_relations > 0}
                tone="warn"
                title={S.ontology.warnFunctional(plan.functional_relations)}
                body={S.ontology.warnFunctionalBody}
                items={plan.relations
                  .filter((r) => r.functional)
                  .map((r) => r.key)}
              />
              <Warning
                show={plan.classes_without_description > 0}
                tone="warn"
                title={S.ontology.warnNoDescription(
                  plan.classes_without_description,
                )}
                body={S.ontology.warnNoDescriptionBody}
                items={plan.classes
                  .filter((c) => !c.has_description)
                  .map((c) => c.key)}
              />
              <Warning
                show={takenCount(plan) > 0}
                tone="danger"
                title={S.ontology.warnKeyTaken(takenCount(plan))}
                body={S.ontology.warnKeyTakenBody}
                items={[...plan.classes, ...plan.relations, ...plan.attributes]
                  .filter((i) => i.disposition === "key_taken")
                  .map(
                    (i) =>
                      `${i.key} — ${S.ontology.importTakenBy(i.conflict_with ?? null)}`,
                  )}
              />

              <div className="mt-3 grid gap-2">
                <PlanRow
                  label={S.ontology.importClasses}
                  items={plan.classes}
                />
                <PlanRow
                  label={S.ontology.importRelations}
                  items={plan.relations}
                />
                <PlanRow
                  label={S.ontology.importAttributes}
                  items={plan.attributes}
                  note={
                    plan.attributes.length > 0
                      ? S.ontology.importAttributesLater
                      : undefined
                  }
                />
              </div>

              {plan.unprojected.length > 0 && (
                <details className="mt-3">
                  <summary className="cursor-pointer text-xs text-neutral-500 hover:text-neutral-300">
                    {S.ontology.importUnprojected} ({plan.unprojected.length})
                  </summary>
                  <p className="mt-1.5 text-[11px] text-neutral-600">
                    {S.ontology.importUnprojectedBody}
                  </p>
                  <ul className="mt-1.5 space-y-0.5">
                    {plan.unprojected.map(([iri, n]) => (
                      <li key={iri} className="flex gap-2 text-[11px]">
                        <span
                          className="font-mono text-neutral-500 truncate"
                          title={iri}
                        >
                          {shortIri(iri)}
                        </span>
                        <span className="u-num text-neutral-600 shrink-0">
                          ×{n}
                        </span>
                      </li>
                    ))}
                  </ul>
                </details>
              )}

              <div className="mt-4 flex items-center gap-2">
                <Button
                  size="sm"
                  disabled={busy}
                  onClick={() => file && apply.mutate(file)}
                >
                  {apply.isPending
                    ? S.ontology.importApplying
                    : S.ontology.importApply}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() => {
                    setFile(null);
                    preview.reset();
                  }}
                >
                  {S.ontology.importCancel}
                </Button>
              </div>
            </>
          )}
        </div>
      )}

      {/* Import history: who touched the ontology with which file and when. The source text is
          kept, keyed by sha256 */}
      <div className="mt-5 border-t border-white/10 pt-3">
        <h4 className="text-xs font-medium text-neutral-400 mb-2">
          {S.ontology.importHistory}
        </h4>
        {!history.data?.imports.length ? (
          <p className="text-xs text-neutral-600">
            {S.ontology.importNoHistory}
          </p>
        ) : (
          <ul className="space-y-1.5">
            {history.data.imports.map((im) => (
              <li key={im.id} className="flex items-baseline gap-2 text-xs">
                <span className="font-mono text-neutral-300 truncate">
                  {im.filename}
                </span>
                <span className="u-num text-neutral-600 shrink-0">
                  {S.ontology.importSize(im.byte_size)}
                </span>
                <span className="ml-auto text-[11px] text-neutral-600 shrink-0">
                  {S.ontology.importBy(
                    im.imported_by_name ?? "—",
                    new Date(im.imported_at).toLocaleDateString(),
                  )}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function takenCount(p: ImportPlan) {
  return [...p.classes, ...p.relations, ...p.attributes].filter(
    (i) => i.disposition === "key_taken",
  ).length;
}

/** The tail of an IRI is the part a person recognizes; the prefix only takes up width here */
function shortIri(iri: string) {
  const i = Math.max(iri.lastIndexOf("#"), iri.lastIndexOf("/"));
  return i < 0 ? iri : iri.slice(i + 1);
}

/** One warning: the title gives the number, one line of body gives the consequence, and the
 *  items fold away inside a details */
function Warning({
  show,
  tone,
  title,
  body,
  items,
}: {
  show: boolean;
  tone: "warn" | "danger";
  title: string;
  body: string;
  items: string[];
}) {
  if (!show) return null;
  return (
    <div
      className={cn(
        "mt-3 rounded-lg border px-3 py-2.5",
        tone === "danger"
          ? "border-rose-500/25 bg-rose-500/[0.06]"
          : "border-amber-500/25 bg-amber-500/[0.06]",
      )}
    >
      <p className="text-xs text-neutral-200">{title}</p>
      <p className="mt-0.5 text-[11px] text-neutral-400">{body}</p>
      {items.length > 0 && (
        <details className="mt-1.5">
          <summary className="cursor-pointer text-[11px] text-neutral-500 hover:text-neutral-300">
            {items.length > 1 ? `${items.length} items` : "1 item"}
          </summary>
          <ul className="mt-1 space-y-0.5">
            {items.map((it) => (
              <li key={it} className="font-mono text-[11px] text-neutral-400">
                {it}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/** The dispositions counted for one section: create / update / skip, with zeros hidden */
function PlanRow({
  label,
  items,
  note,
}: {
  label: string;
  items: PlannedItem[];
  note?: string;
}) {
  if (items.length === 0) return null;
  const n = (d: PlannedItem["disposition"]) =>
    items.filter((i) => i.disposition === d).length;
  return (
    <div className="rounded-lg bg-white/[0.03] px-3 py-2">
      <div className="flex items-center gap-2">
        <span className="text-xs text-neutral-300">{label}</span>
        <span className="ml-auto flex items-center gap-1.5">
          {n("create") > 0 && (
            <Chip tone="success">
              {S.ontology.importWillCreate(n("create"))}
            </Chip>
          )}
          {n("update") > 0 && (
            <Chip tone="info">{S.ontology.importWillUpdate(n("update"))}</Chip>
          )}
          {n("key_taken") > 0 && (
            <Chip tone="neutral">
              {S.ontology.importKeyTaken(n("key_taken"))}
            </Chip>
          )}
        </span>
      </div>
      {note && <p className="mt-1 text-[11px] text-neutral-600">{note}</p>}
    </div>
  );
}
