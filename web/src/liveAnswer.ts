// The answers currently being generated, living outside the components.
//
// **Navigate away once and it was gone.** The streaming `turns` used to be Chat's
// component state, and leaving the conversation page unmounts that component: the state
// was gone, that fetch was still running, and the callbacks wrote into a component that
// had already died. Coming back remounted the component and read from the database -- and
// the database has no such row until generation finishes, so all you could see was the
// line you had asked yourself. Come back a while later and it was fine, because by then
// it had been persisted.
//
// The server-side half (generation does not vanish with the connection) is a separate
// fix; this half solves **whether you can see it when you come back**. Neither can be
// missing: the server keeps the answer, this keeps the stream.
//
// **Keyed by conversation, not a singleton.** This map used to be a single slot, on the
// grounds that "there is only ever one answer in progress at a time". That premise does
// not hold, and it was Chat itself that denied it -- switching KBs does not abort
// ("switching KBs should not kill an answer being written in another KB"), opening a new
// conversation does not abort ("starting a new one is not the same as giving up on the
// last one"), and the send guard is narrowed per conversation (explicitly refusing a
// global lockout that makes messages unsendable). Put those three "does not abort"
// together and two concurrent answers are a routinely reachable state, which a single
// slot cannot hold: the second start overwrote the slot, the first one's callbacks kept
// writing into "the last turn of the current slot", and the two answers interleaved word
// by word; whichever finished first took the other one's stop button away early, leaving
// itself with nobody able to stop it.
//
// So it became a map: whoever starts a stream holds the handle, and every read and write
// has a name on it. The guard in `send` needs no change -- what it was asking all along
// is "is this conversation streaming", and now that question is finally only about this
// conversation.
import type { ChatStep, Source } from "./api";

export interface Turn {
  role: "user" | "assistant";
  content: string;
  steps?: ChatStep[];
  sources?: Source[];
  error?: string;
}

/** A snapshot entry: pure data, for rendering to look at. abort does not go into the
    snapshot -- rendering has no business reaching for it */
export interface Live {
  kbId: string;
  /** null for a new conversation until the server returns an id; kbId is what tells two
      new conversations apart while neither of them has an id yet */
  conversationId: string | null;
  turns: Turn[];
  streaming: boolean;
}

interface Slot {
  live: Live;
  abort: () => void;
}

const lives = new Map<string, Slot>();
const listeners = new Set<() => void>();

// The snapshot is replaced wholesale: useSyncExternalStore relies on reference equality
// to skip unrelated renders -- **no change in another conversation should alter this
// one's picture**, and this old comment only became literally true once it was keyed.
let snapshot: readonly Live[] = [];

function emit() {
  snapshot = [...lives.values()].map((s) => s.live);
  listeners.forEach((l) => l());
}

// A new conversation with no id yet is held by an internal placeholder token; identify
// remaps it to the real id
let pendingSeq = 0;

export interface LiveHandle {
  /** The new conversation got its id from the server: remap this entry from the
      placeholder token to the real id */
  identify: (conversationId: string) => void;
  /** Patch the last turn of this answer (the assistant's one). During generation it is
      the only thing that changes */
  patchLast: (f: (t: Turn) => Turn) => void;
  /** Finish (normally, on error, or because someone pressed stop).
   *
   * **Does not clear.** One version did clear, and that version had a very ugly bug:
   * navigating away unmounts the component, and "hand the final result back to the
   * component" was called on the component that had already died -- a no-op. So the
   * store was empty, the new component had already claimed this conversation earlier
   * and therefore would not go read the database again, and coming back the whole
   * conversation was blank, without even the line you had asked yourself.
   *
   * At that moment this is the only place still holding this content, so it stays: only
   * `streaming` gets written down. The next `begin` clears out the finished entries
   * (see begin), and switching to another conversation fails to claim and naturally
   * reads the database. */
  finish: () => void;
  /** streamChat's abort only exists once it has returned: begin puts in a placeholder
      first and swaps in the real abort once it has it */
  setAbort: (abort: () => void) => void;
}

export const liveAnswer = {
  /** `useSyncExternalStore` requires the same snapshot object to keep the same
      reference as long as it has not changed */
  get: (): readonly Live[] => snapshot,
  subscribe: (l: () => void) => {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
  /** Claim "the conversation being looked at". Found by conversation; kbId only tells
      apart two new conversations that neither have an id yet. Not found means this
      conversation is not here -- the display falls back to the history in the database */
  entry: (kbId: string | null, conversationId: string | null): Live | null =>
    snapshot.find((e) => e.kbId === kbId && e.conversationId === conversationId) ?? null,
  /** Start one. A follow-up in the same conversation replaces the old entry under the
   * same key; at the same time every finished entry is cleared --
   *
   * Clearing only happens when someone sends a new message, and if a cleared
   * conversation is opened again it fails to claim, naturally reads the database, and
   * the content matches (the server persisted it on done). Without clearing, this map
   * grows without bound; entries in progress are never cleared -- that is precisely
   * why this module exists. */
  begin: (
    kbId: string,
    conversationId: string | null,
    turns: Turn[],
    abort: () => void,
  ): LiveHandle => {
    for (const [k, s] of lives) if (!s.live.streaming) lives.delete(k);
    let key = conversationId ?? `__pending__${++pendingSeq}`;
    const slot: Slot = { live: { kbId, conversationId, turns, streaming: true }, abort };
    lives.set(key, slot);
    emit();
    return {
      identify: (id: string) => {
        const current = lives.get(key);
        if (!current) return;
        lives.delete(key);
        key = id;
        current.live = { ...current.live, conversationId: id };
        lives.set(key, current);
        emit();
      },
      patchLast: (f) => {
        const current = lives.get(key);
        if (!current || current.live.turns.length === 0) return;
        const turns = [...current.live.turns];
        turns[turns.length - 1] = f(turns[turns.length - 1]);
        current.live = { ...current.live, turns };
        emit();
      },
      finish: () => {
        const current = lives.get(key);
        if (!current || !current.live.streaming) return;
        current.live = { ...current.live, streaming: false };
        emit();
      },
      setAbort: (a) => {
        const current = lives.get(key);
        if (current) current.abort = a;
      },
    };
  },
  /** For the stop button only: abort + finish the conversation being looked at. The
      other ones go on writing their own entries as usual */
  stop: (kbId: string, conversationId: string | null) => {
    for (const s of lives.values()) {
      if (s.live.kbId === kbId && s.live.conversationId === conversationId) {
        s.abort();
        s.live = { ...s.live, streaming: false };
        emit();
        return;
      }
    }
  },
};
