import { useMemo, useState } from "react";
import {
  AlertTriangle,
  ArrowRight,
  BookOpenCheck,
  BrainCircuit,
  Check,
  CheckCircle2,
  ChevronDown,
  Circle,
  CircleAlert,
  ClipboardList,
  Columns2,
  FileCheck2,
  FileText,
  Fingerprint,
  Layers3,
  Link2,
  LockKeyhole,
  Network,
  PanelRightClose,
  PanelRightOpen,
  Quote,
  Route,
  ScrollText,
  ShieldCheck,
} from "lucide-react";
import type {
  AnswerBlock,
  AnswerMode,
  Citation,
  ResearchAnswer,
  ResearchPhase,
  ResearchRequest,
  WorkspaceView,
} from "../types";
import { formatDateTime } from "../format";

const viewOptions: Array<{
  id: WorkspaceView;
  label: string;
  icon: typeof BookOpenCheck;
}> = [
  { id: "answer", label: "答案", icon: BookOpenCheck },
  { id: "execution", label: "执行", icon: Route },
  { id: "audit", label: "审计", icon: ScrollText },
];

const modeLabels: Record<AnswerMode, string> = {
  "evidence-first": "证据优先",
  "model-led": "综合解读",
};

interface ResearchWorkspaceProps {
  request: ResearchRequest;
  activeView: WorkspaceView;
  answerMode: AnswerMode;
  compareAnswers: boolean;
  selectedCitationId: string | null;
  sourceLedgerOpen: boolean;
  onViewChange: (view: WorkspaceView) => void;
  onAnswerModeChange: (mode: AnswerMode) => void;
  onCompareChange: (compare: boolean) => void;
  onCitationSelect: (citationId: string) => void;
  onSourceLedgerOpenChange: (open: boolean) => void;
}

export function ResearchWorkspace({
  request,
  activeView,
  answerMode,
  compareAnswers,
  selectedCitationId,
  sourceLedgerOpen,
  onViewChange,
  onAnswerModeChange,
  onCompareChange,
  onCitationSelect,
  onSourceLedgerOpenChange,
}: ResearchWorkspaceProps) {
  const selectedCitation = useMemo(
    () =>
      request.citations.find((citation) => citation.id === selectedCitationId) ??
      request.citations[0] ??
      null,
    [request.citations, selectedCitationId],
  );

  return (
    <div className="request-workspace">
      <RequestHeader request={request} />
      <RequestNavigation activeView={activeView} onViewChange={onViewChange} />
      <div
        className={`workspace-body${
          activeView === "answer" && sourceLedgerOpen ? " has-source-ledger" : ""
        }`}
      >
        <div className="workspace-primary">
          {activeView === "answer" ? (
            <AnswerView
              request={request}
              answerMode={answerMode}
              compareAnswers={compareAnswers}
              selectedCitationId={selectedCitation?.id ?? null}
              sourceLedgerOpen={sourceLedgerOpen}
              onAnswerModeChange={onAnswerModeChange}
              onCompareChange={onCompareChange}
              onCitationSelect={(citationId) => {
                onCitationSelect(citationId);
                onSourceLedgerOpenChange(true);
              }}
              onOpenSourceLedger={() => onSourceLedgerOpenChange(true)}
            />
          ) : null}
          {activeView === "execution" ? <ExecutionView request={request} /> : null}
          {activeView === "audit" ? <AuditView request={request} /> : null}
        </div>
        {activeView === "answer" && sourceLedgerOpen ? (
          <SourceLedger
            citation={selectedCitation}
            onClose={() => onSourceLedgerOpenChange(false)}
            onOpenAudit={() => {
              onSourceLedgerOpenChange(false);
              onViewChange("audit");
            }}
          />
        ) : null}
      </div>
    </div>
  );
}

function RequestHeader({ request }: { request: ResearchRequest }) {
  return (
    <header className="request-header">
      <div className="request-heading-copy">
        <div className="request-kicker">
          <span className={`request-status status-${request.status}`}>
            <CheckCircle2 aria-hidden="true" size={14} />
            {request.statusLabel}
          </span>
          <span>请求 #{request.number}</span>
          <span>{formatDateTime(request.updatedAt)} 更新</span>
        </div>
        <h1>{request.shortTitle}</h1>
        <p>{request.clarifiedQuestion}</p>
      </div>
      <div className="locked-contract">
        <LockKeyhole aria-hidden="true" size={17} />
        <div>
          <span>本次锁定版本</span>
          <strong>
            {request.snapshot.name} · {request.snapshot.versionLabel}
          </strong>
        </div>
        <code>{request.snapshot.contentHash.slice(0, 18)}…</code>
      </div>
    </header>
  );
}

