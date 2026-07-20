import { LoaderCircle, Send } from "lucide-react";
import { useLayoutEffect, useRef } from "react";
import type { ResearchConversationDetail } from "../../../research-workspace-client";
import { decideComposerAction } from "../../domain/composer-decision";
import { deriveTurnPresentation } from "../../domain/turn-presentation";

interface ResearchComposerProps {
  conversation?: ResearchConversationDetail;
  draft: string;
  pending: boolean;
  onDraftChange(value: string): void;
  onSubmit(): void;
}
export function ResearchComposer({ conversation, draft, pending, onDraftChange, onSubmit }: ResearchComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const lastTurn = conversation?.turns.at(-1);
  const decision = conversation ? decideComposerAction(conversation) : { kind: "new_turn" as const };
  const presentation = lastTurn ? deriveTurnPresentation(lastTurn) : null;
  const hasBlockingTurn = Boolean(presentation && !presentation.canSubmit);
  const disabled = !conversation || pending || hasBlockingTurn;
  const placeholder = pending
    ? "正在发送…"
    : decision.kind === "dialogue_message"
      ? "继续补充或纠正我的理解…"
      : hasBlockingTurn
        ? "先完成当前研究轮次"
        : "输入需要查证的研究问题…";

  useLayoutEffect(() => {
    const textarea = textareaRef.current;
    if (!textarea) return;
    textarea.style.height = "auto";
    textarea.style.height = `${Math.min(textarea.scrollHeight, 144)}px`;
  }, [draft]);

  return (
    <footer className="composer-region composer">
      <form
        className="research-composer composer-box"
        onSubmit={(event) => {
          event.preventDefault();
          if (draft.trim() && !disabled) onSubmit();
        }}
      >
        <label className="sr-only" htmlFor="research-question">{decision.kind === "dialogue_message" ? "继续对话" : "研究问题"}</label>
        <textarea
          ref={textareaRef}
          id="research-question"
          value={draft}
          onChange={(event) => onDraftChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              event.currentTarget.form?.requestSubmit();
            }
          }}
          rows={1}
          maxLength={4000}
          placeholder={placeholder}
          disabled={disabled}
        />
        <button type="submit" className="send-command" aria-label="发送" title="发送" disabled={disabled || !draft.trim()}>
          {pending ? <LoaderCircle className="spin" aria-hidden="true" /> : <Send aria-hidden="true" />}
        </button>
      </form>
    </footer>
  );
}
