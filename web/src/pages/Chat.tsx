/* Chat: agentic conversation (retrieval/graph tools + remember memory).
   Conversations are persisted: the left rail lists them; the context is assembled server-side,
   the frontend only sends conversation_id + the new message; the action trail (steps) and the
   citations (sources) are stored along with the message, and history replay shares its rendering
   with the live stream. */
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import remend from "remend";
import {
  ArrowUp,
  BookOpen,
  Check,
  ChevronDown,
  Database,
  GitCompareArrows,
  History,
  Search as SearchIcon,
  Square,
  SquarePen,
  MoreHorizontal,
  Waypoints,
  Wrench,
} from "lucide-react";
import { ThinkingOrb, type OrbState } from "thinking-orbs";
import {
  conversationsApi,
  reattachChat,
  streamChat,
  type ChatStep,
  type ConversationRow,
} from "../api";
import { S } from "../i18n";
import { toast } from "../toast";
import { useKb, useKbId } from "../kb";
import { DangerConfirm, RAIL_CLS } from "../ui";
import { liveAnswer, type LiveHandle, type Turn } from "../liveAnswer";
import { NodCard } from "./PendingFacts";

/* `Turn` is defined over in liveAnswer: the in-flight answer is also a list of Turns,
   and it has to outlive this component (see the notes at the top of that file) */

/** Same-tab memory: the last conversation (per KB) and the unsent draft -- coming back to the
 *  page restores them, while a new tab starts from scratch */
const lastKey = (kbId: string) => `chat:last:${kbId}`;
const DRAFT_KEY = "chat:draft";

