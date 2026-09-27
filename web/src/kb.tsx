// Current workspace / knowledge base context: both are switchable and remembered in
// localStorage; when a workspace has no KB, "General" is created automatically.
import { useCallback, useSyncExternalStore } from "react";
import { useLocation, useNavigate, useParams } from "@tanstack/react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Kb, type Workspace } from "./api";
import { kbStore, wsStore } from "./wsStore";

/** The knowledge base id in the current path. **Every page sits under /kb/$kbId, so take it
 *  straight from the path** -- no need to wait for the KB list to load; whoever the link names
 *  is who it is. When outside that scope (the account page and so on), fall back to the
 *  remembered one. */
export function useKbId(): string {
  const params = useParams({ strict: false }) as { kbId?: string };
  const { kb } = useKb();
  return params.kbId ?? kb?.id ?? "";
}

/** Switching KB lands on the same page, but **does not take what was on the page with it**.
 *
 *  It used to replace the kbId in the path wholesale, so `/kb/A/chat/some-conversation` became
 *  `/kb/B/chat/some-conversation` -- the conversation belongs to A, the page takes it and asks B,
 *  gets a 404, and falls back to a new conversation. The single-slot liveAnswer used to cover
 *  this step up (it claimed without asking whose conversation it was); once keyed by KB (#259)
 *  the claim correctly failed, and the 404 came into the open (#261).
 *
 *  So keep only the first segment after kbId: chat, graph, library... the conversation id and
 *  document id one level deeper are things inside that KB, and switching KB should throw them
 *  away. The document page is itself one particular document, so after a switch it lands on the
 *  new KB's Library. Outside the /kb scope, keep the original behaviour. */
export function samePageInKb(pathname: string, fromKbId: string, toKbId: string): string {
  const prefix = `/kb/${fromKbId}`;
  if (!pathname.startsWith(prefix)) return pathname.replace(fromKbId, toKbId);
  const section = pathname.slice(prefix.length).split("/").filter(Boolean)[0] ?? "graph";
  return `/kb/${toKbId}/${section === "doc" ? "library" : section}`;
}

export function useKb(): {
  kb: Kb | null;
  kbs: Kb[];
  workspace: Workspace | null;
  workspaces: Workspace[];
  setWorkspace: (id: string) => void;
  setKb: (id: string) => void;
} {
  const queryClient = useQueryClient();
  const selectedId = useSyncExternalStore(wsStore.subscribe, wsStore.get);
  const selectedKbId = useSyncExternalStore(kbStore.subscribe, kbStore.get);

  const workspaces = useQuery({ queryKey: ["workspaces"], queryFn: api.workspaces });
  const list = workspaces.data ?? [];
  const ws = list.find((w) => w.id === selectedId) ?? list[0] ?? null;

  const kbs = useQuery({
    queryKey: ["kbs", ws?.id],
    queryFn: async () => {
      const existing = await api.kbs(ws!.id);
      if (existing.length > 0) return existing;
      // An empty workspace creates General automatically -- creating a KB is an admin action
      // now, so a non-admin gets a 403: wait silently for an admin to create it (in practice
      // the first user is the admin, and General is always there)
      try {
        const created = await api.createKb(ws!.id, { name: "General" });
        queryClient.invalidateQueries({ queryKey: ["kbs", ws!.id] });
        return [created];
      } catch {
        return [];
      }
    },
    enabled: !!ws,
  });

  const kbList = kbs.data ?? [];
  /* **If the URL has one, the URL decides**: the two are not answering the same question -- the
     address bar says "what does this link point at", localStorage says "what was I looking at
     last time".
     A link somebody else shared has to beat my own memory, otherwise what opens is my KB --
     different data with an interface that looks exactly the same */
  const routeParams = useParams({ strict: false }) as { kbId?: string };
  const wantedKbId = routeParams.kbId ?? selectedKbId;
  const kb = kbList.find((k) => k.id === wantedKbId) ?? kbList[0] ?? null;

  /* **Switching KB is a navigation, not just making a note.**
     That "URL first" rule above is right, and the price is: every page in scope has kbId written
     into its address, so `selectedKbId` never gets its turn. Writing the store alone means the
     value changes and the component re-renders, yet what gets computed is still the same KB --
     which is why that dropdown in the top bar was **entirely dead** under `/kb/$kbId/*`:
     clicking did nothing, and it only took effect after a refresh (the home redirect reads the
     remembered value).

     So the navigation is folded into `setKb` itself, rather than requiring every call site to
     remember to pair it with a `navigate` -- the two that were missed are exactly those (the top
     bar dropdown, Chat's scope switcher), while the three that were written correctly all
     "jump to some specific page" and take the KB along on the way. A convention that can be
     forgotten is a convention that will be forgotten.

     Stay on the current page: switching KB on the ontology page should show the other KB's
     ontology, not send you back to the graph. When there is no kbId in the address (the account
     page and so on) just make the note -- nothing there should be yanked away to begin with, and
     the caller decides where to jump. */
  const navigate = useNavigate();
  const pathname = useLocation({ select: (l) => l.pathname });
  const currentKbId = routeParams.kbId;
  const setKb = useCallback(
    (id: string) => {
      kbStore.set(id);
      if (currentKbId && currentKbId !== id) {
        navigate({ to: samePageInKb(pathname, currentKbId, id), replace: false });
      }
    },
    [navigate, pathname, currentKbId],
  );

  return {
    kb,
    kbs: kbList,
    workspace: ws,
    workspaces: list,
    setWorkspace: wsStore.set,
    setKb,
  };
}
