/* The sources rail ("a source is a folder"): the left nav shared by Library and DocViewer. */
import type { SourceKind } from "../sourceKinds";
import { useQuery } from "@tanstack/react-query";
import {
  Archive,
  BookOpen,
  Brain,
  Briefcase,
  CircleDot,
  Building2,
  Cloud,
  Database,
  FileText,
  FlaskConical,
  FolderOpen,
  Globe,
  HardDrive,
  Inbox,
  Library as LibraryIcon,
  Newspaper,
  Notebook,
  Plus,
  Puzzle,
  Rocket,
  Rss,
  Server,
  SquareKanban,
  Upload,
  Users,
  Webhook,
  type LucideIcon,
} from "lucide-react";
import { api, type SourceView } from "../api";
import { S } from "../i18n";
import { RAIL_CLS } from "../ui";

/** Left-rail selection: all / manual uploads / the id of some source */
export type LibrarySelection = "all" | "uploads" | string;

/** The source icons available to pick from (lucide icon name → component). */
export const SOURCE_ICONS: Record<string, LucideIcon> = {
  "folder-open": FolderOpen,
  globe: Globe,
  rss: Rss,
  webhook: Webhook,
  "book-open": BookOpen,
  newspaper: Newspaper,
  "file-text": FileText,
  database: Database,
  cloud: Cloud,
  "hard-drive": HardDrive,
  inbox: Inbox,
  archive: Archive,
  briefcase: Briefcase,
  "building-2": Building2,
  "flask-conical": FlaskConical,
  notebook: Notebook,
  rocket: Rocket,
  server: Server,
  users: Users,
};

// keyed exhaustively by SourceKind: add a source kind without an icon and tsc goes red
export const KIND_ICON: Record<SourceKind, LucideIcon> = {
  folder: FolderOpen,
  url: Globe,
  rss: Rss,
  api: Webhook,
  custom: Puzzle,
  github_issues: CircleDot,
  jira_issues: SquareKanban,
  s3: HardDrive,
  azure_blob: Cloud,
  gcs: Cloud,
  webdav: FolderOpen,
  notion: Notebook,
  memory: Brain,
  upload: Upload,
};

/** Source kinds that have fetch/sync semantics (folder/api have no notion of syncing) */
export const SYNCING_KINDS = new Set([
  "url",
  "rss",
  "custom",
  "github_issues",
  "jira_issues",
  "s3",
  "azure_blob",
  "gcs",
  "webdav",
  "notion",
]);

export const SYNC_DOT: Record<SourceView["last_sync_status"], string> = {
  never: "bg-neutral-600",
  queued: "bg-[var(--u-warn)]",
  running: "bg-[var(--u-warn)] animate-pulse",
  ok: "bg-[var(--u-ok)]",
  failed: "bg-[var(--u-danger)]",
};

export function sourceIcon(s: SourceView): LucideIcon {
  // built-in kinds have fixed icons; only custom respects the user's own icon choice
  if (s.kind === "custom" && s.icon && SOURCE_ICONS[s.icon]) return SOURCE_ICONS[s.icon];
  return KIND_ICON[s.kind] || Globe;
}

function RailItem({
  active,
  onClick,
  icon,
  label,
  count,
  dot,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  count: number;
  dot?: string;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13px] transition-colors ${
        active ? "u-nav-active" : "text-neutral-400 hover:bg-white/[0.05] hover:text-neutral-200"
      }`}
    >
      <span className="shrink-0 text-neutral-500">{icon}</span>
      <span className="truncate">{label}</span>
      {dot && <span className={`h-1.5 w-1.5 rounded-full shrink-0 ${dot}`} />}
      <span className="ml-auto shrink-0 u-num text-[10.5px] text-neutral-600">{count}</span>
    </button>
  );
}

export function SourcesRail({
  kbId,
  active,
  onSelect,
  onAdd,
}: {
  kbId: string;
  active: LibrarySelection | null;
  onSelect: (sel: LibrarySelection) => void;
  /** When omitted, the "+" is hidden (as on the document viewer page) */
  onAdd?: () => void;
}) {
  // the rail needs only two numbers: how many documents in the whole base, and how many with no
  // source. **Each asks for one page and zero rows** -- the totals come back with the response,
  // so there is no need to pull the documents down and count them
  const docs = useQuery({
    queryKey: ["docCount", kbId],
    queryFn: () => api.documents(kbId, { limit: 1, offset: 0 }),
  });
  const uploads = useQuery({
    queryKey: ["docCount", kbId, "uploads"],
    queryFn: () => api.documents(kbId, { source: "none", limit: 1, offset: 0 }),
  });
  const sources = useQuery({
    queryKey: ["sources", kbId],
    queryFn: () => api.sources(kbId),
  });

  const sourceList = sources.data?.sources ?? [];
  const uploadsCount = uploads.data?.total ?? 0;

  return (
    <aside className={`${RAIL_CLS} flex flex-col`}>
      {/* All documents is pinned at the top as a first-class entry; the SOURCES section
          (including its +) sits below it */}
      <div className="px-2 pt-3">
        <RailItem
          active={active === "all"}
          onClick={() => onSelect("all")}
          icon={<LibraryIcon size={14} />}
          label={S.library.allDocs}
          count={docs.data?.total ?? 0}
        />
      </div>
      <div className="flex items-center justify-between px-4 pt-3 pb-1.5">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-500">
          {S.library.sources}
        </span>
        {onAdd && (
          <button
            onClick={onAdd}
            title={S.library.addSource}
            className="text-neutral-500 hover:text-neutral-200"
          >
            <Plus size={14} />
          </button>
        )}
      </div>
      <div className="u-scroll flex-1 overflow-y-auto px-2 pb-3 space-y-0.5">
        {/* Uploads: the permanent default source (where uploads land by default, undeletable) */}
        <RailItem
          active={active === "uploads"}
          onClick={() => onSelect("uploads")}
          icon={<Upload size={14} />}
          label={S.library.uploads}
          count={uploadsCount}
        />
        {sourceList.map((s) => {
          const Icon = sourceIcon(s);
          return (
            <RailItem
              key={s.id}
              active={active === s.id}
              onClick={() => onSelect(s.id)}
              icon={<Icon size={14} />}
              label={s.name}
              count={s.doc_count}
              dot={
                SYNCING_KINDS.has(s.kind) || s.kind === "api"
                  ? SYNC_DOT[s.last_sync_status]
                  : undefined
              }
            />
          );
        })}
      </div>
    </aside>
  );
}
