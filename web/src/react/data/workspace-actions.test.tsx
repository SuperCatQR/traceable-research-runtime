import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook } from "@testing-library/react";
import type { PropsWithChildren } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ResearchWorkspaceRequestError } from "../../research-workspace-client";
import { createDemoWorkspaceGateway } from "../test/demo-workspace-gateway";
import { workspaceKeys } from "./query-keys";
import { useAuthActions, useWorkspaceActions } from "./workspace-actions";
import { WorkspaceGatewayProvider, type WorkspaceGateway } from "./workspace-gateway";

const accountId = "demo-account";
const conversationId = "conversation-four-day-week";

function createHarness(gateway: WorkspaceGateway) {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: Infinity },
      mutations: { retry: false },
    },
  });
  function Wrapper({ children }: PropsWithChildren) {
    return (
      <QueryClientProvider client={queryClient}>
        <WorkspaceGatewayProvider gateway={gateway}>{children}</WorkspaceGatewayProvider>
      </QueryClientProvider>
    );
  }
  return { queryClient, Wrapper };
}

afterEach(() => {
  window.sessionStorage.clear();
  vi.restoreAllMocks();
});

describe("useAuthActions", () => {
  it("stores an authenticated account without exposing cache details to the caller", async () => {
    const gateway = createDemoWorkspaceGateway();
    const { queryClient, Wrapper } = createHarness(gateway);
    const { result } = renderHook(() => useAuthActions(), { wrapper: Wrapper });

    await act(async () => {
      await result.current.submit({ kind: "login", email: "researcher@example.com", password: "long-enough-password" });
    });

    expect(queryClient.getQueryData(workspaceKeys.session)).toMatchObject({ user_id: accountId });
    expect(result.current.error).toBeNull();
  });
});

