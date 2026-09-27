// Alert event subscription. **Global, not per-KB** -- the top-bar badge counts across KBs, and a
// system-level alert has no KB at all.
//
// The event the server pushes carries no data and checks no permissions (see
// alerts_routes::stream): on receipt, just refetch. What anyone gets to see is decided by the
// list query. So this side does not need to know which KB is current either.
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

export function useAlertEvents() {
  const queryClient = useQueryClient();
  useEffect(() => {
    const es = new EventSource("/api/v1/alerts/events");
    es.addEventListener("alert", () => {
      queryClient.invalidateQueries({ queryKey: ["alerts"] });
    });
    return () => es.close();
  }, [queryClient]);
}
