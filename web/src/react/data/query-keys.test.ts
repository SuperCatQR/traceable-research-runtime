import { describe, expect, it } from "vitest";
import { workspaceKeys } from "./query-keys";

describe("workspace query keys", () => {
  it("isolates every account-owned resource", () => {
    expect(workspaceKeys.models("account-a")).not.toEqual(workspaceKeys.models("account-b"));
    expect(workspaceKeys.conversations("account-a")).not.toEqual(workspaceKeys.conversations("account-b"));
    expect(workspaceKeys.conversation("account-a", "conversation")).not.toEqual(
      workspaceKeys.conversation("account-b", "conversation"),
    );
    expect(workspaceKeys.traceSummary("account-a", "conversation", "turn")).not.toEqual(
      workspaceKeys.traceSummary("account-b", "conversation", "turn"),
    );
  });

  it("separates trace levels, turns, and audit stages", () => {
    expect(workspaceKeys.traceSummary("account", "conversation", "turn-1")).not.toEqual(
      workspaceKeys.traceSummary("account", "conversation", "turn-2"),
    );
    expect(workspaceKeys.traceAudit("account", "conversation", "turn", "search")).not.toEqual(
      workspaceKeys.traceAudit("account", "conversation", "turn", "synthesis"),
    );
    expect(workspaceKeys.traceSummary("account", "conversation", "turn")).not.toEqual(
      workspaceKeys.traceAudit("account", "conversation", "turn", ""),
    );
  });
});
