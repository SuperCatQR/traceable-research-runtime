import { useState } from "react";
import {
  ArrowLeft,
  ArrowRight,
  BookOpenCheck,
  Check,
  CheckCircle2,
  Circle,
  FileText,
  LockKeyhole,
  MessageSquareText,
  Play,
  ShieldCheck,
} from "lucide-react";
import type { AnswerMode, CorpusSnapshot } from "../types";
import { formatDate } from "../format";

type FirstUseStep = "snapshot" | "question" | "clarification" | "ready" | "running";

const steps: Array<{ id: FirstUseStep; label: string }> = [
  { id: "snapshot", label: "资料版本" },
  { id: "question", label: "研究问题" },
  { id: "clarification", label: "问题确认" },
  { id: "ready", label: "冻结输入" },
  { id: "running", label: "开始研究" },
];

interface FirstUseWorkspaceProps {
  snapshots: CorpusSnapshot[];
  onOpenCompletedResearch: () => void;
}

export function FirstUseWorkspace({
  snapshots,
  onOpenCompletedResearch,
}: FirstUseWorkspaceProps) {
  const availableSnapshots = snapshots.filter((snapshot) => snapshot.availability === "available");
  const [step, setStep] = useState<FirstUseStep>("snapshot");
  const [snapshotId, setSnapshotId] = useState(availableSnapshots[0]?.id ?? "");
  const [question, setQuestion] = useState(
    "2026 年在上海解除劳动合同时，经济补偿的月工资基数如何计算，是否有封顶？",
  );
  const [modes, setModes] = useState<AnswerMode[]>(["evidence-first", "model-led"]);
  const [clarification, setClarification] = useState(
    "地点为上海；劳动关系预计在 2026 年解除；希望同时说明计算口径和资料缺口。",
  );
  const selectedSnapshot = snapshots.find((snapshot) => snapshot.id === snapshotId);
  const currentStepIndex = steps.findIndex((item) => item.id === step);

  function toggleMode(mode: AnswerMode) {
    setModes((current) =>
      current.includes(mode)
        ? current.length > 1
          ? current.filter((item) => item !== mode)
          : current
        : [...current, mode],
    );
  }

  return (
    <div className="first-use-workspace">
      <header className="first-use-header">
        <div>
          <span className="section-eyebrow">NEW RESEARCH</span>
          <h1>建立一项可复核的研究</h1>
          <p>选择已有资料版本，明确问题，再冻结本次研究输入。</p>
        </div>
        <span className="local-only-badge">
          <ShieldCheck aria-hidden="true" size={15} />
          本地演示数据
        </span>
      </header>

      <ol className="first-use-steps" aria-label="新建研究步骤">
        {steps.map((item, index) => {
          const complete = index < currentStepIndex;
          const current = index === currentStepIndex;
          return (
            <li className={`${complete ? "is-complete" : ""}${current ? " is-current" : ""}`} key={item.id}>
              <span aria-hidden="true">
                {complete ? <Check size={14} /> : current ? <Circle size={11} /> : index + 1}
              </span>
              {item.label}
            </li>
          );
        })}
      </ol>

      <main className="first-use-main">
        {step === "snapshot" ? (
          <section className="onboarding-panel" aria-labelledby="snapshot-step-title">
            <div className="onboarding-heading">
              <span>步骤 1 / 5</span>
              <h2 id="snapshot-step-title">选择 Markdown Corpus Snapshot</h2>
              <p>Snapshot 会在研究开始后锁定；已有 Request 不会跟随默认版本变化。</p>
            </div>
            <div className="snapshot-options" role="radiogroup" aria-label="资料版本">
              {snapshots.map((snapshot) => {
                const disabled = snapshot.availability === "unavailable";
                return (
                  <label className={`snapshot-option${disabled ? " is-unavailable" : ""}`} key={snapshot.id}>
                    <input
                      type="radio"
                      name="snapshot"
                      value={snapshot.id}
                      checked={snapshotId === snapshot.id}
                      disabled={disabled}
                      onChange={() => setSnapshotId(snapshot.id)}
                    />
                    <span className="snapshot-radio" aria-hidden="true" />
                    <span className="snapshot-option-copy">
                      <strong>{snapshot.name}</strong>
                      <span>
                        {snapshot.versionLabel} · {snapshot.documentCount} 篇文档 · {formatDate(snapshot.publishedAt)} 发布
                      </span>
                      <code>{snapshot.contentHash.slice(0, 28)}…</code>
                    </span>
                    <span className={`availability availability-${snapshot.availability}`}>
                      {disabled ? "不可用" : "可用"}
                    </span>
                  </label>
                );
              })}
            </div>
            <div className="onboarding-actions align-end">
              <button
                className="primary-command"
                type="button"
                disabled={!selectedSnapshot}
                onClick={() => setStep("question")}
              >
                下一步
                <ArrowRight aria-hidden="true" size={16} />
              </button>
            </div>
          </section>
        ) : null}

        {step === "question" ? (
          <section className="onboarding-panel" aria-labelledby="question-step-title">
            <div className="onboarding-heading">
              <span>步骤 2 / 5</span>
              <h2 id="question-step-title">提出研究问题</h2>
              <p>回答方式可同时选择；两种表达会共享同一次研究的证据和结论。</p>
            </div>
            <label className="field-label" htmlFor="research-question">
              研究问题
            </label>
            <textarea
              id="research-question"
              value={question}
              maxLength={1200}
              onChange={(event) => setQuestion(event.target.value)}
            />
            <div className="character-count">{question.length} / 1200</div>
            <fieldset className="answer-mode-fieldset">
              <legend>回答方式</legend>
              <label>
                <input
                  type="checkbox"
                  checked={modes.includes("evidence-first")}
                  onChange={() => toggleMode("evidence-first")}
                />
                <span>
                  <BookOpenCheck aria-hidden="true" size={17} />
                  <strong>证据优先</strong>
                  <small>以文档关联结论为主体</small>
                </span>
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={modes.includes("model-led")}
                  onChange={() => toggleMode("model-led")}
                />
                <span>
                  <MessageSquareText aria-hidden="true" size={17} />
                  <strong>综合解读</strong>
                  <small>以模型知识为骨架并由文档纠正</small>
                </span>
              </label>
            </fieldset>
            <div className="onboarding-actions">
              <button className="secondary-command" type="button" onClick={() => setStep("snapshot")}>
                <ArrowLeft aria-hidden="true" size={16} />
                上一步
              </button>
              <button
                className="primary-command"
                type="button"
                disabled={question.trim().length < 12}
                onClick={() => setStep("clarification")}
              >
                评估问题
                <ArrowRight aria-hidden="true" size={16} />
              </button>
            </div>
          </section>
        ) : null}

        {step === "clarification" ? (
          <section className="onboarding-panel clarification-panel" aria-labelledby="clarification-step-title">
            <div className="onboarding-heading">
              <span>步骤 3 / 5</span>
              <h2 id="clarification-step-title">补充会改变研究范围的条件</h2>
            </div>
            <div className="clarification-exchange">
              <div className="dialogue-turn user-turn">
                <span>你</span>
                <p>{question}</p>
              </div>
              <div className="dialogue-turn system-turn">
                <span>问题确认</span>
                <p>请确认适用地区、劳动关系解除时间，以及是否需要给出当年具体封顶金额。</p>
              </div>
            </div>
            <label className="field-label" htmlFor="clarification-answer">
              补充说明
            </label>
            <textarea
              id="clarification-answer"
              value={clarification}
              onChange={(event) => setClarification(event.target.value)}
            />
            <div className="onboarding-actions">
              <button className="secondary-command" type="button" onClick={() => setStep("question")}>
                <ArrowLeft aria-hidden="true" size={16} />
                上一步
              </button>
              <button
                className="primary-command"
                type="button"
                disabled={clarification.trim().length < 10}
                onClick={() => setStep("ready")}
              >
                提交补充
                <ArrowRight aria-hidden="true" size={16} />
              </button>
            </div>
          </section>
        ) : null}

        {step === "ready" ? (
          <section className="onboarding-panel" aria-labelledby="ready-step-title">
            <div className="onboarding-heading ready-heading">
              <CheckCircle2 aria-hidden="true" size={21} />
              <div>
                <span>步骤 4 / 5</span>
                <h2 id="ready-step-title">问题已明确，可以冻结本次输入</h2>
              </div>
            </div>
            <dl className="frozen-brief">
              <div>
                <dt>明确问题</dt>
                <dd>{question}</dd>
              </div>
              <div>
                <dt>已知上下文</dt>
                <dd>{clarification}</dd>
              </div>
              <div>
                <dt>资料版本</dt>
                <dd>
                  {selectedSnapshot?.name} · {selectedSnapshot?.versionLabel}
                </dd>
              </div>
              <div>
                <dt>回答方式</dt>
                <dd>{modes.map((mode) => (mode === "evidence-first" ? "证据优先" : "综合解读")).join("、")}</dd>
              </div>
            </dl>
            <div className="freeze-notice">
              <LockKeyhole aria-hidden="true" size={17} />
              <p>开始后不能修改问题、资料版本或回答方式；需要变更时创建新的 Request。</p>
            </div>
            <div className="onboarding-actions">
              <button className="secondary-command" type="button" onClick={() => setStep("clarification")}>
                <ArrowLeft aria-hidden="true" size={16} />
                返回补充
              </button>
              <button className="primary-command" type="button" onClick={() => setStep("running")}>
                <Play aria-hidden="true" size={16} />
                开始研究
              </button>
            </div>
          </section>
        ) : null}

        {step === "running" ? (
          <section className="onboarding-panel running-panel" aria-labelledby="running-step-title">
            <div className="running-pulse" aria-hidden="true">
              <span />
            </div>
            <div className="onboarding-heading">
              <span>步骤 5 / 5</span>
              <h2 id="running-step-title">正在锁定的资料中研究</h2>
              <p>已保存执行契约；进度按阶段和真实计数展示。</p>
            </div>
            <div className="running-checkpoint-list">
              <div className="is-complete">
                <Check aria-hidden="true" size={14} />
                <span>问题确认</span>
                <strong>完成</strong>
              </div>
              <div className="is-complete">
                <Check aria-hidden="true" size={14} />
                <span>独立模型答案</span>
                <strong>完成</strong>
              </div>
              <div className="is-current">
                <Circle aria-hidden="true" size={11} />
                <span>导航范围发现</span>
                <strong>选择了 2 个方向</strong>
              </div>
              <div>
                <span className="checkpoint-index">04</span>
                <span>正文读取与逐字取证</span>
                <strong>等待</strong>
              </div>
            </div>
            <div className="running-actions">
              <button className="primary-command" type="button" onClick={onOpenCompletedResearch}>
                查看完成结果
                <ArrowRight aria-hidden="true" size={16} />
              </button>
            </div>
          </section>
        ) : null}
      </main>
    </div>
  );
}