describe("useWorkspaceActions", () => {
  it("reuses an idempotency key for the same failed request and clears it after success", async () => {
    const gateway = createDemoWorkspaceGateway();
    const create = gateway.conversations.create;
    const keys: string[] = [];
    let shouldFail = true;
    gateway.conversations.create = async (modelProfileId, idempotencyKey) => {
      keys.push(idempotencyKey);
      if (shouldFail) {
        shouldFail = false;
        throw new Error("temporary failure");
      }
      return create(modelProfileId, idempotencyKey);
    };
    const { queryClient, Wrapper } = createHarness(gateway);
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      expect(await result.current.conversation.create("profile-primary")).toBeUndefined();
      expect(await result.current.conversation.create("profile-primary")).toBeDefined();
      expect(await result.current.conversation.create("profile-primary")).toBeDefined();
    });

    expect(keys[0]).toBe(keys[1]);
    expect(keys[2]).not.toBe(keys[1]);
    const created = queryClient.getQueryData<{ conversation_id: string }>(
      workspaceKeys.conversation(accountId, "conversation-3"),
    );
    expect(created?.conversation_id).toBe("conversation-3");
  });

  it("replaces the key when the payload changes instead of reviving an older failed key", async () => {
    const gateway = createDemoWorkspaceGateway();
    const keys: string[] = [];
    gateway.conversations.create = async (_modelProfileId, idempotencyKey) => {
      keys.push(idempotencyKey);
      throw new Error("still offline");
    };
    const { Wrapper } = createHarness(gateway);
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      await result.current.conversation.create("profile-a");
      await result.current.conversation.create("profile-b");
      await result.current.conversation.create("profile-a");
    });

    expect(new Set(keys).size).toBe(3);
  });

  it("updates the cached turn and conversation metadata after composer submission", async () => {
    const gateway = createDemoWorkspaceGateway();
    const conversation = await gateway.conversations.load(conversationId);
    const { queryClient, Wrapper } = createHarness(gateway);
    queryClient.setQueryData(workspaceKeys.conversation(accountId, conversationId), conversation);
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      expect(await result.current.conversation.send(conversation, "继续比较长期结果")).toBe(true);
    });

    const cached = queryClient.getQueryData<typeof conversation>(
      workspaceKeys.conversation(accountId, conversationId),
    );
    expect(cached?.turn_count).toBe(6);
    expect(cached?.latest_turn_status).toBe("clarifying");
    expect(cached?.turns.at(-1)?.user_question).toBe("继续比较长期结果");
  });

  it("refreshes a stale conversation after a dialogue revision conflict", async () => {
    const gateway = createDemoWorkspaceGateway();
    await gateway.conversations.startTurn(conversationId, "先提出一个需要澄清的问题", "web_first", "seed-key");
    const conversation = await gateway.conversations.load(conversationId);
    const keys: string[] = [];
    gateway.conversations.submitMessage = async (_id, _turnId, _revision, _message, key) => {
      keys.push(key);
      throw new ResearchWorkspaceRequestError(
        "对话版本已经更新",
        409,
        "dialogue_revision_conflict",
        true,
      );
    };
    const { queryClient, Wrapper } = createHarness(gateway);
    const conversationKey = workspaceKeys.conversation(accountId, conversationId);
    queryClient.setQueryData(conversationKey, conversation);
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      expect(await result.current.conversation.send(conversation, "优先比较长期证据")).toBe(false);
      expect(await result.current.conversation.send(conversation, "优先比较长期证据")).toBe(false);
    });

    expect(keys[0]).toBe(keys[1]);
    expect(queryClient.getQueryState(conversationKey)?.isInvalidated).toBe(true);
  });

  it("removes an archived conversation from active cache and refreshes the archive", async () => {
    const gateway = createDemoWorkspaceGateway();
    const summaries = await gateway.conversations.list();
    const { queryClient, Wrapper } = createHarness(gateway);
    const archiveKey = workspaceKeys.archivedConversations(accountId);
    queryClient.setQueryData(workspaceKeys.conversations(accountId), summaries);
    queryClient.setQueryData(archiveKey, []);
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      expect(await result.current.conversation.archive(conversationId)).toBe(true);
    });

    expect(queryClient.getQueryData<unknown[]>(workspaceKeys.conversations(accountId))).toEqual([]);
    expect(queryClient.getQueryState(archiveKey)?.isInvalidated).toBe(true);
  });

  it("clears account caches, drafts, and idempotency state on authentication expiry", async () => {
    const gateway = createDemoWorkspaceGateway();
    gateway.conversations.update = async () => {
      throw new ResearchWorkspaceRequestError("登录已过期", 401, "authentication_required", false);
    };
    const expired = vi.fn();
    const { queryClient, Wrapper } = createHarness(gateway);
    queryClient.setQueryData(workspaceKeys.models(accountId), [{ profile_id: "profile-primary" }]);
    window.sessionStorage.setItem(`research-draft:${accountId}:${conversationId}`, "保留中的草稿");
    const { result } = renderHook(() => useWorkspaceActions(accountId, expired), { wrapper: Wrapper });

    await act(async () => {
      expect(await result.current.conversation.update(conversationId, { title: "新标题" })).toBe(false);
    });

    expect(expired).toHaveBeenCalledOnce();
    expect(queryClient.getQueryData(workspaceKeys.models(accountId))).toBeUndefined();
    expect(queryClient.getQueryData(workspaceKeys.session)).toBeNull();
    expect(window.sessionStorage.length).toBe(0);
  });

  it("preserves local account state when remote logout fails", async () => {
    const gateway = createDemoWorkspaceGateway();
    gateway.auth.logout = async () => {
      throw new Error("network unavailable");
    };
    const { queryClient, Wrapper } = createHarness(gateway);
    queryClient.setQueryData(workspaceKeys.models(accountId), [{ profile_id: "profile-primary" }]);
    window.sessionStorage.setItem(`research-draft:${accountId}:${conversationId}`, "草稿");
    const { result } = renderHook(() => useWorkspaceActions(accountId, vi.fn()), { wrapper: Wrapper });

    await act(async () => {
      await result.current.logout();
    });

    expect(queryClient.getQueryData(workspaceKeys.models(accountId))).toEqual([
      { profile_id: "profile-primary" },
    ]);
    expect(queryClient.getQueryData(workspaceKeys.session)).toBeUndefined();
    expect(window.sessionStorage.getItem(`research-draft:${accountId}:${conversationId}`)).toBe("草稿");
    expect(result.current.error).toBe("network unavailable");
  });

  it("clears local account state when logout reports an expired session", async () => {
    const gateway = createDemoWorkspaceGateway();
    gateway.auth.logout = async () => {
      throw new ResearchWorkspaceRequestError("登录已过期", 401, "authentication_required", false);
    };
    const expired = vi.fn();
    const { queryClient, Wrapper } = createHarness(gateway);
    queryClient.setQueryData(workspaceKeys.models(accountId), [{ profile_id: "profile-primary" }]);
    window.sessionStorage.setItem(`research-draft:${accountId}:${conversationId}`, "草稿");
    const { result } = renderHook(() => useWorkspaceActions(accountId, expired), { wrapper: Wrapper });

    await act(async () => {
      await result.current.logout();
    });

    expect(expired).toHaveBeenCalledOnce();
    expect(queryClient.getQueryData(workspaceKeys.models(accountId))).toBeUndefined();
    expect(queryClient.getQueryData(workspaceKeys.session)).toBeNull();
    expect(window.sessionStorage.length).toBe(0);
  });
});
