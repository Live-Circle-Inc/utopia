// The KB scope layer: everything under `/kb/$kbId` belongs to this knowledge base.
//
// **Why the KB goes in the path and not in a query parameter**: a KB is a container, not a
// filter -- the graph, search, the library, the ontology and review all live under it. A path
// can express that containment, and the router backstops you: `/kb/$kbId/search` simply
// cannot be constructed without an id. `?kb=` is optional, it falls off as soon as you hop
// between tabs, and falling off does not even raise an error -- so you are quietly looking at
// another KB's data, with an interface that looks exactly the same.
//
// **Why localStorage on top of that**: the two answer different questions. The URL answers
// "what does this link point at", localStorage answers "what was I looking at last time". So
// if it is in the URL the URL wins, and only when it is absent do we fall back to memory
// (see kb.tsx and KbRedirect below).
import { useEffect } from "react";
import { Link, Outlet, useNavigate, useParams } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";

import { ApiError, api } from "../api";
import { S } from "../i18n";
import { useKb } from "../kb";
import { kbStore, wsStore } from "../wsStore";

/** The landing page when the KB does not exist or there is no permission. **A blank graph is
 *  not enough** -- the most common failure of a shared link is that the other side has no
 *  permission, and an empty graph looks like "this KB is empty", which is a different
 *  thing entirely. */
function KbNoAccess({ status }: { status: number }) {
  return (
    <div className="grid h-full place-items-center px-6">
      <div className="max-w-sm text-center">
        <h2 className="text-[15px] text-neutral-200">
          {status === 404 ? S.kbScope.missingTitle : S.kbScope.deniedTitle}
        </h2>
        <p className="mt-2 text-xs leading-relaxed text-neutral-500">
          {status === 404 ? S.kbScope.missingBody : S.kbScope.deniedBody}
        </p>
        <Link
          to="/account/kbs"
          className="u-btn u-btn-ghost mt-4 inline-block px-3 py-1.5 text-xs"
        >
          {S.kbScope.myKbs}
        </Link>
      </div>
    </div>
  );
}

export function KbScope() {
  const { kbId } = useParams({ from: "/app/kb/$kbId" });
  // **Ask the backend directly instead of looking in the current workspace's list**: the link
  // may point at a KB in another workspace, in which case it is not in the list even though
  // the user does have permission -- judging by the list would kill it by mistake
  const kb = useQuery({
    queryKey: ["kbOne", kbId],
    queryFn: () => api.kbDetail(kbId),
    retry: false,
  });

  // Whichever KB gets opened, "last viewed" follows it; the workspace is aligned along with
  // it, otherwise the switcher in the top bar still shows the previous workspace
  useEffect(() => {
    if (!kb.data) return;
    kbStore.set(kb.data.id);
    wsStore.set(kb.data.workspace_id);
  }, [kb.data]);

  if (kb.isError) {
    const status = kb.error instanceof ApiError ? kb.error.status : 500;
    return <KbNoAccess status={status} />;
  }
  // Draw nothing while loading: this layer is only a scope, and flashing a spinner instead
  // looks like the page is jumping
  if (!kb.data) return null;
  return <Outlet />;
}

/** Takeover of the old paths (addresses without a KB, like `/graph`): resolve which KB to go
 *  to, then go there.
 *
 *  **It cannot redirect straight from beforeLoad** -- at that point the KB list has not been
 *  fetched yet, and localStorage may hold nothing at all (new device, cleared cache). So this
 *  is a component instead, which waits for useKb to resolve the KB before moving. */
export function KbRedirect({
  page,
}: {
  page:
    | "graph"
    | "search"
    | "chat"
    | "library"
    | "ontology"
    | "mappings"
    | "review"
    | "settings";
}) {
  const { kb } = useKb();
  const navigate = useNavigate();
  useEffect(() => {
    if (!kb) return;
    navigate({
      to: `/kb/$kbId/${page}`,
      params: { kbId: kb.id },
      replace: true,
    });
  }, [kb, page, navigate]);
  return null;
}
