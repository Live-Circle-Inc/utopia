import {
  createRootRoute,
  createRoute,
  createRouter,
  redirect,
} from "@tanstack/react-router";
import { Account } from "./pages/Account";
import { AccountShell } from "./pages/AccountShell";
import { Tokens } from "./pages/Tokens";
import { Chat } from "./pages/Chat";
import { DocViewer } from "./pages/DocViewer";
import { DocsPage } from "./pages/Docs";
import { Graph } from "./pages/Graph";
import { Library } from "./pages/Library";
import { Login } from "./pages/Login";
import { Privacy, Terms } from "./pages/Legal";
import { KbRedirect, KbScope } from "./pages/KbScope";
import { KbSettings } from "./pages/KbSettings";
import { MyKbs } from "./pages/MyKbs";
import { NotFound } from "./pages/ServerDown";
import { Ontology } from "./pages/Ontology";
import { Mappings } from "./pages/Mappings";
import { Review } from "./pages/Review";
import { Search } from "./pages/Search";
import { Settings } from "./pages/Settings";
import { Shell } from "./pages/Shell";

const rootRoute = createRootRoute();

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/login",
  component: Login,
});

// Public legal pages: reachable before login
const privacyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/privacy",
  component: Privacy,
});

const termsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/terms",
  component: Terms,
});

const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "app",
  component: Shell,
});

const indexRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/",
  beforeLoad: () => {
    // Home = the graph: the product's differentiating front door
    throw redirect({ to: "/graph" });
  },
});

/* Knowledge-base scope. **A KB is a container, not a filter** -- every page below belongs to
   one particular KB, the path expresses that containment, and so the router catches the whole
   "forgot to pass the KB" class of mistake for us: `/kb/$kbId/search` simply cannot be
   constructed without an id. Full reasoning in pages/KbScope.tsx */
const kbRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/kb/$kbId",
  component: KbScope,
});

const chatRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "chat",
  component: Chat,
});

// The conversation is the route: /chat/$conversationId only carries the URL (refresh/share
// lands back in the same conversation); rendering stays with the parent Chat -- the parent
// stays mounted across /chat ↔ /chat/$id, so the stream is never cut
const chatConversationRoute = createRoute({
  getParentRoute: () => chatRoute,
  path: "$conversationId",
  component: () => null,
});

const searchRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "search",
  component: Search,
  // The query is "what you are looking at", not "how you are looking at it" -- refresh, back
  // and share all rebuild from it (same as doc's chunk, review's queue). Pagination stays
  // local, like the graph's detail level
  validateSearch: (search: Record<string, unknown>): { q?: string } => ({
    q: typeof search.q === "string" ? search.q : undefined,
  }),
});

const graphRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "graph",
  /* The graph page's shareable state. **All three are "what you are looking at", not "how you
     are looking at it"** -- which is why the detail level (how many to draw) deliberately stays
     out of the URL: that is a local matter of taste, and it should not follow you to another
     machine.

     - entity: who is selected
     - focus: whether we are inside some entity's neighbourhood (a different picture from
       "selected within the whole graph")
     - at: which moment the timeline is parked on. **This is the one we can least afford to
       lose** -- this product's selling point is "see the world at a given moment", and a link
       without the moment throws away the most interesting part of it */
  validateSearch: (
    search: Record<string, unknown>,
  ): { entity?: string; focus?: string; at?: string } => ({
    entity: typeof search.entity === "string" ? search.entity : undefined,
    focus: typeof search.focus === "string" ? search.focus : undefined,
    at: typeof search.at === "string" ? search.at : undefined,
  }),
  component: Graph,
});

const docRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "doc/$docId",
  validateSearch: (search: Record<string, unknown>): { chunk?: string } => ({
    chunk: typeof search.chunk === "string" ? search.chunk : undefined,
  }),
  component: DocViewer,
});

const libraryRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "library",
  validateSearch: (search: Record<string, unknown>): { src?: string } => ({
    src: typeof search.src === "string" ? search.src : undefined,
  }),
  component: Library,
});

const ontologyRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "ontology",
  component: Ontology,
});

const mappingsRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "mappings",
  component: Mappings,
});

const reviewRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "review",
  // Arrived from the dispute chip on the entity panel: land in the matching queue and light
  // up that one card (0017 §3)
  validateSearch: (
    search: Record<string, unknown>,
  ): { queue?: string; item?: string } => ({
    queue: typeof search.queue === "string" ? search.queue : undefined,
    item: typeof search.item === "string" ? search.item : undefined,
  }),
  component: Review,
});

