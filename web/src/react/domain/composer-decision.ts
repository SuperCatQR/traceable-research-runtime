import type { ResearchConversationDetail } from "../../research-workspace-client";

export type ComposerDecision =
  | { kind: "new_turn" }
  | { kind: "dialogue_message"; turnId: string; revision: number };

export function decideComposerAction(conversation: ResearchConversationDetail): ComposerDecision {
  const lastTurn = conversation.turns.at(-1);
  if (
    lastTurn?.status === "clarifying"
    && lastTurn.dialogue
    && (lastTurn.dialogue.status === "awaiting_message" || lastTurn.dialogue.status === "failed")
  ) {
    return {
      kind: "dialogue_message",
      turnId: lastTurn.turn_id,
      revision: lastTurn.dialogue.revision,
    };
  }
  return { kind: "new_turn" };
}
