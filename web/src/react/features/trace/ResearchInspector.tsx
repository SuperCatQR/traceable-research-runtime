import { ChevronRight, LoaderCircle, RefreshCw, X } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import type { ResearchConversationDetail } from "../../../research-workspace-client";
import {
  errorMessage,
  useTraceAuditQuery,
  useTraceSummaryQuery,
} from "../../data/workspace-queries";
import { ScrollIndicator } from "../../shared/ScrollIndicator";
import { formatTraceTimestamp, safeEvidenceUrl } from "../../shared/format";
import { statusLabels } from "../research/status-labels";

export type InspectorView = "summary" | "audit";

interface ResearchInspectorProps {
  accountId: string;
  conversation: ResearchConversationDetail;
  open: boolean;
  selectedTurnId?: string;
  view: InspectorView;
  stage: string;
  onClose(): void;
  onSelectTurn(turnId: string): void;
  onChangeView(view: InspectorView): void;
  onChangeStage(stage: string): void;
  onAuthenticationError(error: unknown): void;
}

export function ResearchInspector({
  accountId,
  conversation,
  open,
  selectedTurnId,
  view,
  stage,
  onClose,
  onSelectTurn,
  onChangeView,
  onChangeStage,
  onAuthenticationError,
}: ResearchInspectorProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const selectedTurn = conversation.turns.find((turn) => turn.turn_id === selectedTurnId)
    ?? conversation.turns.at(-1);
  const summary = useTraceSummaryQuery(
    accountId,
    conversation.conversation_id,
    selectedTurn?.turn_id,
    open && view === "summary",
    selectedTurn?.updated_at,
  );
  const audit = useTraceAuditQuery(
    accountId,
    conversation.conversation_id,
    selectedTurn?.turn_id,
    stage,
    open && view === "audit",
  );
  const entries = useMemo(() => audit.data?.pages.flatMap((page) => page.entries) ?? [], [audit.data]);
  const requestError = view === "summary" ? summary.error : audit.error;

  useEffect(() => {
    if (requestError) onAuthenticationError(requestError);
  }, [onAuthenticationError, requestError]);

  if (!open || !selectedTurn) return null;
  const pending = view === "summary" ? summary.isPending : audit.isPending;
  const refresh = () => view === "summary" ? summary.refetch() : audit.refetch();

  return (
    <aside className="research-inspector inspector scroll-indicator-host" aria-label="研究检查器">
      <header className="research-inspector-header inspector-header">
        <div><span>TRACE VIEW</span><h2>研究过程</h2></div>
        <button className="icon-button" type="button" onClick={onClose} aria-label="关闭研究过程" title="关闭"><X /></button>
      </header>
      <div className="inspector-turn-selector">
        <label className="sr-only" htmlFor="research-inspector-turn">选择研究轮次</label>
        <select id="research-inspector-turn" value={selectedTurn.turn_id} onChange={(event) => onSelectTurn(event.target.value)}>
          {conversation.turns.map((turn) => (
            <option key={turn.turn_id} value={turn.turn_id}>第 {turn.turn_number} 轮 · {statusLabels[turn.status]}</option>
          ))}
        </select>
      </div>
      <div className="inspector-tabs" role="tablist" aria-label="研究记录层级">
        <button type="button" role="tab" aria-selected={view === "summary"} onClick={() => onChangeView("summary")}>概览</button>
        <button type="button" role="tab" aria-selected={view === "audit"} onClick={() => onChangeView("audit")}>审计详情</button>
      </div>
      <div ref={scrollRef} id="research-inspector-scroll" className="research-inspector-content inspector-scroll">
        {pending ? (
          <div className="inspector-status"><LoaderCircle className="spin" /><span>正在读取研究记录</span></div>
        ) : requestError ? (
          <div className="inspector-error">
            <p>{errorMessage(requestError, "无法读取研究记录")}</p>
            <button className="icon-button" type="button" onClick={() => void refresh()} aria-label="重试" title="重试"><RefreshCw /></button>
          </div>
        ) : view === "summary" ? (
          <TraceSummary summary={summary.data} />
        ) : (
          <TraceAudit
            entries={entries}
            stage={stage}
            hasMore={audit.hasNextPage}
            loadingMore={audit.isFetchingNextPage}
            onChangeStage={onChangeStage}
            onLoadMore={() => void audit.fetchNextPage()}
          />
        )}
      </div>
      <ScrollIndicator scrollerRef={scrollRef} />
    </aside>
  );
}