// Built-in docs: public route (readable before login; works offline in a private deployment)
const docsIndexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/docs",
  beforeLoad: () => {
    throw redirect({ to: "/docs/$slug", params: { slug: "ingest" } });
  },
});

const docsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/docs/$slug",
  component: DocsPage,
});

// Account layer (Profile / Administration): nothing to do with a KB, so it gets its own shell
// with no tab nav
const accountShellRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "account",
  component: AccountShell,
});

const accountRoute = createRoute({
  getParentRoute: () => accountShellRoute,
  path: "/account",
  component: Account,
});

const myKbsRoute = createRoute({
  getParentRoute: () => accountShellRoute,
  path: "/account/kbs",
  component: MyKbs,
});

// Personal tokens (0014): the key you hand an agent belongs to a person, so it lives in the
// account layer, not inside a KB
const tokensRoute = createRoute({
  getParentRoute: () => accountShellRoute,
  path: "/account/tokens",
  component: Tokens,
});

const adminRoute = createRoute({
  getParentRoute: () => accountShellRoute,
  path: "/admin",
  // Deep-link a particular tab (e.g. "register a new connection" in a KB's data section goes
  // straight to Data sources)
  validateSearch: (
    search: Record<string, unknown>,
  ): { tab?: "models" | "members" | "kbs" | "datasources" | "deployment" } => ({
    tab:
      search.tab === "models" ||
      search.tab === "members" ||
      search.tab === "kbs" ||
      search.tab === "datasources" ||
      search.tab === "deployment"
        ? search.tab
        : undefined,
  }),
  component: Settings,
});

// Legacy path compatibility: /settings → /admin
const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings",
  beforeLoad: () => {
    throw redirect({ to: "/admin" });
  },
});

const kbSettingsRoute = createRoute({
  getParentRoute: () => kbRoute,
  path: "settings",
  component: KbSettings,
});

/* Legacy path compatibility: KB-less addresses like `/graph` still work -- we resolve which
   KB they should go to and then redirect. **Not done as a beforeLoad redirect** -- at that
   point the KB list has not been fetched yet, and localStorage may hold nothing at all (new
   device, cleared cache), so we have to wait for useKb to resolve it */
/* **Written out one by one, no factory function**: inside a factory `path` is a `string`, the
   type system no longer sees the literal, and `redirect({ to: "/graph" })` elsewhere stops
   type-checking. Verbosity traded for type safety */
const legacyGraphRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/graph",
  component: () => <KbRedirect page="graph" />,
});
const legacySearchRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/search",
  component: () => <KbRedirect page="search" />,
});
const legacyChatRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/chat",
  component: () => <KbRedirect page="chat" />,
});
const legacyLibraryRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/library",
  component: () => <KbRedirect page="library" />,
});
const legacyOntologyRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/ontology",
  component: () => <KbRedirect page="ontology" />,
});
const legacyMappingsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/mappings",
  component: () => <KbRedirect page="mappings" />,
});
const legacyReviewRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/review",
  component: () => <KbRedirect page="review" />,
});
const legacyKbSettingsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: "/kb-settings",
  component: () => <KbRedirect page="settings" />,
});

const routeTree = rootRoute.addChildren([
  loginRoute,
  privacyRoute,
  termsRoute,
  settingsRoute,
  docsIndexRoute,
  docsRoute,
  accountShellRoute.addChildren([accountRoute, myKbsRoute, tokensRoute, adminRoute]),
  appRoute.addChildren([
    indexRoute,
    legacyGraphRoute,
    legacySearchRoute,
    legacyChatRoute,
    legacyLibraryRoute,
    legacyOntologyRoute,
    legacyMappingsRoute,
    legacyReviewRoute,
    legacyKbSettingsRoute,
    kbRoute.addChildren([
      chatRoute.addChildren([chatConversationRoute]),
      searchRoute,
      graphRoute,
      docRoute,
      libraryRoute,
      reviewRoute,
      ontologyRoute,
      mappingsRoute,
      kbSettingsRoute,
    ]),
  ]),
]);

export const router = createRouter({
  routeTree,
  // The lost city: the penalty page for unknown paths
  defaultNotFoundComponent: NotFound,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