function RequestNavigation({
  activeView,
  onViewChange,
}: {
  activeView: WorkspaceView;
  onViewChange: (view: WorkspaceView) => void;
}) {
  return (
    <nav className="request-navigation" aria-label="研究视图">
      {viewOptions.map((option) => {
        const Icon = option.icon;
        return (
          <button
            key={option.id}
            type="button"
            className={activeView === option.id ? "is-active" : ""}
            aria-current={activeView === option.id ? "page" : undefined}
            onClick={() => onViewChange(option.id)}
          >
            <Icon aria-hidden="true" size={16} />
            {option.label}
          </button>
        );
      })}
    </nav>
  );
}

interface AnswerViewProps {
  request: ResearchRequest;
  answerMode: AnswerMode;
  compareAnswers: boolean;
  selectedCitationId: string | null;
  sourceLedgerOpen: boolean;
  onAnswerModeChange: (mode: AnswerMode) => void;
  onCompareChange: (compare: boolean) => void;
  onCitationSelect: (citationId: string) => void;
  onOpenSourceLedger: () => void;
}

function AnswerView({
  request,
  answerMode,
  compareAnswers,
  selectedCitationId,
  sourceLedgerOpen,
  onAnswerModeChange,
  onCompareChange,
  onCitationSelect,
  onOpenSourceLedger,
}: AnswerViewProps) {
  const selectedAnswer =
    request.answers.find((answer) => answer.mode === answerMode) ?? request.answers[0];
  const answers = compareAnswers ? request.answers : selectedAnswer ? [selectedAnswer] : [];

  return (
    <section className="answer-view" aria-labelledby="answer-view-heading">
      <div className="answer-toolbar">
        <div>
          <span className="section-eyebrow">同一次研究 · 共享证据与结论</span>
          <h2 id="answer-view-heading">研究答案</h2>
        </div>
        <div className="answer-controls">
          <div className="mode-control" aria-label="回答方式">
            {request.requestedModes.map((mode) => (
              <button
                key={mode}
                type="button"
                className={!compareAnswers && answerMode === mode ? "is-selected" : ""}
                aria-pressed={!compareAnswers && answerMode === mode}
                onClick={() => {
                  onCompareChange(false);
                  onAnswerModeChange(mode);
                }}
              >
                {modeLabels[mode]}
              </button>
            ))}
          </div>
          <button
            className={`secondary-command compare-command${compareAnswers ? " is-active" : ""}`}
            type="button"
            aria-pressed={compareAnswers}
            onClick={() => onCompareChange(!compareAnswers)}
          >
            <Columns2 aria-hidden="true" size={16} />
            对照
          </button>
          {!sourceLedgerOpen ? (
            <button
              className="icon-button"
              type="button"
              aria-label="打开来源账页"
              title="打开来源账页"
              onClick={onOpenSourceLedger}
            >
              <PanelRightOpen aria-hidden="true" size={18} />
            </button>
          ) : null}
        </div>
      </div>

      <div className={`answer-columns${compareAnswers ? " is-comparing" : ""}`}>
        {answers.map((answer) => (
          <AnswerComposition
            key={answer.mode}
            answer={answer}
            selectedCitationId={selectedCitationId}
            onCitationSelect={onCitationSelect}
          />
        ))}
      </div>

      {request.coverageGaps.length > 0 ? (
        <section className="coverage-gap-band" aria-labelledby="coverage-gap-title">
          <div className="gap-heading">
            <AlertTriangle aria-hidden="true" size={20} />
            <div>
              <span className="section-eyebrow">LIMITED RESULT</span>
              <h3 id="coverage-gap-title">仍有 {request.coverageGaps.length} 个高优先级缺口</h3>
            </div>
          </div>
          {request.coverageGaps.map((gap) => (
            <div className="gap-detail" key={gap.id}>
              <strong>{gap.question}</strong>
              <p>{gap.explanation}</p>
              <span>{gap.status === "disclosed" ? "已在答案中披露" : "尚未解决"}</span>
            </div>
          ))}
        </section>
      ) : null}
    </section>
  );
}

