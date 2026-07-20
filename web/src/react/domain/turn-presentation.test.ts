import { describe, expect, it } from "vitest";
import type { ResearchTurn } from "../../research-workspace-client";
import { activeConversationPollDelay, deriveTurnPresentation } from "./turn-presentation";

function turn(overrides: Partial<ResearchTurn> = {}): ResearchTurn {
  return {
    turn_id: "turn-1",
    turn_number: 1,
    user_question: "question",
    status: "clarifying",
    answer: null,
    dialogue: {
      revision: 1,
      status: "thinking",
      messages: [{ role: "user", text: "question" }],
      failure: null,
    },
    created_at: 1,
    updated_at: 1,
    completed_at: null,
    ...overrides,
  };
}

describe("deriveTurnPresentation", () => {
  it.each([
    [turn(), "understanding", 3000, false],
    [turn({ dialogue: { revision: 2, status: "awaiting_message", messages: [], failure: null } }), "awaiting_user", null, true],
    [turn({ dialogue: { revision: 2, status: "failed", messages: [], failure: "failed" } }), "awaiting_user", null, true],
    [turn({ status: "ready" }), "researching", 5000, false],
    [turn({ status: "running" }), "researching", 5000, false],
    [turn({ status: "failed" }), "failed", null, true],
    [turn({ status: "cancelled" }), "cancelled", null, true],
    [turn({ status: "completed", answer: { answer: "done", sources: [] } }), "completed", null, true],
  ] as const)("maps a legal state", (input, kind, pollAfterMs, canSubmit) => {
    expect(deriveTurnPresentation(input)).toMatchObject({ kind, pollAfterMs, canSubmit });
  });

  it("rejects incomplete terminal and clarification contracts", () => {
    expect(deriveTurnPresentation(turn({ status: "completed", answer: null })).kind).toBe("contract_error");
    expect(deriveTurnPresentation(turn({ dialogue: null })).kind).toBe("contract_error");
    expect(deriveTurnPresentation(turn({
      dialogue: { revision: 2, status: "research_started", messages: [], failure: null },
    })).kind).toBe("contract_error");
    expect(deriveTurnPresentation(turn({ status: "unexpected" as ResearchTurn["status"] })).kind).toBe("contract_error");
    expect(deriveTurnPresentation(turn({
      dialogue: { revision: 2, status: "unexpected" as NonNullable<ResearchTurn["dialogue"]>["status"], messages: [], failure: null },
    })).kind).toBe("contract_error");
  });

  it("uses the background polling interval only for active turns", () => {
    const conversation = {
      conversation_id: "conversation",
      title: "title",
      model_profile_id: "model",
      model_profile_name: "model",
      turn_count: 1,
      latest_turn_status: "clarifying" as const,
      created_at: 1,
      updated_at: 1,
      turns: [turn()],
    };
    expect(activeConversationPollDelay(conversation, false)).toBe(3000);
    expect(activeConversationPollDelay(conversation, true)).toBe(15000);
    expect(activeConversationPollDelay({ ...conversation, turns: [turn({ status: "failed" })] }, false)).toBe(false);
  });
});