export function Chat() {
  const kbId = useKbId();
  const { kb, kbs, setKb } = useKb();
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  // The conversation is the route: /chat/$conversationId; the URL is the single source of truth
  // for which conversation is current (refresh and back work for free)
  const { conversationId: routeConvId } = useParams({ strict: false }) as {
    conversationId?: string;
  };
  const [activeId, setActiveId] = useState<string | null>(null);
  // The criterion for the route-sync effect: state commits later than the re-render navigate
  // triggers, so only a synchronous ref write makes the "after a streaming create, only swap the
  // URL" guard hit reliably
  const activeIdRef = useRef<string | null>(null);
  // The turns that have already finished, read from the database. **The in-flight one is not in
  // here** -- see below
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState(() => sessionStorage.getItem(DRAFT_KEY) ?? "");
  /* **Claim by URL, not by state.** The top of this file already says "the URL is the single
     source of truth for the current conversation", and yet this once used `activeId` -- which is
     state, and coming back after navigating away it updates later than the first render, so that
     frame fails to recognise itself and the screen sits empty. Using the id from the address bar
     leaves no ordering to get wrong. While a new conversation has not been given an id yet both
     are empty, which lines up too */
  const currentId = routeConvId ?? activeId;
  // Every in-flight answer lives outside the component (see liveAnswer.ts); here we claim only
  // "the one being looked at". Changes in any other conversation never touch this one's snapshot
  // reference -- "an answer being generated elsewhere should not change anything here" is now
  // literally true at the render layer too: React skips re-rendering on reference equality, so
  // another conversation growing word by word no longer disturbs the current one
  const liveHere = useSyncExternalStore(
    liveAnswer.subscribe,
    () => liveAnswer.entry(kb?.id ?? null, currentId),
  );
  /* **It is "this conversation" that is streaming, not "some conversation".**
     Written globally, while another conversation is generating this one's input box would turn
     into a stop button and refuse to send, and the last turn would be treated as still streaming
     -- so the citations get hidden (that criterion lives in TurnView).
     An answer being generated elsewhere should not change anything here */
  const streaming = liveHere?.streaming ?? false;
  const shown = liveHere ? liveHere.turns : turns;
  const [scopeOpen, setScopeOpen] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<ConversationRow | null>(null);
  // Conversation search. **Searches the body as well as the title** -- what people remember is
  // usually the sentence they asked
  const [convSearch, setConvSearch] = useState("");
  // Which row has its three-dot menu open. Only one at a time
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const scopeRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Scope popover: click outside / Esc to close (same convention as ui/Dropdown)
  useEffect(() => {
    if (!scopeOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (!scopeRef.current?.contains(e.target as Node)) setScopeOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setScopeOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [scopeOpen]);

  const convs = useQuery({
    queryKey: ["conversations", kb?.id, convSearch],
    queryFn: () => conversationsApi.list(kb!.id, convSearch),
    enabled: !!kb,
    placeholderData: (prev) => prev,
  });
  // Renaming: **edit in place**, no dialog -- changing one name is not worth interrupting the
  // whole page
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const rename = useMutation({
    mutationFn: (v: { id: string; title: string }) =>
      conversationsApi.rename(kb!.id, v.id, v.title),
    onSuccess: () => {
      setRenamingId(null);
      queryClient.invalidateQueries({ queryKey: ["conversations", kb?.id] });
    },
    onError: (e: Error) => toast.error(e.message),
  });

  // Drop straight to the bottom (instant): smooth scrolling crawls the whole way while the
  // stream keeps appending
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "instant" });
  }, [shown]);

  // Switching KB goes back to a new conversation (receiving kb for the first time is not a
  // switch -- loading /chat/$id directly must not wipe the URL)
  const prevKbRef = useRef<string | null>(null);
  useEffect(() => {
    const prev = prevKbRef.current;
    prevKbRef.current = kb?.id ?? null;
    if (prev && kb && prev !== kb.id) {
      // **No abort**: switching KB should not kill an answer being written in the other KB; it
      // lands in that KB's conversation
      activeIdRef.current = null;
      setActiveId(null);
      setTurns([]);
      navigate({ to: "/kb/$kbId/chat", params: { kbId }, replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kb?.id]);

  // Route -> conversation load; a bare /chat restores this KB's last conversation (come back to
  // the page and you are still in the same conversation)
  useEffect(() => {
    if (!kb) return;
    if (!routeConvId) {
      const last = sessionStorage.getItem(lastKey(kb.id));
      if (last) {
        navigate({
          to: "/kb/$kbId/chat/$conversationId",
          params: { kbId, conversationId: last },
          replace: true,
        });
      }
      return;
    }
    if (routeConvId === activeIdRef.current) return; // After a streaming create, sync the URL only -- do NOT reload
    loadConversation(routeConvId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kb?.id, routeConvId]);

  // A restored draft expands the textarea (its height is otherwise maintained by onChange)
  useEffect(() => {
    const el = inputRef.current;
    if (el && el.value) {
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, 192)}px`;
    }
  }, []);

  const invalidateList = () =>
    queryClient.invalidateQueries({ queryKey: ["conversations", kb?.id] });

  /** A click in the list only changes the URL; loading is the route-sync effect's job */
  const openConversation = (id: string) =>
    navigate({
      to: "/kb/$kbId/chat/$conversationId",
      params: { kbId, conversationId: id },
    });

  /** Reattach to an answer that is being generated. If nothing is running the server replies
   *  `idle` and nothing happens. */
  const attachIfRunning = (id: string, history: Turn[]) => {
    let abort = () => {};
    let handle: LiveHandle | null = null;
    const stop = reattachChat(kb!.id, id, {
      onConversation: () => {},
      /* **Only create the turn once the snapshot arrives.** Putting down an empty slot first
         and then waiting for the answer flashes an empty assistant bubble on conversations where
         nothing is running -- and that is the overwhelming majority of them.
         The snapshot overwrites: it is that answer in full as of right now, not a delta */
      onSnapshot: (s) => {
        handle = liveAnswer.begin(
          kb!.id,
          id,
          [
            ...history,
            {
              role: "assistant",
              content: s.content,
              steps: s.steps.length ? s.steps : undefined,
              sources: s.sources.length ? s.sources : undefined,
            },
          ],
          abort,
        );
      },
      onSources: (sources) => handle?.patchLast((t) => ({ ...t, sources })),
      onStep: (step) =>
        handle?.patchLast((t) => ({ ...t, steps: [...(t.steps ?? []), step] })),
      onDelta: (text) => handle?.patchLast((t) => ({ ...t, content: t.content + text })),
      onDone: () => {
        handle?.finish();
        invalidateList();
      },
      onError: (message) => {
        handle?.patchLast((t) => ({ ...t, error: message }));
        handle?.finish();
      },
      onIdle: () => {},
    });
    abort = stop;
  };

  const loadConversation = async (id: string) => {
    // Coming back to the conversation still being written: claim it directly, don't read the
    // database -- that row is only there once it has finished being written
    if (liveAnswer.entry(kb!.id, id)) {
      activeIdRef.current = id;
      setActiveId(id);
      return;
    }
    activeIdRef.current = id;
    setActiveId(id);
    try {
      const { messages } = await conversationsApi.detail(kb!.id, id);
      sessionStorage.setItem(lastKey(kb!.id), id);
      const history: Turn[] = messages.map((m) => ({
        role: m.role,
        content: m.content,
        steps: m.steps.length ? m.steps : undefined,
        sources: m.sources.length ? m.sources : undefined,
      }));
      setTurns(history);
      /* **Reattach after a refresh.** The store above lives only inside this one page; a
         refresh, a new tab or a different machine cannot reach it, while over on the server the
         generation is still running. So we ask "is anything running for this conversation" --
         no is the most common answer, and the cost is one request that returns `idle`
         immediately.
         We only ask when the last message is the user's: that is exactly the shape of "asked
         but not answered yet" */
      if (history[history.length - 1]?.role === "user") {
        attachIfRunning(id, history);
      }
    } catch {
      // Dead link (conversation deleted / belongs to another KB): quietly fall back to a new
      // conversation
      sessionStorage.removeItem(lastKey(kb!.id));
      activeIdRef.current = null;
      setActiveId(null);
      setTurns([]);
      navigate({ to: "/kb/$kbId/chat", params: { kbId }, replace: true });
    }
  };

  const newChat = () => {
    // Again no abort: starting a new conversation does not mean abandoning the last one
    if (kb) sessionStorage.removeItem(lastKey(kb.id));
    activeIdRef.current = null;
    setActiveId(null);
    setTurns([]);
    navigate({ to: "/kb/$kbId/chat", params: { kbId } });
    inputRef.current?.focus();
  };

  const removeConversation = async (id: string) => {
    await conversationsApi.remove(kb!.id, id);
    if (sessionStorage.getItem(lastKey(kb!.id)) === id) {
      sessionStorage.removeItem(lastKey(kb!.id));
    }
    invalidateList();
    if (id === activeId) newChat();
  };

  const send = () => {
    const q = input.trim();
    if (!q || streaming || !kb) return;
    setInput("");
    sessionStorage.removeItem(DRAFT_KEY);
    if (inputRef.current) inputRef.current.style.height = "auto";

    /* **The result stays in the store; it is not handed back to component state.**
       Handing it back would have to go through a `setTurns`, and by the time the stream ends this
       component may long since have unmounted -- that call is a no-op and the content vanishes
       with it (come back and it is blank, without even the question bubble).
       Kept in the store, whoever mounts claims it. This conversation has an identity from the
       opening moment: create the entry first, open the stream after, and the callbacks follow the
       handle and write only to their own conversation */
    const handle = liveAnswer.begin(
      kb.id,
      activeId,
      // Continue from the turns currently shown on screen, not from component state -- once a
      // stream ends the content only lands in the store, while state is still the database
      // history from the last conversation load, and using it would make the previous answer
      // disappear from the screen
      [...(liveHere?.turns ?? turns), { role: "user", content: q }, { role: "assistant", content: "" }],
      () => {},
    );
    const abort = streamChat(
      kb.id,
      { conversation_id: activeId ?? undefined, message: q },
      {
        onConversation: (id) => {
          handle.identify(id);
          // Write the ref synchronously before swapping the URL: the route-sync effect then
          // skips the reload because the ids are equal, and the stream is not interrupted
          activeIdRef.current = id;
          setActiveId(id);
          sessionStorage.setItem(lastKey(kb.id), id);
          navigate({
            to: "/kb/$kbId/chat/$conversationId",
            params: { kbId, conversationId: id },
            replace: true,
          });
          invalidateList();
        },
        onSources: (sources) => handle.patchLast((t) => ({ ...t, sources })),
        onStep: (step) =>
          handle.patchLast((t) => ({ ...t, steps: [...(t.steps ?? []), step] })),
        onDelta: (text) =>
          handle.patchLast((t) => ({ ...t, content: t.content + text })),
        onDone: () => {
          handle.finish();
          invalidateList();
        },
        onError: (message) => {
          handle.patchLast((t) => ({ ...t, error: message }));
          handle.finish();
        },
      },
    );
    // streamChat's abort only exists once it has returned; until the real abort is in hand, the
    // handle holds a no-op in its place
    handle.setAbort(abort);
  };

  /* The composer card: enters centred on a new conversation's first screen, then docks to the
     bottom once you are in the conversation (the same block of JSX reused in both places) */
  const composerCard = (
    <div className="rounded-2xl border border-white/[0.12] bg-white/[0.04] backdrop-blur-md focus-within:border-white/30 transition-colors px-4 pt-3 pb-2">
      <textarea
        ref={inputRef}
        rows={1}
        className="w-full bg-transparent outline-none text-sm resize-none leading-relaxed max-h-48 u-scroll placeholder:text-neutral-600"
        placeholder={S.ask.placeholder}
        value={input}
        onChange={(e) => {
          setInput(e.target.value);
          sessionStorage.setItem(DRAFT_KEY, e.target.value);
          const el = e.currentTarget;
          el.style.height = "auto";
          el.style.height = `${Math.min(el.scrollHeight, 192)}px`;
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            send();
          }
        }}
      />
      <div className="flex items-center justify-between gap-3 pt-1">
        <div className="flex items-center gap-2.5 min-w-0">
          {/* Scope chip: makes "which KB am I asking" visible right at the point of asking;
              switching KB keeps the existing semantics (starts a new conversation) */}
          <div ref={scopeRef} className="relative shrink-0">
            <button
              onClick={() => setScopeOpen((v) => !v)}
              title={S.ask.scopeLabel}
              className="flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-neutral-400 hover:text-neutral-200 hover:bg-white/[0.07] transition-colors max-w-52"
            >
              <Database size={12} className="shrink-0 text-neutral-500" />
              <span className="truncate">{kb?.name ?? "…"}</span>
              <ChevronDown
                size={11}
                className={`shrink-0 text-neutral-600 transition-transform ${
                  scopeOpen ? "rotate-180" : ""
                }`}
              />
            </button>
            {scopeOpen && (
              <div className="u-pop u-pop-up absolute bottom-full mb-1.5 left-0 z-50 w-56 rounded-lg shadow-xl overflow-hidden">
                <div className="px-2.5 pt-2 pb-1 text-[9.5px] font-medium uppercase tracking-[0.1em] text-neutral-600 border-b border-white/5">
                  {S.ask.scopeLabel}
                </div>
                <div className="u-scroll max-h-60 overflow-y-auto">
                  {kbs.map((k) => (
                    <button
                      key={k.id}
                      onClick={() => {
                        setScopeOpen(false);
                        if (k.id !== kb?.id) setKb(k.id);
                      }}
                      className={`w-full flex items-center gap-2 text-left px-2.5 py-1.5 text-xs ${
                        k.id === kb?.id
                          ? "bg-white/[0.12] text-white"
                          : "text-neutral-300 hover:bg-white/[0.06] hover:text-white"
                      }`}
                    >
                      <span className="flex-1 min-w-0 truncate">{k.name}</span>
                      {k.id === kb?.id && <Check size={12} className="shrink-0 text-neutral-400" />}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
          <span className="text-[11px] text-neutral-600 truncate">{S.ask.composerHint}</span>
        </div>
        {streaming ? (
          <button
            onClick={() => {
              // **Stop only the conversation being looked at** -- changing page, conversation
              // or KB interrupts nothing (liveAnswer.ts), and the others keep writing to their
              // own entries
              if (kb) liveAnswer.stop(kb.id, currentId);
            }}
            title={S.ask.stop}
            className="h-8 w-8 shrink-0 rounded-lg grid place-items-center bg-white/[0.08] text-neutral-200 hover:bg-white/[0.14] transition-colors"
          >
            <Square size={11} fill="currentColor" />
          </button>
        ) : (
          <button
            onClick={send}
            disabled={!input.trim()}
            title={S.ask.send}
            className={`h-8 w-8 shrink-0 rounded-lg grid place-items-center transition-colors ${
              input.trim()
                ? "bg-white text-black hover:bg-neutral-200"
                : "bg-white/[0.07] text-neutral-600"
            }`}
          >
            <ArrowUp size={15} strokeWidth={2.4} />
          </button>
        )}
      </div>
    </div>
  );

  return (
    <div className="h-full flex">
      {/* Conversation rail */}
      <aside className={`${RAIL_CLS} flex flex-col`}>
        <div className="px-2 pt-3 pb-1">
          {/* Same styling as a conversation row: the rail is one column of homogeneous rows,
              and New chat is simply the first of them */}
          <button
            onClick={newChat}
            className="w-full flex items-center gap-2 rounded-lg px-2.5 py-2 text-left text-[13px] text-neutral-300 hover:bg-white/[0.05] hover:text-white transition-colors"
          >
            <SquarePen size={14} className="shrink-0 text-neutral-500" />
            {S.ask.newChat}
          </button>
        </div>
        {/* Search. **Duplicate titles are the norm** (ask the same question twice and you have
            one), while the sentence in the body is what people actually remember -- so the
            server searches both */}
        <div className="px-2 pb-2">
          <input
            className="input-dark w-full px-2.5 py-1.5 text-[12.5px]"
            placeholder={S.ask.searchConversations}
            value={convSearch}
            onChange={(e) => setConvSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setConvSearch("")}
          />
        </div>
        <div className="u-scroll flex-1 overflow-y-auto px-2 pb-3 space-y-0.5">
          {(convs.data?.conversations ?? []).map((c: ConversationRow) => (
            <div
              key={c.id}
              className={`group relative rounded-lg transition-colors ${
                c.id === activeId ? "u-nav-active" : "hover:bg-white/[0.05]"
              }`}
            >
              {/* Single-line title; the delete control surfaces on hover (it asks for
                  confirmation, it does not delete outright) */}
              {renamingId === c.id ? (
                /* Edit in place: Enter saves, Esc cancels. Changing one name is not worth a
                   dialog */
                <input
                  autoFocus
                  className="input-dark w-full px-2 py-1.5 text-[13px]"
                  value={renameDraft}
                  onChange={(e) => setRenameDraft(e.target.value)}
                  onBlur={() => setRenamingId(null)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && renameDraft.trim())
                      rename.mutate({ id: c.id, title: renameDraft });
                    if (e.key === "Escape") setRenamingId(null);
                  }}
                />
              ) : (
                <button
                  onClick={() => openConversation(c.id)}
                  className="w-full text-left px-2.5 py-2"
                >
                  <span
                    className={`block truncate pr-5 text-[13px] ${
                      c.id === activeId ? "text-white" : "text-neutral-300"
                    }`}
                  >
                    {c.title || S.ask.untitled}
                  </span>
                </button>
              )}
              {/* Three-dot menu: **one entry point holding every action**. It used to be a bare
                  delete out on the right, and delete is the one action here that least deserves
                  to happen in a single step */}
              {renamingId !== c.id && (
                <button
                  onClick={() => setMenuFor(menuFor === c.id ? null : c.id)}
                  title={S.ask.moreActions}
                  className="absolute right-2 top-1/2 -translate-y-1/2 hidden group-hover:block text-neutral-600 hover:text-neutral-200"
                >
                  <MoreHorizontal size={14} />
                </button>
              )}
              {menuFor === c.id && (
                <>
                  {/* Click elsewhere to close. A full-screen overlay rather than a document
                      listener: no need to remember to remove the listener on unmount */}
                  <div
                    className="fixed inset-0 z-10"
                    onClick={() => setMenuFor(null)}
                  />
                  <div className="glass-strong absolute right-2 top-8 z-20 w-32 rounded-lg py-1 shadow-xl">
                    <button
                      className="w-full px-3 py-1.5 text-left text-xs text-neutral-300 hover:bg-white/5"
                      onClick={() => {
                        setRenameDraft(c.title || "");
                        setRenamingId(c.id);
                        setMenuFor(null);
                      }}
                    >
                      {S.ask.rename}
                    </button>
                    <button
                      className="w-full px-3 py-1.5 text-left text-xs text-neutral-300 hover:bg-white/5"
                      onClick={() => {
                        navigator.clipboard?.writeText(c.title || "");
                        setMenuFor(null);
                      }}
                    >
                      {S.ask.copyTitle}
                    </button>
                    <button
                      className="w-full px-3 py-1.5 text-left text-xs text-[var(--u-danger)] hover:bg-white/5"
                      onClick={() => {
                        setPendingDelete(c);
                        setMenuFor(null);
                      }}
                    >
                      {S.ask.deleteConversation}
                    </button>
                  </div>
                </>
              )}
            </div>
          ))}
          {convs.data?.conversations.length === 0 && (
            <p className="px-2.5 py-2 text-xs text-neutral-600">{S.ask.noConversations}</p>
          )}
        </div>
      </aside>

      {/* Conversation area: a new conversation's first screen = greeting + centred composer
          (the ChatGPT/Claude convention); once there are messages the composer docks to the
          bottom */}
      <div className="flex-1 min-w-0 flex flex-col">
        {shown.length === 0 ? (
          /* Anchored to the top third rather than vertically centred: centring looks like it is
             sinking in a tall window.
             22vh + top chrome(~100px) ≈ greeting lands at 37% height, composer centre ~49% */
          <div className="flex-1 px-4 pt-[22vh]">
            <div className="w-full max-w-3xl mx-auto">
              <h1
                className="text-center text-[26px] text-neutral-100 mb-9"
                style={{ fontFamily: "var(--font-brand)", letterSpacing: "0.03em" }}
              >
                {S.ask.greeting}
              </h1>
              {composerCard}
            </div>
          </div>
        ) : (
          <>
            <div className="flex-1 overflow-y-auto u-scroll px-4 py-6">
              <div className="max-w-3xl mx-auto space-y-4">
                {shown.map((t, i) => (
                  <TurnView key={i} turn={t} live={streaming && i === shown.length - 1} />
                ))}
                <div ref={bottomRef} />
              </div>
            </div>
            <div className="px-4 pb-4 pt-2">
              <div className="max-w-3xl mx-auto">{composerCard}</div>
            </div>
          </>
        )}
      </div>

      {pendingDelete && (
        <DangerConfirm
          title={S.ask.deleteTitle}
          hint={S.ask.deleteHint(pendingDelete.title || S.ask.untitled)}
          confirmLabel={S.ask.deleteBtn}
          cancelLabel={S.ask.cancel}
          onConfirm={() => {
            removeConversation(pendingDelete.id);
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </div>
  );
}

/** One stretch of prose, or one group of calls that happened at the same time. */
type Segment =
  | { kind: "text"; text: string; last: boolean }
  | { kind: "steps"; steps: ChatStep[] };

/** Split one reply turn into segments arranged in the order things happened.
 *
 *  The split point is `step.at` -- how long the prose already was when that step happened.
 *  **Messages stored before this migration have no `at`**: back then the ordering information
 *  genuinely was not saved, and it cannot be invented, nor should it be -- they fall back to
 *  the old look, with the whole trail up front. */
function segments(turn: Turn): Segment[] {
  const steps = turn.steps ?? [];
  const text = turn.content ?? "";
  if (steps.length === 0) {
    return text ? [{ kind: "text", text, last: true }] : [];
  }
  if (steps.some((s) => s.at === undefined)) {
    return [
      { kind: "steps", steps },
      ...(text ? [{ kind: "text" as const, text, last: true }] : []),
    ];
  }
  const out: Segment[] = [];
  let cursor = 0;
  for (let i = 0; i < steps.length; ) {
    const at = steps[i].at!;
    // Steps at the same position are joined into one group: there is no prose between the
    // several calls in one round -- they were a single fan-out to begin with
    let j = i;
    while (j < steps.length && steps[j].at === at) j++;
    const before = text.slice(cursor, at);
    if (before) out.push({ kind: "text", text: before, last: false });
    out.push({ kind: "steps", steps: steps.slice(i, j) });
    cursor = at;
    i = j;
  }
  const tail = text.slice(cursor);
  if (tail) out.push({ kind: "text", text: tail, last: true });
  return out;
}

function stepIcon(kind: ChatStep["kind"]) {
  if (kind === "search") return <SearchIcon size={11} />;
  if (kind === "docs") return <BookOpen size={11} />;
  if (kind === "entity") return <Waypoints size={11} />;
  if (kind === "facts") return <History size={11} />;
  // facts reads the world axis and changes reads the record-time axis, so the two graph tools get
  // different icons -- looking at the step line, the user should be able to tell which axis was
  // being asked about
  if (kind === "changes") return <GitCompareArrows size={11} />;
  if (kind === "query") return <Database size={11} />;
  return <Wrench size={11} />;
}

/** Tool step -> orb state: the thinking orb speaks the language of the current action */
function orbState(kind?: ChatStep["kind"]): OrbState {
  if (kind === "search" || kind === "docs") return "searching";
  if (kind === "entity") return "connecting";
  if (kind === "facts" || kind === "changes") return "solving";
  if (kind === "query" || kind === "tool") return "working";
  return "listening"; // no steps yet: the message has only just arrived
}

/** Thinking indicator: the thinking-orbs orb + the current action (the app is styled dark for
 *  good, so theme is pinned to dark). */
function Thinking({ step }: { step?: ChatStep }) {
  return (
    <span className="inline-flex items-center gap-2.5 text-neutral-500">
      <ThinkingOrb state={orbState(step?.kind)} size={20} theme="dark" />
      {step && (
        <span className="text-xs truncate">
          {step.label} · {step.detail}
        </span>
      )}
    </span>
  );
}

function TurnView({ turn, live }: { turn: Turn; live?: boolean }) {
  const kbId = useKbId();
  if (turn.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="u-bubble-user max-w-[85%] rounded-2xl rounded-tr-sm px-4 py-2 text-sm whitespace-pre-wrap text-neutral-100">
          {turn.content}
        </div>
      </div>
    );
  }

  const thinking = live && !turn.content && !turn.error;
  const lastStep = turn.steps?.[turn.steps.length - 1];

  return (
    <div className="max-w-[95%]">
      {/* No bubble for agent replies: the prose lands straight on the canvas (user messages
          keep a bubble so the roles stay distinguishable) */}
      <div className="py-1 text-sm text-neutral-200 leading-relaxed">
        {/* **The trail is threaded through the prose in the order things happened.**
            The model looks things up as it talks: say a sentence, make a call, say another one.
            Hoist the calls into one block at the front and it reads as "searched seven times,
            then said it all in one breath" -- which is not what it did, and the sentence between
            two adjacent calls, "let me look at this one first", loses the thing it explained.
            Several calls in the same round share a position, so they merge into a group
            naturally -- and one group is one round */}
        {segments(turn).map((seg, i) =>
          seg.kind === "steps" ? (
            <div
              key={i}
              className="my-2.5 space-y-1 border-l border-white/15 pl-2.5"
            >
              {seg.steps.map((s, j) => (
                <div key={j}>
                  <div className="flex items-center gap-1.5 text-xs">
                    <span className="text-neutral-600">{stepIcon(s.kind)}</span>
                    <span className="text-neutral-400 truncate">{s.label}</span>
                    <span className="text-neutral-600 shrink-0">· {s.detail}</span>
                  </div>
                  {/* The remember step is followed by a confirmation card (0015): the facts
                      extracted from this sentence wait for a human nod first. Extraction is
                      async, so the card only grows in when the job finishes; on replay it is
                      redrawn from the same chunk */}
                  {s.chunk_id && <NodCard kbId={kbId} chunkId={s.chunk_id} />}
                </div>
              ))}
            </div>
          ) : (
            /* react-markdown carries the rendering (all skinning belongs to the u-chat-prose
               design system), remend patches unclosed syntax mid-stream (bold/fences/links),
               and rehype-highlight does the code highlighting -- mature parts assembled, the
               look holds up on its own.
               **Only the segment still growing needs remend**: the earlier ones have already
               closed out */
            <div key={i} className="u-chat-prose">
              <Markdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeHighlight]}>
                {live && seg.last ? remend(seg.text) : seg.text}
              </Markdown>
            </div>
          ),
        )}
        {thinking && <Thinking step={lastStep} />}
        {turn.error && <div className="text-rose-400">{turn.error}</div>}
      </div>
      {/* **Citations only appear once the answer has finished speaking.**
          `sources` is sent incrementally, one retrieval at a time; render along with it and a
          still-growing list hangs below a sentence that is not finished, pushing the prose
          upwards as it grows. It is the answer's signature, not part of the process -- the
          process has already been accounted for by the trail above */}
      {!live && turn.sources && turn.sources.length > 0 && (
        <div className="mt-2 space-y-1">
          {turn.sources.map((s) =>
            s.kind === "charter" ? (
              /* Handbook citation: visually set apart from data citations (BookOpen), and
                 jumps to the properly typeset /docs section */
              <Link
                key={s.n}
                to="/docs/$slug"
                params={{ slug: s.slug! }}
                hash={s.anchor || undefined}
                title={s.excerpt}
                className="flex items-center gap-1.5 text-xs text-neutral-500 glass rounded-lg px-3 py-1.5 glass-hover hover:text-neutral-300"
              >
                <span className="u-num text-[var(--u-accent)]">[{s.n}]</span>
                <BookOpen size={11} className="shrink-0 text-neutral-600" />
                <span className="truncate">
                  {/* In the intro section the heading is the article name; avoids "X › X" */}
                  {s.heading && s.heading !== s.filename
                    ? `${s.filename} › ${s.heading}`
                    : s.filename}
                </span>
              </Link>
            ) : (
              <Link
                key={s.n}
                to="/kb/$kbId/doc/$docId"
                params={{ kbId, docId: s.document_id! }}
                search={{ chunk: s.chunk_id }}
                title={s.excerpt}
                className="block text-xs text-neutral-500 glass rounded-lg px-3 py-1.5 glass-hover hover:text-neutral-300"
              >
                <span className="u-num text-[var(--u-accent)]">[{s.n}]</span> {s.filename} ·{" "}
                {s.excerpt.slice(0, 60)}…
              </Link>
            ),
          )}
        </div>
      )}
    </div>
  );
}
