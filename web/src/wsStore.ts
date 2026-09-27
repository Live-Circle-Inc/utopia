// Current workspace/KB selection: localStorage persistence + useSyncExternalStore subscription.
function makeStore(key: string) {
  let current: string | null =
    typeof localStorage !== "undefined" ? localStorage.getItem(key) : null;
  const listeners = new Set<() => void>();
  return {
    get: () => current,
    set: (id: string) => {
      current = id;
      localStorage.setItem(key, id);
      listeners.forEach((l) => l());
    },
    subscribe: (l: () => void) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
  };
}

export const wsStore = makeStore("utopia.ws");
export const kbStore = makeStore("utopia.kb");