function TraceSummary({ summary }: { summary: ReturnType<typeof useTraceSummaryQuery>["data"] }) {
  if (!summary) return <div className="inspector-status"><span>当前轮次尚无可展示的研究概览。</span></div>;
  return (
    <>
      <section className="inspector-section inspector-model"><h3>本轮模型</h3><code>{summary.model_id}</code></section>
      {summary.understanding && (
        <section className="inspector-section"><h3>问题理解</h3><p>{summary.understanding.message}</p><small>{summary.understanding.rationale}</small></section>
      )}
      {summary.rounds.length > 0 && (
        <section className="inspector-section">
          <h3>检索覆盖</h3>
          {summary.rounds.map((round) => (
            <div className="trace-round" key={round.round}>
              <strong>第 {round.round} 轮</strong><span>{round.search_result_count} 条导航结果</span>
              <ul>{round.directions.map((direction) => <li key={direction}>{direction}</li>)}</ul>
            </div>
          ))}
          <small>已归档 {summary.archived_source_count} 个来源{summary.skipped_source_count ? `，跳过 ${summary.skipped_source_count} 个` : ""}</small>
        </section>
      )}
      {summary.selected_sources.length > 0 && (
        <section className="inspector-section"><h3>主要来源</h3><ul className="inspector-source-list">
          {summary.selected_sources.map((source, index) => {
            const safe = safeEvidenceUrl(source.url);
            return <li key={`${source.url}-${index}`}>{safe ? <a href={safe} target="_blank" rel="noreferrer">{source.title}</a> : <span>{source.title}</span>}<small>{source.rationale}</small></li>;
          })}
        </ul></section>
      )}
      {summary.synthesis_rationale && <section className="inspector-section"><h3>结论综合</h3><p>{summary.synthesis_rationale}</p></section>}
      {summary.failure && <section className="inspector-section inspector-failure"><h3>运行状态</h3><p>{summary.failure.stage}：{summary.failure.message}</p></section>}
    </>
  );
}

const stages = [
  ["", "全部"],
  ["dialogue", "理解"],
  ["setup", "准备"],
  ["planning", "规划"],
  ["search", "搜索"],
  ["archive", "归档"],
  ["selection", "选源"],
  ["synthesis", "结论"],
  ["failure", "失败"],
] as const;

function TraceAudit({
  entries,
  stage,
  hasMore,
  loadingMore,
  onChangeStage,
  onLoadMore,
}: {
  entries: NonNullable<ReturnType<typeof useTraceAuditQuery>["data"]>["pages"][number]["entries"];
  stage: string;
  hasMore: boolean;
  loadingMore: boolean;
  onChangeStage(stage: string): void;
  onLoadMore(): void;
}) {
  return (
    <>
      <label className="audit-filter"><span>阶段</span><select value={stage} onChange={(event) => onChangeStage(event.target.value)}>
        {stages.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select></label>
      {entries.length ? (
        <ol className="audit-entry-list">
          {entries.map((entry, index) => {
            const metadata = [
              entry.sequence === null ? null : `#${entry.sequence}`,
              entry.occurred_at ? formatTraceTimestamp(entry.occurred_at) : null,
            ].filter(Boolean).join(" · ");
            return <li key={`${entry.sequence ?? "dialogue"}-${index}`}>
              {metadata && <time>{metadata}</time>}<span>{entry.label}</span><p>{entry.detail}</p>{entry.rationale && <small>{entry.rationale}</small>}
            </li>;
          })}
        </ol>
      ) : <div className="inspector-status"><span>这个筛选条件下没有审计事件。</span></div>}
      {hasMore && <button className="icon-button audit-more" type="button" onClick={onLoadMore} disabled={loadingMore} aria-label="加载更多审计记录" title="加载更多">{loadingMore ? <LoaderCircle className="spin" /> : <ChevronRight />}</button>}
    </>
  );
}
