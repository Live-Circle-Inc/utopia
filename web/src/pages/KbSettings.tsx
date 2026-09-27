/* Knowledge base settings: sections in the left rail (General / Members / Danger zone),
   laying the skeleton for settings still to come (extraction settings, retention policy,
   KB-level tokens...). Access control is enforced on the API side (KB admin and up).
   Rendering the sections mutually exclusively also cured the stacking bug where a
   dropdown popover was hidden behind a later glass card (backdrop-filter creates its own
   stacking context). */
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams, useNavigate } from "@tanstack/react-router";
import {
  History as HistoryIcon,
  Lock,
  Settings2,
  TriangleAlert,
  Users,
} from "lucide-react";
import { api, type AuditEvent } from "../api";
import { LANG_NAMES, S } from "../i18n";
import { toast } from "../toast";
import {
  DangerConfirm,
  Dropdown,
  Loading,
  Pager,
  RAIL_CLS,
  SearchSelect,
} from "../ui";

const KB_ROLES = [
  { value: "viewer", label: S.kbset.roles.viewer },
  { value: "editor", label: S.kbset.roles.editor },
  { value: "admin", label: S.kbset.roles.admin },
];

/**
 * Which roles this KB can grant.
 *
 * **An open KB has no viewer to grant**: for an open KB `access::kb_role` hands Viewer
 * to everyone in the deployment, so writing a `role=viewer` row grants nothing extra --
 * a no-op record that still takes up a line in the member list and makes people think it
 * did something. The only meaning left for listing it here is "grants write access".
 *
 * Historical data may still hold viewer rows for open KBs, but those rows no longer show
 * in the list (see `listed`), so there is no need to guard here against "the current
 * value is not among the options".
 */
function rolesFor(isOpen: boolean) {
  return isOpen ? KB_ROLES.filter((r) => r.value !== "viewer") : KB_ROLES;
}

type Section = "general" | "members" | "activity" | "danger";

