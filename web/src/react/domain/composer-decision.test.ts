import { describe, expect, it } from "vitest";
import type { ResearchConversationDetail, ResearchTurn } from "../../research-workspace-client";
import { decideComposerAction } from "./composer-decision";

const baseTurn: ResearchTurn = {
  turn_id: "turn-1",
  turn_number: 1,
  user_question: "question",
  status: "clarifying",
  answer: null,
  dialogue: { revision: 7, status: "awaiting_message", messages: [], failure: null },
  created_at: 1,
  updated_at: 1,
  completed_at: null,
};

function conversation(turn: ResearchTurn): ResearchConversationDetail {
  return {
    conversation_id: "conversation",
    title: "title",
    model_profile_id: "profile",
    model_profile_name: "profile",
    turn_count: 1,
    latest_turn_status: turn.status,
    created_at: 1,
    updated_at: 1,
    turns: [turn],
  };
}

describe("decideComposerAction", () => {
  it("submits a revision-guarded message while clarification awaits the user", () => {
    expect(decideComposerAction(conversation(baseTurn))).toEqual({
      kind: "dialogue_message",
      turnId: "turn-1",
      revision: 7,
    });
  });

  it("starts a new turn after a terminal result", () => {
    expect(decideComposerAction(conversation({ ...baseTurn, status: "completed" }))).toEqual({ kind: "new_turn" });
  });
});
