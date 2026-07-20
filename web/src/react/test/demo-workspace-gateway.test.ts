import { describe, expect, it } from "vitest";
import {
  createDemoWorkspaceGateway,
  resolveDemoWorkspaceScenario,
  type DemoWorkspaceScenario,
} from "./demo-workspace-gateway";

describe("demo workspace scenario resolution", () => {
  it.each([
    ["", "complete"],
    ["complete", "complete"],
    ["running", "running"],
    ["empty", "empty"],
    ["error", "error"],
    ["long", "long"],
    ["setup", "setup"],
    ["unknown", "complete"],
    [null, "complete"],
  ] as const)("resolves %s to %s", (value, expected) => {
    expect(resolveDemoWorkspaceScenario(value)).toBe(expected);
  });
});

describe("demo workspace fixtures", () => {
  async function loadLastTurn(scenario: DemoWorkspaceScenario) {
    const gateway = createDemoWorkspaceGateway(scenario);
    const [summary] = await gateway.conversations.list();
    expect(summary).toBeDefined();
    const conversation = await gateway.conversations.load(summary.conversation_id);
    return { summary, turn: conversation.turns.at(-1) };
  }

  it("provides the completed multi-turn workspace by default", async () => {
    const gateway = createDemoWorkspaceGateway();
    const [summary] = await gateway.conversations.list();
    const conversation = await gateway.conversations.load(summary.conversation_id);

    expect(summary.latest_turn_status).toBe("completed");
    expect(conversation.turns).toHaveLength(5);
    expect(conversation.turns.at(-1)?.answer).not.toBeNull();
  });

  it("provides a running final turn without a completed answer", async () => {
    const { summary, turn } = await loadLastTurn("running");

    expect(summary.latest_turn_status).toBe("running");
    expect(turn).toMatchObject({ status: "running", answer: null, completed_at: null });
  });

  it("provides an authenticated workspace with no active conversations", async () => {
    const gateway = createDemoWorkspaceGateway("empty");

    await expect(gateway.auth.current()).resolves.toMatchObject({ user_id: "demo-account" });
    await expect(gateway.models.list()).resolves.toHaveLength(1);
    await expect(gateway.conversations.list()).resolves.toEqual([]);
  });

  it("provides a first-time setup workspace without model profiles", async () => {
    const gateway = createDemoWorkspaceGateway("setup");

    await expect(gateway.auth.current()).resolves.toMatchObject({ user_id: "demo-account" });
    await expect(gateway.models.list()).resolves.toEqual([]);
    await expect(gateway.conversations.list()).resolves.toEqual([]);
  });

  it("provides a failed final turn with failure context", async () => {
    const { summary, turn } = await loadLastTurn("error");

    expect(summary.latest_turn_status).toBe("failed");
    expect(turn).toMatchObject({
      status: "failed",
      answer: null,
      completed_at: null,
      dialogue: {
        status: "failed",
        failure: "Demo research failed before synthesis.",
      },
    });
  });

  it("provides long Chinese content for wrapping and alignment checks", async () => {
    const gateway = createDemoWorkspaceGateway("long");
    const [summary] = await gateway.conversations.list();
    const conversation = await gateway.conversations.load(summary.conversation_id);

    expect(summary.title.length).toBeGreaterThan(20);
    expect(conversation.turns.at(-1)?.user_question.length).toBeGreaterThan(50);
    expect(conversation.turns.at(-1)?.answer?.answer.length).toBeGreaterThan(250);
  });
});