function AnswerComposition({
  answer,
  selectedCitationId,
  onCitationSelect,
}: {
  answer: ResearchAnswer;
  selectedCitationId: string | null;
  onCitationSelect: (citationId: string) => void;
}) {
  return (
    <article className="answer-composition">
      <header className="composition-heading">
        <div>
          {answer.mode === "evidence-first" ? (
            <BookOpenCheck aria-hidden="true" size={17} />
          ) : (
            <BrainCircuit aria-hidden="true" size={17} />
          )}
          <span>{modeLabels[answer.mode]}</span>
        </div>
        <p>{answer.summary}</p>
      </header>
      <div className="bound-answer">
        {answer.blocks.map((block, index) => (
          <AnswerSourceBlock
            key={block.id}
            block={block}
            index={index + 1}
            selectedCitationId={selectedCitationId}
            onCitationSelect={onCitationSelect}
          />
        ))}
      </div>
    </article>
  );
}

function AnswerSourceBlock({
  block,
  index,
  selectedCitationId,
  onCitationSelect,
}: {
  block: AnswerBlock;
  index: number;
  selectedCitationId: string | null;
  onCitationSelect: (citationId: string) => void;
}) {
  const sourceLabel =
    block.kind === "evidence"
      ? "文档关联结论"
      : block.kind === "model"
        ? "模型补充"
        : "文档结论 + 模型补充";

  return (
    <section className={`answer-source-block source-${block.kind}`}>
      <div className="binding-rail" aria-hidden="true">
        <span>{String(index).padStart(2, "0")}</span>
      </div>
      <div className="answer-block-content">
        <div className="source-label-row">
          <span className={`source-kind-label source-${block.kind}`}>
            {block.kind === "evidence" ? (
              <FileCheck2 aria-hidden="true" size={13} />
            ) : block.kind === "model" ? (
              <BrainCircuit aria-hidden="true" size={13} />
            ) : (
              <Layers3 aria-hidden="true" size={13} />
            )}
            {sourceLabel}
          </span>
          <span className="claim-label">{block.label}</span>
        </div>
        <p>{block.text}</p>
        {block.modelNotice ? (
          <div className="model-notice">
            <CircleAlert aria-hidden="true" size={14} />
            {block.modelNotice}
          </div>
        ) : null}
        {block.citationIds.length > 0 ? (
          <div className="citation-links" aria-label="支持引用">
            {block.citationIds.map((citationId, citationIndex) => (
              <button
                key={citationId}
                type="button"
                className={selectedCitationId === citationId ? "is-selected" : ""}
                aria-pressed={selectedCitationId === citationId}
                onClick={() => onCitationSelect(citationId)}
              >
                <Link2 aria-hidden="true" size={13} />
                引用 {citationIndex + 1}
                <ArrowRight aria-hidden="true" size={13} />
              </button>
            ))}
          </div>
        ) : null}
      </div>
    </section>
  );
}

function SourceLedger({
  citation,
  onClose,
  onOpenAudit,
}: {
  citation: Citation | null;
  onClose: () => void;
  onOpenAudit: () => void;
}) {
  return (
    <aside className="source-ledger" aria-label="来源账页">
      <header className="source-ledger-header">
        <div>
          <span className="section-eyebrow">SOURCE LEDGER</span>
          <h2>逐字来源</h2>
        </div>
        <button
          className="icon-button"
          type="button"
          aria-label="关闭来源账页"
          title="关闭来源账页"
          onClick={onClose}
        >
          <PanelRightClose aria-hidden="true" size={18} />
        </button>
      </header>
      {citation ? (
        <div className="source-ledger-content">
          <div className="verification-status">
            <ShieldCheck aria-hidden="true" size={17} />
            <div>
              <strong>逐字引文已校验</strong>
              <span>不代表结论语义已被程序证明</span>
            </div>
          </div>
          <div className="ledger-claim">
            <span>关联 Claim</span>
            <code>{citation.claimId}</code>
          </div>
          <blockquote>
            <Quote aria-hidden="true" size={19} />
            <p>{citation.quote}</p>
          </blockquote>
          <dl className="source-metadata">
            <div>
              <dt>文档</dt>
              <dd>{citation.documentTitle}</dd>
            </div>
            <div>
              <dt>章节</dt>
              <dd>{citation.sectionHeading}</dd>
            </div>
            <div>
              <dt>文档 ID</dt>
              <dd>
                <code>{citation.documentId}</code>
              </dd>
            </div>
            <div>
              <dt>版本 hash</dt>
              <dd>
                <code>{citation.versionHash}</code>
              </dd>
            </div>
            <div>
              <dt>审计序号</dt>
              <dd>
                <code>#{citation.traceSequence}</code>
              </dd>
            </div>
          </dl>
          <button className="secondary-command ledger-audit-link" type="button" onClick={onOpenAudit}>
            <Fingerprint aria-hidden="true" size={15} />
            定位执行记录
          </button>
        </div>
      ) : (
        <div className="source-ledger-empty">
          <Quote aria-hidden="true" size={24} />
          <strong>尚未选择引用</strong>
          <p>选择答案中的引用后，这里显示逐字来源。</p>
        </div>
      )}
    </aside>
  );
}

