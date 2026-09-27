import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { api, type ChunkFact } from "../api";
import { S } from "../i18n";
import { useKbId } from "../kb";
import { Pager, pageSlice } from "../ui";
import { SourcesRail } from "./SourcesRail";

const DOC_PAGE = 12;

const ym = (iso: string | null) => (iso ? iso.slice(0, 7) : null);

function factRange(f: ChunkFact): string | null {
  if (!f.valid_from && !f.valid_to) return null;
  return `${ym(f.valid_from) ?? "…"} → ${ym(f.valid_to) ?? S.doc.ongoing}`;
}

export function DocViewer() {
  const kbId = useKbId();
  const { docId } = useParams({ from: "/app/kb/$kbId/doc/$docId" });
  const { chunk } = useSearch({ from: "/app/kb/$kbId/doc/$docId" });
  const navigate = useNavigate();

  const detail = useQuery({
    queryKey: ["docDetail", docId],
    queryFn: () => api.documentDetail(docId),
  });
  // The evidence chain in reverse: the facts extracted out of each chunk (fetched for the whole
  // document in one go, grouped by chunk)
  const extractions = useQuery({
    queryKey: ["docExtractions", docId],
    queryFn: () => api.documentExtractions(docId),
  });
  const factsByChunk = useMemo(() => {
    const map = new Map<string, ChunkFact[]>();
    for (const f of extractions.data?.facts ?? []) {
      if (!map.has(f.chunk_id)) map.set(f.chunk_id, []);
      map.get(f.chunk_id)!.push(f);
    }
    return map;
  }, [extractions.data]);

  // null = locate automatically: on a citation jump, turn to the page the target chunk sits on
  const [page, setPage] = useState<number | null>(null);
  useEffect(() => setPage(null), [chunk, docId]);

  const highlightRef = useRef<HTMLDivElement>(null);
  // Citation jump: scroll to the target chunk and light it up for a moment, then fade back to the
  // normal state (the highlight does not stay put)
  const [flash, setFlash] = useState(false);
  useEffect(() => {
    if (detail.data && highlightRef.current) {
      highlightRef.current.scrollIntoView({ behavior: "smooth", block: "center" });
      setFlash(true);
      // Smooth scrolling itself eats a few hundred milliseconds, so the lit-up window has to
      // leave enough visible time after the scrolling is done
      const t = setTimeout(() => setFlash(false), 2600);
      return () => clearTimeout(t);
    }
  }, [detail.data, chunk]);

  if (detail.isPending)
    return <div className="p-8 text-sm text-neutral-500">{S.doc.loading}</div>;
  if (detail.isError)
    return (
      <div className="p-8 text-sm text-rose-400">
        {(detail.error as Error).message}
      </div>
    );

  const { document: doc, chunks } = detail.data;
  const targetIdx = chunk ? chunks.findIndex((c) => c.id === chunk) : -1;
  const curPage = page ?? (targetIdx >= 0 ? Math.floor(targetIdx / DOC_PAGE) : 0);
  const { rows: pagedChunks, safe: safePage } = pageSlice(chunks, curPage, DOC_PAGE);

  return (
    <div className="h-full flex">
      {/* The sources rail is always present: the folder the current document belongs to is
          highlighted, and clicking another folder jumps back to the matching Library view */}
      <SourcesRail
        kbId={doc.kb_id}
        active={doc.source_id ?? "uploads"}
        onSelect={(sel) => navigate({ to: "/kb/$kbId/library", params: { kbId }, search: { src: sel } })}
      />
      <div className="flex-1 min-w-0 overflow-y-auto u-scroll">
        <div className="max-w-4xl mx-auto p-6">
        <div className="mb-4 flex items-baseline justify-between gap-4">
          <div>
            <h2 className="text-lg font-bold text-neutral-100 break-all">{doc.filename}</h2>
            <p className="mt-0.5 text-xs text-neutral-500">
              {chunks.length} {S.doc.sections} · {(doc.size_bytes / 1024).toFixed(0)} KB ·{" "}
              {new Date(doc.created_at).toLocaleDateString()}
            </p>
          </div>
          <Link to="/kb/$kbId/library" params={{ kbId }} className="shrink-0 text-sm text-[var(--u-accent)] hover:underline">
            {S.doc.backToLibrary}
          </Link>
        </div>

        <div className="space-y-3">
          {pagedChunks.map((c) => {
            const hit = c.id === chunk;
            const facts = factsByChunk.get(c.id) ?? [];
            return (
              <div key={c.id} ref={hit ? highlightRef : undefined} className="flex gap-3">
                <div
                  className={`flex-1 min-w-0 rounded-xl border p-4 text-sm leading-relaxed whitespace-pre-wrap border-white/10 transition-[border-color,background-color,box-shadow] duration-700 ${
                    hit && flash ? "u-flash" : "bg-white/[0.04]"
                  }`}
                >
                  <div className="mb-1.5 text-xs text-neutral-500">
                    {S.doc.section} {c.seq + 1}
                    {hit && (
                      <span
                        className={`ml-2 text-[var(--u-accent)] transition-opacity duration-700 ${
                          flash ? "opacity-100" : "opacity-0"
                        }`}
                      >
                        {S.doc.citedHere}
                      </span>
                    )}
                  </div>
                  {c.text}
                </div>

                {/* Extraction cross-check rail: which facts this chunk produced (entities
                    jump to the graph) */}
                {facts.length > 0 && (
                  <aside className="w-64 shrink-0 rounded-xl border border-white/10 bg-white/[0.02] p-3">
                    <div className="mb-2 text-[10px] font-medium uppercase tracking-[0.08em] text-neutral-600">
                      {S.doc.extracted} · {facts.length}
                    </div>
                    <div className="space-y-2">
                      {facts.map((f) => {
                        const range = factRange(f);
                        return (
                          <div key={f.fact_id} className="text-xs leading-snug">
                            <div>
                              <Link
                                to="/kb/$kbId/graph"
                                params={{ kbId }}
                                search={{ entity: f.subject_id }}
                                className="text-neutral-200 hover:text-white hover:underline underline-offset-2 decoration-white/30"
                              >
                                {f.subject}
                              </Link>
                              <span
                                className={
                                  f.predicate === null
                                    ? "italic text-neutral-700"
                                    : "text-neutral-600"
                                }
                                title={
                                  f.predicate && f.inferred
                                    ? S.graph.inferredPredicate
                                    : undefined
                                }
                              >
                                {" "}
                                {f.predicate ?? S.graph.unknownPredicate}{" "}
                              </span>
                              {f.object_id ? (
                                <Link
                                  to="/kb/$kbId/graph"
                                  params={{ kbId }}
                                  search={{ entity: f.object_id }}
                                  className="text-neutral-200 hover:text-white hover:underline underline-offset-2 decoration-white/30"
                                >
                                  {f.object}
                                </Link>
                              ) : (
                                <span className="text-neutral-300">{f.object ?? ""}</span>
                              )}
                            </div>
                            {range && <div className="u-num text-[10.5px] text-neutral-500">{range}</div>}
                          </div>
                        );
                      })}
                    </div>
                  </aside>
                )}
              </div>
            );
          })}
        </div>
          <Pager total={chunks.length} pageSize={DOC_PAGE} page={safePage} onPage={setPage} />
        </div>
      </div>
    </div>
  );
}
