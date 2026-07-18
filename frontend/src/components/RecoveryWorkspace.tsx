import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowRight,
  Check,
  CheckCircle2,
  Circle,
  Clock3,
  FileCheck2,
  FileText,
  LockKeyhole,
  RotateCcw,
  ShieldCheck,
  X,
} from "lucide-react";
import type { ResearchRequest } from "../types";
import { formatDateTime } from "../format";

type RecoveryState = "interrupted" | "resuming" | "resumed" | "cancelled";

interface RecoveryWorkspaceProps {
  request: ResearchRequest;
  onOpenCompletedResearch: () => void;
  onNewResearch: () => void;
}

export function RecoveryWorkspace({
  request,
  onOpenCompletedResearch,
  onNewResearch,
}: RecoveryWorkspaceProps) {
  const [recoveryState, setRecoveryState] = useState<RecoveryState>("interrupted");
  const [showCancelDialog, setShowCancelDialog] = useState(false);
  const cancelTriggerRef = useRef<HTMLButtonElement>(null);
  const cancelDialogRef = useRef<HTMLElement>(null);
  const returnButtonRef = useRef<HTMLButtonElement>(null);
  const cancelledHeadingRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    if (recoveryState !== "resuming") {
      return undefined;
    }
    const timer = window.setTimeout(() => setRecoveryState("resumed"), 1100);
    return () => window.clearTimeout(timer);
  }, [recoveryState]);

  useEffect(() => {
    if (!showCancelDialog) {
      return undefined;
    }
    const trigger = cancelTriggerRef.current;
    returnButtonRef.current?.focus();
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setShowCancelDialog(false);
        return;
      }
      if (event.key !== "Tab") {
        return;
      }
      const focusable = Array.from(
        cancelDialogRef.current?.querySelectorAll<HTMLElement>("button:not([disabled])") ?? [],
      );
      const first = focusable[0];
      const last = focusable.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      if (trigger?.isConnected) {
        trigger.focus();
      }
    };
  }, [showCancelDialog]);

  useEffect(() => {
    if (recoveryState === "cancelled") {
      cancelledHeadingRef.current?.focus();
    }
  }, [recoveryState]);

  if (recoveryState === "cancelled") {
    return (
      <div className="terminal-state-workspace">
        <div className="terminal-state-mark">
          <X aria-hidden="true" size={25} />
        </div>
        <span className="section-eyebrow">REQUEST CANCELLED</span>
        <h1 ref={cancelledHeadingRef} tabIndex={-1}>
          请求 #{request.number} 已取消
        </h1>
        <p>已保存的执行记录仍可审计，但该 Request 不再恢复。需要继续时请创建新的 Request。</p>
        <button className="primary-command" type="button" onClick={onNewResearch}>
          新建研究
          <ArrowRight aria-hidden="true" size={16} />
        </button>
      </div>
    );
  }

  return (
    <div className="recovery-workspace">
      <header className="recovery-header">
        <div className="request-kicker">
          <span>请求 #{request.number}</span>
          <span>{formatDateTime(request.updatedAt)} 更新</span>
        </div>
        <h1>{request.shortTitle}</h1>
        <p>{request.clarifiedQuestion}</p>
        <div className="locked-contract compact-contract">
          <LockKeyhole aria-hidden="true" size={16} />
          <span>锁定版本</span>
          <strong>
            {request.snapshot.name} · {request.snapshot.versionLabel}
          </strong>
        </div>
      </header>

      {recoveryState === "interrupted" ? (
        <section className="recovery-band is-interrupted" aria-labelledby="interrupted-title">
          <div className="recovery-band-icon">
            <AlertTriangle aria-hidden="true" size={21} />
          </div>
          <div className="recovery-band-copy">
            <span className="section-eyebrow">RETRYABLE INTERRUPTION</span>
            <h2 id="interrupted-title">研究暂时中断，检查点已保存</h2>
            <p>模型服务返回可重试的 503。已提交的读取、逐字证据和执行记录不会重复写入。</p>
          </div>
          <div className="recovery-actions">
            <button className="primary-command" type="button" onClick={() => setRecoveryState("resuming")}>
              <RotateCcw aria-hidden="true" size={16} />
              继续研究
            </button>
            <button
              ref={cancelTriggerRef}
              className="danger-command"
              type="button"
              onClick={() => setShowCancelDialog(true)}
            >
              取消请求
            </button>
          </div>
        </section>
      ) : null}

      {recoveryState === "resuming" ? (
        <section className="recovery-band is-resuming" aria-live="polite">
          <div className="resume-spinner" aria-hidden="true" />
          <div className="recovery-band-copy">
            <span className="section-eyebrow">RESUMING</span>
            <h2>正在从检查点继续</h2>
            <p>恢复前先重新读取并校验完整执行记录。</p>
          </div>
        </section>
      ) : null}

      {recoveryState === "resumed" ? (
        <section className="recovery-band is-resumed" aria-live="polite">
          <div className="recovery-band-icon">
            <CheckCircle2 aria-hidden="true" size={21} />
          </div>
          <div className="recovery-band-copy">
            <span className="section-eyebrow">CHECKPOINT RESUMED</span>
            <h2>研究已继续并完成</h2>
            <p>恢复沿用原 Snapshot、问题和回答方式，没有创建新的 execution。</p>
          </div>
          <button className="primary-command" type="button" onClick={onOpenCompletedResearch}>
            查看答案
            <ArrowRight aria-hidden="true" size={16} />
          </button>
        </section>
      ) : null}

      <section className="checkpoint-summary" aria-labelledby="checkpoint-title">
        <header className="section-heading-row">
          <div>
            <span className="section-eyebrow">PERSISTED CHECKPOINT</span>
            <h2 id="checkpoint-title">中断前已保存</h2>
          </div>
          <span className="checkpoint-time">
            <Clock3 aria-hidden="true" size={15} />
            14:32:18 HKT
          </span>
        </header>
        <div className="metric-strip recovery-metrics" aria-label="检查点计数">
          <div className="metric-item">
            <FileText aria-hidden="true" size={16} />
            <strong>{request.counts.selectedDocuments}</strong>
            <span>选中文档</span>
          </div>
          <div className="metric-item">
            <FileCheck2 aria-hidden="true" size={16} />
            <strong>{request.counts.segmentReads}</strong>
            <span>正文读取</span>
          </div>
          <div className="metric-item">
            <ShieldCheck aria-hidden="true" size={16} />
            <strong>{request.counts.acceptedEvidence}</strong>
            <span>逐字证据</span>
          </div>
        </div>
        <ol className="recovery-phase-list">
          {request.phases.map((phase, index) => (
            <li className={`phase-${phase.status}`} key={phase.id}>
              <span className="phase-marker" aria-hidden="true">
                {phase.status === "complete" ? (
                  <Check size={14} />
                ) : phase.status === "attention" ? (
                  <AlertTriangle size={14} />
                ) : (
                  <Circle size={10} />
                )}
              </span>
              <span className="phase-number">{String(index + 1).padStart(2, "0")}</span>
              <div>
                <strong>{phase.label}</strong>
                <p>{phase.detail}</p>
              </div>
            </li>
          ))}
        </ol>
      </section>

      {showCancelDialog ? (
        <div className="dialog-scrim" role="presentation">
          <section
            ref={cancelDialogRef}
            className="confirmation-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="cancel-dialog-title"
          >
            <div className="dialog-icon">
              <AlertTriangle aria-hidden="true" size={21} />
            </div>
            <h2 id="cancel-dialog-title">取消这个 Request？</h2>
            <p>取消会写入唯一终态，之后不能恢复。已保存的安全执行记录仍会保留。</p>
            <div className="dialog-actions">
              <button
                ref={returnButtonRef}
                className="secondary-command"
                type="button"
                onClick={() => setShowCancelDialog(false)}
              >
                返回
              </button>
              <button
                className="danger-command is-solid"
                type="button"
                onClick={() => {
                  setShowCancelDialog(false);
                  setRecoveryState("cancelled");
                }}
              >
                确认取消
              </button>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