export function KbSettings() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  /* The KB id comes from the path. **It used to be `?kb=`** -- that was the only place
     carrying the KB around before this routing rework; now the whole area lives under
     /kb/$kbId, so it no longer has to be its own special case */
  const { kbId } = useParams({ from: "/app/kb/$kbId/settings" });

  // Failed job count and requeue (#216). The query key carries the KB id, and a requeue
  // invalidates it so it refetches
  const failedJobs = useQuery({
    queryKey: ["jobs", "failed", kbId],
    queryFn: () => api.failedJobs(kbId!),
    enabled: !!kbId,
  });
  const requeue = useMutation({
    mutationFn: () => api.requeueJobs(kbId!),
    onSuccess: (r) => {
      toast.success(S.kbset.requeued(r.requeued));
      queryClient.invalidateQueries({ queryKey: ["jobs", "failed", kbId] });
    },
    onError: (e) => toast.error(String(e)),
  });
  const kb = useQuery({
    queryKey: ["kbOne", kbId],
    queryFn: () => api.kbDetail(kbId!),
    enabled: !!kbId,
  });

  const [section, setSection] = useState<Section>("general");
  const [name, setName] = useState("");
  const [desc, setDesc] = useState("");
  const [visibility, setVisibility] = useState<"open" | "restricted">("open");
  const [autoExtend, setAutoExtend] = useState(true);
  // **Off by default**, the opposite of the one above: inference writes facts into the
  // ledger, and a declaration can be wrong
  const [materialize, setMaterialize] = useState(false);
  const [inferMins, setInferMins] = useState(60);
  const [ontoLang, setOntoLang] = useState<"en" | "zh">("en");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (kb.data) {
      setName(kb.data.name);
      setDesc(kb.data.description ?? "");
      setVisibility(kb.data.visibility);
      setAutoExtend(kb.data.auto_extend_ontology);
      setMaterialize(kb.data.materialize_inferences);
      setInferMins(kb.data.inference_interval_minutes);
      setOntoLang(kb.data.ontology_lang);
    }
  }, [kb.data]);

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["kbOne", kbId] });
    queryClient.invalidateQueries({ queryKey: ["kbs"] });
  };

  const save = useMutation({
    mutationFn: () =>
      api.updateKb(kbId!, {
        name: name.trim(),
        description: desc.trim() || null,
        visibility,
        auto_extend_ontology: autoExtend,
        materialize_inferences: materialize,
        inference_interval_minutes: inferMins,
        ontology_lang: ontoLang,
      }),
    onSuccess: () => {
      setError(null);
      invalidate();
    },
    onError: (e) => setError((e as Error).message),
  });

  const removeKb = useMutation({
    mutationFn: () => api.deleteKb(kbId!),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["kbs"] });
      navigate({ to: "/kb/$kbId/library", params: { kbId } });
    },
    onError: (e) => setError((e as Error).message),
  });

  if (!kbId || kb.isPending) return <Loading>{S.nav.loading}</Loading>;
  if (kb.isError)
    return (
      <div className="p-8 text-sm text-rose-400">
        {(kb.error as Error).message}
      </div>
    );

  const lbl = "block text-xs font-medium text-neutral-500 mb-1";
  const rail =
    "w-full flex items-center gap-2.5 rounded-lg px-3 py-2 text-[13px] text-left transition-colors";

  const isDefault = kb.data.is_default;
  const sections: {
    key: Section;
    label: string;
    Icon: typeof Settings2;
    danger?: boolean;
  }[] = [
    { key: "general", label: S.kbset.general, Icon: Settings2 },
    { key: "members", label: S.kbset.members, Icon: Users },
    { key: "activity", label: S.kbset.activity, Icon: HistoryIcon },
    // The default KB cannot be deleted: the danger section does not appear at all
    ...(isDefault
      ? []
      : [
          {
            key: "danger" as Section,
            label: S.kbset.danger,
            Icon: TriangleAlert,
            danger: true,
          },
        ]),
  ];

  return (
    <div className="h-full flex">
      {/* Section nav: future extraction settings / retention policy / tokens extend here */}
      <aside className={`${RAIL_CLS} p-3 space-y-0.5`}>
        {sections.map(({ key, label, Icon, danger }) => (
          <button
            key={key}
            onClick={() => setSection(key)}
            className={`${rail} ${
              section === key
                ? "u-nav-active"
                : danger
                  ? "text-neutral-500 hover:bg-white/[0.05] hover:text-[var(--u-danger)]"
                  : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200"
            }`}
          >
            <Icon size={14} />
            {label}
          </button>
        ))}
      </aside>

      <main className="flex-1 min-w-0 overflow-y-auto u-scroll px-8 py-6">
        <div className="max-w-xl space-y-5">
          {/* No KB name appended: the top-bar switcher already says which KB this is */}
          <h2 className="u-title text-lg">{S.kbset.title}</h2>

          {section === "general" && (
            <div className="glass rounded-xl p-4 space-y-3">
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <label className={lbl}>{S.settings.kbs.name}</label>
                  <input
                    className="input-dark w-full px-3 py-2 text-sm"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                  />
                </div>
                <div>
                  <label className={lbl}>{S.settings.kbs.visibility}</label>
                  {isDefault ? (
                    /* The default KB is locked open: the explanation stays permanently
                       visible (buried in a hover is the same as no explanation), with the
                       details in the full-width row below the grid */
                    <div className="flex items-center gap-1.5 rounded-lg border border-white/10 px-2.5 py-2 text-[11px] text-neutral-500 cursor-not-allowed">
                      <Lock size={11} className="shrink-0 text-neutral-600" />
                      {S.kbset.defaultOpenLabel}
                    </div>
                  ) : (
                    <div className="flex rounded-lg overflow-hidden border border-white/10">
                      {(
                        [
                          ["open", "Open"],
                          ["restricted", S.settings.kbs.visRestricted],
                        ] as const
                      ).map(([v, label]) => (
                        <button
                          key={v}
                          onClick={() => setVisibility(v)}
                          title={label}
                          className={`flex-1 px-2 py-2 text-[11px] truncate transition-colors ${
                            visibility === v
                              ? "bg-white/[0.12] text-white"
                              : "text-neutral-500 hover:bg-white/[0.05] hover:text-neutral-300"
                          }`}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              </div>
              {isDefault && (
                <p className="text-xs leading-relaxed text-neutral-500">
                  {S.kbset.defaultOpenNote}
                </p>
              )}
              <div>
                <label className={lbl}>{S.settings.kbs.description}</label>
                <input
                  className="input-dark w-full px-3 py-2 text-sm"
                  value={desc}
                  onChange={(e) => setDesc(e.target.value)}
                />
              </div>
              {/* Auto-extend the ontology: on by default, because nobody chose the ten
                  default relations a new KB comes with. The note has to make clear that
                  turning it off loses **only** the doing-it-for-you, not the noticing */}
              <label className="flex items-start gap-2.5 pt-1 cursor-pointer">
                <input
                  type="checkbox"
                  className="mt-0.5 accent-[var(--u-accent)]"
                  checked={autoExtend}
                  onChange={(e) => setAutoExtend(e.target.checked)}
                />
                <span className="min-w-0">
                  <span className="block text-sm text-neutral-200">
                    {S.kbset.autoExtend}
                  </span>
                  <span className="block text-xs leading-relaxed text-neutral-500">
                    {S.kbset.autoExtendNote}
                  </span>
                </span>
              </label>
              {/* Materialized inference: **off by default**, the opposite of the one
                  above. Auto-extending the ontology touches the vocabulary; this one
                  touches the ledger -- it writes facts into the graph by the axioms, and
                  a declaration can be wrong */}
              <label className="flex items-start gap-2.5 pt-1 cursor-pointer">
                <input
                  type="checkbox"
                  className="mt-0.5 accent-[var(--u-accent)]"
                  checked={materialize}
                  onChange={(e) => setMaterialize(e.target.checked)}
                />
                <span className="min-w-0">
                  <span className="block text-sm text-neutral-200">
                    {S.kbset.materialize}
                  </span>
                  <span className="block text-xs leading-relaxed text-neutral-500">
                    {S.kbset.materializeNote}
                  </span>
                </span>
              </label>
              {/* Re-inference interval. **Only shown while it is switched on** -- with
                  it off it affects nothing, and sitting there it only makes people think
                  that setting it will make inference run */}
              {materialize && (
                <div className="pl-6 flex items-center gap-2">
                  <label className="text-xs text-neutral-500">
                    {S.kbset.inferEvery}
                  </label>
                  <input
                    type="number"
                    min={5}
                    max={10080}
                    className="input-dark w-24 px-2 py-1 text-xs u-num"
                    value={inferMins}
                    onChange={(e) => setInferMins(Number(e.target.value))}
                  />
                  <span className="text-xs text-neutral-500">
                    {S.kbset.minutes}
                  </span>
                  {kb.data.last_inference_at && (
                    <span className="text-[11px] text-neutral-600">
                      {S.kbset.lastInference(
                        new Date(kb.data.last_inference_at).toLocaleString(),
                      )}
                    </span>
                  )}
                </div>
              )}
              {/* Failed jobs (#216): only shown when there are any. "Run it again" puts
                  every failed job in this KB back on the queue */}
              {failedJobs.data && failedJobs.data.failed > 0 && (
                <div className="flex items-center gap-2">
                  <span className="text-xs text-neutral-400">
                    {S.kbset.failedJobs(failedJobs.data.failed)}
                  </span>
                  <button
                    className="u-btn u-btn-ghost px-2.5 py-1 text-xs"
                    disabled={requeue.isPending}
                    onClick={() => requeue.mutate()}
                  >
                    {S.kbset.requeue}
                  </button>
                </div>
              )}
              {/* Corpus language. **Not the interface language** -- class descriptions
                  go verbatim into the extraction prompt, and the reader is the model
                  reading these documents, so it follows the documents, not the reader */}
              <div className="pt-1">
                <span className="block text-sm text-neutral-200">
                  {S.kbset.ontologyLang}
                </span>
                <span className="mt-0.5 block text-xs leading-relaxed text-neutral-500">
                  {S.kbset.ontologyLangNote}
                </span>
                <div className="mt-2 flex gap-1 rounded-lg bg-white/5 p-1 w-fit">
                  {(["en", "zh"] as const).map((l) => (
                    <button
                      key={l}
                      onClick={() => setOntoLang(l)}
                      className={`rounded-md px-3 py-1 text-[12px] font-medium transition-colors ${
                        ontoLang === l
                          ? "bg-white/10 text-neutral-100"
                          : "text-neutral-500 hover:text-neutral-300"
                      }`}
                    >
                      {LANG_NAMES[l]}
                    </button>
                  ))}
                </div>
              </div>
              <div className="flex items-center gap-3">
                <button
                  className="u-btn u-btn-primary px-3.5 py-1.5 text-xs"
                  disabled={!name.trim() || save.isPending}
                  onClick={() => save.mutate()}
                >
                  {S.kbset.save}
                </button>
                {save.isSuccess && (
                  <span className="text-xs text-neutral-400">
                    {S.kbset.saved}
                  </span>
                )}
              </div>
            </div>
          )}

          {section === "members" && (
            <KbMembers kbId={kbId} isOpen={kb.data.visibility === "open"} />
          )}

          {section === "activity" && <KbActivity kbId={kbId} />}

          {section === "danger" && (
            <div className="glass rounded-2xl px-5 py-4 flex items-center justify-between gap-4">
              <div className="min-w-0">
                <div className="text-sm font-medium text-neutral-200">
                  {S.kbset.deleteRowTitle}
                </div>
                <div className="mt-0.5 text-xs text-neutral-500">
                  {S.kbset.deleteRowHint}
                </div>
              </div>
              <button
                className="u-btn px-3.5 py-1.5 text-xs font-semibold shrink-0"
                style={{
                  background: "var(--u-danger-solid)",
                  color: "#ffffff",
                }}
                onClick={() => setConfirmingDelete(true)}
              >
                {S.kbset.deleteRowBtn}
              </button>
            </div>
          )}

          {error && <p className="text-sm text-rose-400">{error}</p>}

          {confirmingDelete && (
            <DangerConfirm
              title={S.kbset.deleteKb}
              hint={S.kbset.deleteHint(kb.data.name)}
              requireText={kb.data.name}
              confirmLabel={S.kbset.deleteBtn}
              cancelLabel={S.library.cancel}
              busy={removeKb.isPending}
              onConfirm={() => removeKb.mutate()}
              onCancel={() => setConfirmingDelete(false)}
            />
          )}
        </div>
      </main>
    </div>
  );
}

/** Pick a human-readable name out of detail (semantics differ per action, so fall back key by key) */
function auditDetailName(e: AuditEvent): string {
  const d = e.detail;
  const cand = [d.label, d.name, d.filename, d.key, d.role];
  const hit = cand.find((v) => typeof v === "string" && v);
  return typeof hit === "string" ? hit : "";
}

const AUDIT_PAGE = 50;

function KbActivity({ kbId }: { kbId: string }) {
  // The filters follow how people actually look things up: one kind of action, one
  // person, one stretch of time. Actions match by prefix -- `entity.` alone scoops up the
  // whole retyped / renamed family
  const [action, setAction] = useState("");
  const [since, setSince] = useState("");
  const [until, setUntil] = useState("");
  const [page, setPage] = useState(0);
  const audit = useQuery({
    queryKey: ["kbAudit", kbId, action, since, until, page],
    queryFn: () =>
      api.kbAudit(kbId, {
        action: action || undefined,
        since: since || undefined,
        until: until || undefined,
        limit: AUDIT_PAGE,
        offset: page * AUDIT_PAGE,
      }),
    placeholderData: (prev) => prev,
  });
  const events = audit.data?.events ?? [];
  const total = audit.data?.total ?? 0;
  // The dropdown is filled from the actions that actually happened in this KB, not from
  // a hardcoded list
  const actions = audit.data?.actions ?? [];
  const filtered = !!(action || since || until);

  const reset = (fn: () => void) => {
    fn();
    setPage(0);
  };

  return (
    <div className="glass rounded-xl p-4">
      <p className="text-xs text-neutral-500 mb-3">{S.kbset.activityHint}</p>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <select
          className="input-dark px-2 py-1 text-xs"
          value={action}
          onChange={(e) => reset(() => setAction(e.target.value))}
        >
          <option value="">{S.kbset.auditAllActions}</option>
          {actions.map((a) => (
            <option key={a} value={a}>
              {a}
            </option>
          ))}
        </select>
        <input
          type="date"
          className="input-dark px-2 py-1 text-xs u-num"
          value={since}
          title={S.kbset.auditSince}
          onChange={(e) => reset(() => setSince(e.target.value))}
        />
        <span className="text-xs text-neutral-600">→</span>
        <input
          type="date"
          className="input-dark px-2 py-1 text-xs u-num"
          value={until}
          title={S.kbset.auditUntil}
          onChange={(e) => reset(() => setUntil(e.target.value))}
        />
        {filtered && (
          <button
            className="u-btn u-btn-ghost px-2 py-1 text-xs"
            onClick={() =>
              reset(() => {
                setAction("");
                setSince("");
                setUntil("");
              })
            }
          >
            {S.kbset.auditClear}
          </button>
        )}
        <span className="ml-auto u-num text-[11px] text-neutral-500">
          {S.kbset.auditTotal(total)}
        </span>
      </div>
      {audit.isPending ? (
        <p className="text-xs text-neutral-600">{S.nav.loading}</p>
      ) : events.length === 0 ? (
        <p className="text-xs text-neutral-600">{S.kbset.activityEmpty}</p>
      ) : (
        <div className="space-y-0.5">
          {events.map((e) => (
            <div
              key={e.id}
              className="flex items-baseline gap-3 py-1.5 text-[13px]"
            >
              <span className="u-num shrink-0 text-[11px] text-neutral-600">
                {e.created_at.slice(0, 16).replace("T", " ")}
              </span>
              <span className="min-w-0 truncate">
                <span className="text-neutral-200">
                  {e.actor_name ??
                    (e.actor_id
                      ? S.kbset.deletedUser
                      : e.action.startsWith("review.")
                        ? S.kbset.adjudicator
                        : S.kbset.engine)}
                </span>{" "}
                <span className="text-neutral-500">
                  {S.kbset.auditActions[e.action] ?? e.action}
                </span>
                {auditDetailName(e) && (
                  <span className="text-neutral-300">
                    {" "}
                    “{auditDetailName(e)}”
                  </span>
                )}
              </span>
            </div>
          ))}
        </div>
      )}
      <Pager total={total} pageSize={AUDIT_PAGE} page={page} onPage={setPage} />
    </div>
  );
}

function KbMembers({ kbId, isOpen }: { kbId: string; isOpen: boolean }) {
  const queryClient = useQueryClient();
  const members = useQuery({
    queryKey: ["kbMembers", kbId],
    queryFn: () => api.kbMembers(kbId),
  });
  const orgUsers = useQuery({ queryKey: ["orgUsers"], queryFn: api.orgUsers });
  const [addUserId, setAddUserId] = useState("");
  // An open KB does not even have the viewer option, so the default has to follow suit
  const [addRole, setAddRole] = useState(isOpen ? "editor" : "viewer");

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["kbMembers", kbId] });

  const setMember = useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: string }) =>
      api.setKbMember(kbId, userId, role),
    onSuccess: () => {
      setAddUserId("");
      invalidate();
    },
  });
  const remove = useMutation({
    mutationFn: (userId: string) => api.removeKbMember(kbId, userId),
    onSuccess: invalidate,
  });

  // In an open KB a `role=viewer` row is **equivalent to not having the row at all**:
  // everyone has read access anyway, so that record grants nothing. Which is why the list
  // only keeps the people who genuinely hold write access.
  //
  // **Leaving them out of memberIds is the matching other half**, and it cannot be
  // skipped: kept in there, that person vanishes from the add picker, and so can never be
  // granted editor again -- a record that was supposed to mean nothing would instead lock
  // the person out
  const listed = (members.data?.members ?? []).filter(
    (m) => !isOpen || m.role !== "viewer",
  );
  const memberIds = new Set(listed.map((m) => m.user_id));
  const addable = orgUsers.data?.filter((u) => !memberIds.has(u.id)) ?? [];

  if (members.isError) return null;

  return (
    <div className="glass rounded-xl p-4">
      <p className="text-xs text-neutral-500 mb-3">
        {isOpen ? S.kbset.membersHintOpen : S.kbset.membersHintRestricted}
      </p>

      {members.data && listed.length === 0 && (
        <p className="text-xs text-neutral-600 mb-3">
          {isOpen ? S.kbset.noWriters : S.kbset.noMembers}
        </p>
      )}
      {listed.map((m) => (
        <div key={m.user_id} className="flex items-center gap-3 py-1.5">
          <div className="min-w-0 flex-1">
            <span className="text-sm text-neutral-200">{m.display_name}</span>
            <span className="ml-2 text-xs text-neutral-500">{m.email}</span>
          </div>
          <Dropdown
            size="sm"
            className="w-24"
            value={m.role}
            onChange={(role) => setMember.mutate({ userId: m.user_id, role })}
            options={rolesFor(isOpen)}
          />
          <button
            onClick={() => remove.mutate(m.user_id)}
            className="text-xs text-neutral-500 hover:text-rose-400"
          >
            {S.kbset.remove}
          </button>
        </div>
      ))}

      {/* **The picker is always there**, not shown or hidden by "is there anyone left to
          add". A control that comes and goes is more confusing than one sitting there
          empty -- the first reaction to something missing is that the feature broke, not
          that "there is nobody to add". The empty list is SearchSelect's own business to
          state (it has a noMatches empty state), so no extra sentence is needed here */}
      <div className="mt-3 flex gap-2 items-center border-t border-white/5 pt-3">
        <SearchSelect
          className="flex-1"
          value={addUserId}
          onChange={setAddUserId}
          placeholder={S.kbset.addMember}
          options={addable.map((u) => ({
            value: u.id,
            label: u.display_name,
            hint: u.email,
          }))}
        />
        <Dropdown
          className="w-24"
          value={addRole}
          onChange={setAddRole}
          options={rolesFor(isOpen)}
        />
        <button
          className="u-btn u-btn-primary px-3 py-1.5 text-xs"
          disabled={!addUserId || setMember.isPending}
          onClick={() => setMember.mutate({ userId: addUserId, role: addRole })}
        >
          {S.members.add}
        </button>
      </div>
    </div>
  );
}