function ExecutionView({ request }: { request: ResearchRequest }) {
  return (
    <section className="execution-view" aria-labelledby="execution-view-title">
      <header className="section-heading-row">
        <div>
          <span className="section-eyebrow">EXECUTION OVERVIEW</span>
          <h2 id="execution-view-title">研究执行</h2>
        </div>
        <span className="stop-reason">
          <CheckCircle2 aria-hidden="true" size={15} />
          {request.stopReason}
        </span>
      </header>

      <div className="metric-strip" aria-label="研究计数">
        <Metric label="导航方向" value={request.counts.navigationBranches} icon={Network} />
        <Metric label="选中文档" value={request.counts.selectedDocuments} icon={FileText} />
        <Metric label="正文读取" value={request.counts.segmentReads} icon={BookOpenCheck} />
        <Metric label="逐字证据" value={request.counts.acceptedEvidence} icon={ShieldCheck} />
      </div>

      <div className="execution-columns">
        <section className="phase-ledger" aria-labelledby="phase-ledger-title">
          <h3 id="phase-ledger-title">执行阶段</h3>
          <ol>
            {request.phases.map((phase, index) => (
              <PhaseRow key={phase.id} phase={phase} index={index + 1} />
            ))}
          </ol>
        </section>
        <section className="scope-summary" aria-labelledby="scope-summary-title">
          <h3 id="scope-summary-title">研究范围</h3>
          <div className="scope-group">
            <span>已选导航方向</span>
            <ul>
              {request.selectedNavigationLabels.map((label) => (
                <li key={label}>
                  <Network aria-hidden="true" size={14} />
                  {label}
                </li>
              ))}
            </ul>
          </div>
          <div className="scope-group">
            <span>已选文档</span>
            <ul>
              {request.selectedDocumentTitles.map((title) => (
                <li key={title}>
                  <FileText aria-hidden="true" size={14} />
                  {title}
                </li>
              ))}
            </ul>
          </div>
        </section>
      </div>
    </section>
  );
}

function Metric({
  label,
  value,
  icon: Icon,
}: {
  label: string;
  value: number;
  icon: typeof Network;
}) {
  return (
    <div className="metric-item">
      <Icon aria-hidden="true" size={16} />
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function PhaseRow({ phase, index }: { phase: ResearchPhase; index: number }) {
  return (
    <li className={`phase-row phase-${phase.status}`}>
      <div className="phase-marker">
        {phase.status === "complete" ? (
          <Check aria-hidden="true" size={14} />
        ) : phase.status === "attention" ? (
          <AlertTriangle aria-hidden="true" size={14} />
        ) : (
          <Circle aria-hidden="true" size={11} />
        )}
      </div>
      <div>
        <span>{String(index).padStart(2, "0")}</span>
        <strong>{phase.label}</strong>
        <p>{phase.detail}</p>
      </div>
    </li>
  );
}

function AuditView({ request }: { request: ResearchRequest }) {
  const [visibleCount, setVisibleCount] = useState(8);
  const visibleItems = request.audit.slice(0, visibleCount);
  const hasMore = visibleCount < request.audit.length;

  return (
    <section className="audit-view" aria-labelledby="audit-view-title">
      <header className="section-heading-row">
        <div>
          <span className="section-eyebrow">DETAILED AUDIT</span>
          <h2 id="audit-view-title">执行记录</h2>
        </div>
        <div className="audit-safety-label">
          <ShieldCheck aria-hidden="true" size={15} />
          仅显示安全摘要
        </div>
      </header>
      <div className="audit-column-labels" aria-hidden="true">
        <span>序号 / 时间</span>
        <span>事件摘要</span>
      </div>
      <ol className="audit-list">
        {visibleItems.map((item) => (
          <li key={item.sequence}>
            <div className="audit-sequence">
              <code>#{String(item.sequence).padStart(3, "0")}</code>
              <time>{item.time}</time>
            </div>
            <div className="audit-event">
              <span>{item.eventType}</span>
              <p>{item.summary}</p>
            </div>
          </li>
        ))}
      </ol>
      {hasMore ? (
        <button
          className="secondary-command load-more"
          type="button"
          onClick={() => setVisibleCount((count) => count + 6)}
        >
          <ChevronDown aria-hidden="true" size={16} />
          加载更多
        </button>
      ) : (
        <div className="audit-end">
          <ClipboardList aria-hidden="true" size={15} />
          已显示本次执行的全部安全记录
        </div>
      )}
    </section>
  );
}
