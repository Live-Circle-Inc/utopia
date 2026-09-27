// KB event-stream subscription: an incoming event only invalidates and refetches through
// react-query (events carry no business data, so they are idempotent by nature).
// EventSource reconnects on its own when the connection drops; this replaces the polling in
// Library/Review.
//
// **Invalidation is coalesced.** While a document is being extracted, every fact that lands emits
// a graph event. Each event used to invalidate on its own, and over those few seconds the graph
// page refetched the overview a dozen-odd times -- always the same graph, and only the last one
// counted. So an event now only records the key, and after a short pause everything is
// invalidated in one go: a burst of events buys exactly one refetch, and that last one is
// guaranteed to contain every change before it. Idempotence is unchanged; "refresh on every one"
// simply became "refresh on the last one".
import { useEffect } from "react";
import { useQueryClient, type QueryKey } from "@tanstack/react-query";

/** The quiet period between bursts of events. The gap between facts landing during extraction is
 *  far shorter than this, and the eye cannot see a delay this small */
const SETTLE_MS = 300;

export function useKbEvents(kbId: string | undefined) {
  const queryClient = useQueryClient();

  useEffect(() => {
    if (!kbId) return;
    const pending = new Map<string, QueryKey>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      timer = null;
      const keys = [...pending.values()];
      pending.clear();
      for (const key of keys) queryClient.invalidateQueries({ queryKey: key });
    };
    const invalidate = (...keys: QueryKey[]) => {
      for (const key of keys) pending.set(JSON.stringify(key), key);
      if (timer === null) timer = setTimeout(flush, SETTLE_MS);
    };

    const es = new EventSource(`/api/v1/kbs/${kbId}/events`);
    es.addEventListener("document", () => invalidate(["documents", kbId], ["graph"]));
    es.addEventListener("graph", () => invalidate(["graph"]));
    // mapping discovery also emits review when it finishes: the Pending column has to refresh too
    es.addEventListener("review", () => invalidate(["review", kbId], ["mappings", kbId]));
    // a line of memory extracted a fact that waits for a human nod (0015): the confirmation card
    // in the conversation grows in along with it
    es.addEventListener("pending", () => invalidate(["pending", kbId], ["review", kbId]));
    es.addEventListener("source", () => invalidate(["sources", kbId], ["documents", kbId]));
    return () => {
      es.close();
      // on unmount, flush what is queued rather than dropping it: coming back to the page must
      // show fresh data
      if (timer !== null) {
        clearTimeout(timer);
        flush();
      }
    };
  }, [kbId, queryClient]);
}
