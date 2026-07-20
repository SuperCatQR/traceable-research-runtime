import { FileSearch, LoaderCircle } from "lucide-react";
import { useEffect, useRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type {
  DialogueMessage,
  ResearchConversationDetail,
  ResearchTurn,
} from "../../../research-workspace-client";
import { deriveTurnPresentation } from "../../domain/turn-presentation";
import { ScrollIndicator } from "../../shared/ScrollIndicator";
import { safeEvidenceUrl } from "../../shared/format";
import { TurnNavigator } from "./TurnNavigator";

interface ConversationTranscriptProps {
  conversation?: ResearchConversationDetail;
  hasModels: boolean;
  pendingMessage?: string;
  onCreateConversation(): void;
  onOpenSettings(): void;
  onOpenInspector(turnId: string): void;
}

function ResearchAnswer({ turn, onOpenInspector }: { turn: ResearchTurn; onOpenInspector(turnId: string): void }) {
  const answer = turn.answer!;
  return (
    <div className="research-answer answer-block">
      <div className="answer-heading">
        <div><p className="eyebrow">研究回答 / {String(turn.turn_number).padStart(2, "0")}</p><h2>研究结论</h2></div>
        {turn.completed_at && <time className="answer-date">{new Date(turn.completed_at * 1000).toLocaleDateString("zh-CN")}</time>}
      </div>
      <div className="answer-prose">
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          skipHtml
          components={{
            a: ({ href, children }) => {
              const safe = href ? safeEvidenceUrl(href) : undefined;
              return safe
                ? <a href={safe} target="_blank" rel="noreferrer">{children}</a>
                : <span>{children}</span>;
            },
          }}
        >
          {answer.answer}
        </ReactMarkdown>
      </div>
      {answer.sources.length > 0 && (
        <details className="answer-sources">
          <summary>来源 {answer.sources.length}</summary>
          <ul>
            {answer.sources.map((source, index) => {
              const safe = safeEvidenceUrl(source.url);
              return <li key={`${source.url}-${index}`}>{safe ? <a href={safe} target="_blank" rel="noreferrer">{source.title}</a> : <span>{source.title}</span>}</li>;
            })}
          </ul>
        </details>
      )}
      <div className="answer-footer">
        <button type="button" onClick={() => onOpenInspector(turn.turn_id)}><FileSearch aria-hidden="true" />查看研究概览</button>
        <span>证据与审计记录按需加载</span>
      </div>
    </div>
  );
}

function ResearchProgress() {
  return <div className="research-progress" role="status"><LoaderCircle className="spin" aria-hidden="true" /><span>正在检索、锁定快照并核验来源</span></div>;
}

function TurnOutcome({ turn, onOpenInspector }: { turn: ResearchTurn; onOpenInspector(turnId: string): void }) {
  if (turn.answer) return <ResearchAnswer turn={turn} onOpenInspector={onOpenInspector} />;
  const presentation = deriveTurnPresentation(turn);
  if (presentation.kind === "researching" || presentation.kind === "understanding") return <ResearchProgress />;
  if (presentation.kind === "failed") return <div className="turn-failure"><p>研究未完成。你可以继续提出新的研究问题。</p></div>;
  if (presentation.kind === "cancelled") return <div className="turn-failure"><p>这轮研究已取消。</p></div>;
  if (presentation.kind === "contract_error") return <div className="turn-failure contract-error" role="alert"><p>无法显示此轮研究。{presentation.detail}。</p></div>;
  if (turn.dialogue?.status === "failed") {
    return <div className="turn-failure"><p>{turn.dialogue.failure ?? "模型暂时无法继续理解该问题。"}</p></div>;
  }
  return null;
}

function UserMessage({ turn, message }: { turn: ResearchTurn; message: DialogueMessage }) {
  return (
    <article className="transcript-message user-message question-block">
      <div className="message-body question-message">
        <p className="eyebrow">用户问题 / {String(turn.turn_number).padStart(2, "0")}</p>
        <h2>{message.text}</h2>
      </div>
      <span className="question-index" aria-hidden="true">Q</span>
    </article>
  );
}

function AssistantMessage({
  turn,
  message,
  latest,
  onOpenInspector,
}: {
  turn: ResearchTurn;
  message: DialogueMessage;
  latest: boolean;
  onOpenInspector(turnId: string): void;
}) {
  return (
    <article className={`transcript-message assistant-message status-${turn.status}`}>
      <div className="assistant-message-accent" aria-hidden="true"><span /></div>
      <div className="assistant-response">
        <div className="message-body"><p>{message.text}</p></div>
        {latest && <TurnOutcome turn={turn} onOpenInspector={onOpenInspector} />}
      </div>
    </article>
  );
}

function ResearchTurnView({ turn, onOpenInspector }: { turn: ResearchTurn; onOpenInspector(turnId: string): void }) {
  const dialogue = turn.dialogue?.messages.length
    ? turn.dialogue.messages
    : [{ role: "user" as const, text: turn.user_question }];
  const lastAssistantIndex = dialogue.map((message) => message.role).lastIndexOf("assistant");
  return (
    <section
      id={`turn-${turn.turn_id}`}
      className="research-turn conversation-turn-record"
      data-turn-id={turn.turn_id}
      data-conversation-turn={turn.turn_id}
    >
      {dialogue.map((message, index) => message.role === "user"
        ? <UserMessage key={`${index}-${message.text}`} turn={turn} message={message} />
        : <AssistantMessage key={`${index}-${message.text}`} turn={turn} message={message} latest={index === lastAssistantIndex} onOpenInspector={onOpenInspector} />)}
      {lastAssistantIndex === -1 && <TurnOutcome turn={turn} onOpenInspector={onOpenInspector} />}
    </section>
  );
}

export function ConversationTranscript({
  conversation,
  hasModels,
  pendingMessage,
  onCreateConversation,
  onOpenSettings,
  onOpenInspector,
}: ConversationTranscriptProps) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const autoScrollCancelledRef = useRef(false);
  const scrollAnchorKey = conversation
    ? `${conversation.conversation_id}:${conversation.turns.at(-1)?.turn_id ?? "empty"}`
    : "none";

  const cancelAutoScroll = () => {
    autoScrollCancelledRef.current = true;
  };

  useEffect(() => {
    if (scrollAnchorKey === "none") return undefined;
    autoScrollCancelledRef.current = false;
    const scrollToLatest = () => {
      if (autoScrollCancelledRef.current) return;
      const scroller = scrollerRef.current;
      const last = scroller?.querySelector<HTMLElement>("[data-conversation-turn]:last-child");
      if (scroller && last) {
        scroller.scrollTop = Math.max(
          0,
          last.getBoundingClientRect().top
            - scroller.getBoundingClientRect().top
            + scroller.scrollTop
            - 4,
        );
      }
    };
    const frame = requestAnimationFrame(() => requestAnimationFrame(scrollToLatest));
    const timer = window.setTimeout(scrollToLatest, 180);
    void document.fonts?.ready.then(scrollToLatest);
    const onUserScrollIntent = () => {
      autoScrollCancelledRef.current = true;
    };
    const scroller = scrollerRef.current;
    scroller?.addEventListener("wheel", onUserScrollIntent, { passive: true });
    scroller?.addEventListener("touchstart", onUserScrollIntent, { passive: true });
    scroller?.addEventListener("pointerdown", onUserScrollIntent, { passive: true });
    scroller?.addEventListener("keydown", onUserScrollIntent);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(timer);
      scroller?.removeEventListener("wheel", onUserScrollIntent);
      scroller?.removeEventListener("touchstart", onUserScrollIntent);
      scroller?.removeEventListener("pointerdown", onUserScrollIntent);
      scroller?.removeEventListener("keydown", onUserScrollIntent);
    };
  }, [scrollAnchorKey]);

  useEffect(() => {
    if (!pendingMessage) return undefined;
    const frame = requestAnimationFrame(() => {
      const scroller = scrollerRef.current;
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
    return () => cancelAnimationFrame(frame);
  }, [pendingMessage]);

  if (!conversation) {
    return (
      <div className="workspace-empty">
        <span className="empty-ledger-mark" aria-hidden="true" />
        <h2>{hasModels ? "开始一项研究" : "先连接一个模型"}</h2>
        <button className="secondary-command" type="button" onClick={hasModels ? onCreateConversation : onOpenSettings}>
          {hasModels ? "新研究" : "添加模型配置"}
        </button>
      </div>
    );
  }
  if (conversation.turns.length === 0) {
    return <div className="workspace-empty transcript-empty"><span className="empty-ledger-mark" aria-hidden="true" /><h2>写下需要查证的问题</h2></div>;
  }
  return (
    <>
      <div ref={scrollerRef} className="conversation-transcript document-scroll" id="conversation-transcript" aria-live="polite">
        <div className="transcript-inner research-document">
          {conversation.turns.map((turn) => <ResearchTurnView key={turn.turn_id} turn={turn} onOpenInspector={onOpenInspector} />)}
          {pendingMessage && (
            <section className="research-turn pending-turn" aria-live="polite">
              <article className="transcript-message user-message question-block">
                <div className="message-body question-message">
                  <p className="eyebrow">发送中</p>
                  <h2>{pendingMessage}</h2>
                </div>
                <span className="question-index" aria-hidden="true">Q</span>
              </article>
            </section>
          )}
        </div>
      </div>
      <TurnNavigator turns={conversation.turns} scrollerRef={scrollerRef} onUserNavigate={cancelAutoScroll} />
      <ScrollIndicator scrollerRef={scrollerRef} primary />
    </>
  );
}
