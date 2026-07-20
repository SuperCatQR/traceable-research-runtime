import type {
  DialogueStatus,
  ResearchConversationDetail,
  ResearchTurn,
  ResearchTurnStatus,
} from "../../research-workspace-client";

export type TurnPresentation =
  | { kind: "understanding"; canSubmit: false; pollAfterMs: 3000 }
  | { kind: "awaiting_user"; canSubmit: true; pollAfterMs: null }
  | { kind: "researching"; canSubmit: false; pollAfterMs: 5000 }
  | { kind: "completed"; canSubmit: true; pollAfterMs: null }
  | { kind: "failed"; canSubmit: true; pollAfterMs: null }
  | { kind: "cancelled"; canSubmit: true; pollAfterMs: null }
  | { kind: "contract_error"; canSubmit: false; pollAfterMs: null; detail: string };

const knownTurnStatuses = new Set<ResearchTurnStatus>([
  "clarifying",
  "ready",
  "running",
  "completed",
  "failed",
  "cancelled",
]);

const knownDialogueStatuses = new Set<DialogueStatus>([
  "thinking",
  "awaiting_message",
  "research_started",
  "failed",
  "cancelled",
]);

export function deriveTurnPresentation(turn: ResearchTurn): TurnPresentation {
  if (!knownTurnStatuses.has(turn.status)) {
    return { kind: "contract_error", canSubmit: false, pollAfterMs: null, detail: `未知轮次状态：${String(turn.status)}` };
  }
  if (turn.status === "completed") {
    return turn.answer
      ? { kind: "completed", canSubmit: true, pollAfterMs: null }
      : { kind: "contract_error", canSubmit: false, pollAfterMs: null, detail: "已完成的轮次缺少回答正文" };
  }
  if (turn.status === "ready" || turn.status === "running") {
    return { kind: "researching", canSubmit: false, pollAfterMs: 5000 };
  }
  if (turn.status === "failed") return { kind: "failed", canSubmit: true, pollAfterMs: null };
  if (turn.status === "cancelled") return { kind: "cancelled", canSubmit: true, pollAfterMs: null };
  if (!turn.dialogue) {
    return { kind: "contract_error", canSubmit: false, pollAfterMs: null, detail: "澄清中的轮次缺少对话状态" };
  }
  if (!knownDialogueStatuses.has(turn.dialogue.status)) {
    return { kind: "contract_error", canSubmit: false, pollAfterMs: null, detail: `未知对话状态：${String(turn.dialogue.status)}` };
  }
  if (turn.dialogue.status === "thinking") {
    return { kind: "understanding", canSubmit: false, pollAfterMs: 3000 };
  }
  if (turn.dialogue.status === "awaiting_message" || turn.dialogue.status === "failed") {
    return { kind: "awaiting_user", canSubmit: true, pollAfterMs: null };
  }
  return {
    kind: "contract_error",
    canSubmit: false,
    pollAfterMs: null,
    detail: `轮次与对话状态不兼容：clarifying/${turn.dialogue.status}`,
  };
}
export function activeConversationPollDelay(
  conversation: ResearchConversationDetail | undefined,
  pageIsHidden: boolean,
): number | false {
  const turn = conversation?.turns.at(-1);
  if (!turn) return false;
  const delay = deriveTurnPresentation(turn).pollAfterMs;
  if (delay === null) return false;
  return pageIsHidden ? 15_000 : delay;
}
