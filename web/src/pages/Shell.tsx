import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  Link,
  Outlet,
  useNavigate,
  useRouterState,
} from "@tanstack/react-router";
import {
  BookMarked,
  Database,
  Library as LibraryIcon,
  ListChecks,
  MessagesSquare,
  Search as SearchIcon,
  Settings as SettingsIcon,
  Shapes,
  Waypoints,
} from "lucide-react";
import { api, ApiError } from "../api";
import { S } from "../i18n";
import { useKb, useKbId } from "../kb";
import { Dropdown, GithubMark, Wordmark } from "../ui";
import { AlertBell } from "./AlertBell";
import { UserMenu } from "./UserMenu";
import { ServerDown } from "./ServerDown";
import { useAlertEvents } from "../useAlertEvents";
import { useKbEvents } from "../useKbEvents";
import { usePageTitle } from "../useTitle";

const TABS = [
  // The graph is the shop window, so it comes first; the two ways of querying (Search/Ask) follow
  { to: "/kb/$kbId/graph", label: S.nav.graph, Icon: Waypoints },
  { to: "/kb/$kbId/search", label: S.nav.search, Icon: SearchIcon },
  { to: "/kb/$kbId/chat", label: S.nav.ask, Icon: MessagesSquare },
  { to: "/kb/$kbId/library", label: S.nav.library, Icon: LibraryIcon },
  { to: "/kb/$kbId/review", label: S.review.title, Icon: ListChecks },
  { to: "/kb/$kbId/ontology", label: S.ontology.title, Icon: Shapes },
  // The ontology says "what there is in the world", data mappings say "how this number is
  // worked out in the database" -- so they go next to each other
  { to: "/kb/$kbId/mappings", label: S.mapping.title, Icon: Database },
  // KB settings is scoped to "the current knowledge base" just like the other tabs, so it sits
  // alongside them in the content navigation
  { to: "/kb/$kbId/settings", label: S.nav.settings, Icon: SettingsIcon },
] as const;

export function Shell() {
  const navigate = useNavigate();
  const kbId = useKbId();

  const me = useQuery({ queryKey: ["me"], queryFn: api.me });
  const health = useQuery({
    queryKey: ["health"],
    queryFn: api.health,
    staleTime: Infinity,
  });
  const { kb, kbs, setKb } = useKb();
  // The title follows the current tab: `Graph · Utopia`; the document viewer counts as Library
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const tabLabel =
    TABS.find((t) => pathname.startsWith(t.to))?.label ??
    (pathname.startsWith("/doc/") ? S.nav.library : undefined);
  usePageTitle(S.app.name, tabLabel);
  // The one global KB event stream connection: document / review status refresh live (in place
  // of polling)
  useKbEvents(kb?.id);
  // The alert stream is global: the badge spans KBs, and system-level alerts have no KB at all
  useAlertEvents();

  // Not logged in goes to the login page. **Side effects belong in an effect**; the reason is in
  // the 401 branch below
  const unauthorized =
    me.isError && me.error instanceof ApiError && me.error.status === 401;
  useEffect(() => {
    if (unauthorized) navigate({ to: "/login" });
  }, [unauthorized, navigate]);

  if (me.isPending) {
    return (
      <div className="min-h-screen flex items-center justify-center text-neutral-500 text-sm">
        {S.nav.loading}
      </div>
    );
  }

  if (me.isError) {
    // **The redirect is done in an effect, not during render.** Calling `navigate` during
    // render means changing the router's state in the middle of somebody else's render, and
    // React keeps a permanent "Cannot update a component while rendering a different component"
    // warning up. It does not break today, but it has the smell of "depends on render order" --
    // and that is the kind of place most likely to turn into a real bug when the layout changes
    if (me.error instanceof ApiError && me.error.status === 401) {
      return null;
    }
    return <ServerDown />;
  }

  return (
    <div className="h-screen flex flex-col overflow-hidden u-arrive">
      {/* Top bar: brand + workspace + user (Vercel style) */}
      {/* z-40: backdrop-filter makes the top bar and the tab strip each their own stacking
          context, and without raising this the latter covers the popover panels inside the top
          bar by DOM order */}
      <header className="glass-strong relative z-40 border-x-0 border-t-0 h-14 shrink-0 flex items-center gap-4 px-5">
        {/* Wordmark: letters fade in one by one, ↗ floats out on hover, a click goes to the
            website */}
        <Wordmark className="text-[17px]" />
        {/* The one and only breadcrumb level: the knowledge base. Workspace has been folded
            from a concept down to an invisible deployment-level pipe (settings/members still go
            through it for the API, the way organizations do for a single tenant). */}
        <span className="text-neutral-700">/</span>
        {/* A pure switcher: creating a KB is an admin action, whose entry point is in System
            settings › Knowledge bases */}
        <Dropdown
          className="w-40"
          size="sm"
          icon={<BookMarked size={12} />}
          menuLabel={S.nav.kbLabel}
          value={kb?.id ?? ""}
          onChange={setKb}
          options={kbs.map((k) => ({ value: k.id, label: k.name }))}
        />
        {/* Three groups: project links / alerts / identity. **gap-3 between groups, gap-1.5
            within a group** -- spacing is expressed by structure, not by patching one element
            with a one-off ml.
            The user menu used to carry an ml-1.5 (set back when it sat right up against the
            GitHub pill), and once the bell was inserted between the two it became 6px on the
            left and 12px on the right */}
        <div className="ml-auto flex items-center gap-3">
          {/* Project links: Docs + the [GitHub·version] pill (the version comes from the
              backend health endpoint, so it matches the deployment).
              The version is folded into the GitHub pill: two elements of equal height, visually
              balanced.
              These two are a pair, so they sit closer together than the gap between groups */}
          <div className="flex items-center gap-1.5">
            <Link
              to="/docs"
              className="px-2 py-1 rounded-lg text-[12.5px] text-neutral-500 hover:text-neutral-200 hover:bg-white/[0.05] transition-colors"
            >
              {S.nav.docs}
            </Link>
            <a
              href={S.login.githubUrl}
              target="_blank"
              rel="noreferrer"
              title="GitHub"
              className="flex items-center gap-1.5 rounded-full border border-white/10 px-2.5 py-1 text-neutral-500 hover:text-neutral-200 hover:border-white/25 transition-colors"
            >
              <GithubMark size={13} />
              {health.data && (
                <span className="u-num text-[11px]">
                  v{health.data.version}
                </span>
              )}
            </a>
          </div>
          {/* Alert badge: the unread count across KBs. Failures used to stay only in the logs
              and in jobs.last_error, and not one document in the interface would change colour
              (0005) */}
          <AlertBell />
          {/* User menu: profile / system administration (admins only) / log out */}
          <UserMenu user={me.data} />
        </div>
      </header>

      {/* Tab navigation strip: icon + text, underline on the active state (Vercel style) */}
      <nav className="glass-strong border-x-0 border-t-0 shrink-0 flex items-stretch gap-1 px-4">
        {TABS.map(({ to, label, Icon }) => (
          <Link
            key={to}
            to={to}
            params={{ kbId }}
            className="flex items-center gap-2 px-3.5 py-2.5 text-[13.5px] font-medium text-neutral-400 border-b-2 border-transparent hover:text-neutral-200"
            activeProps={{
              className:
                "flex items-center gap-2 px-3.5 py-2.5 text-[13.5px] font-medium text-white border-b-2 border-white",
            }}
          >
            <Icon size={15} strokeWidth={1.8} />
            {label}
          </Link>
        ))}
      </nav>

      <main className="flex-1 min-h-0">
        <Outlet />
      </main>
    </div>
  );
}
