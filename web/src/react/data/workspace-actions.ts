import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo, useRef, useState } from "react";
import {
  createIdempotencyKey,
  ResearchWorkspaceRequestError,
  type ModelProfile,
  type ResearchAnswerStyle,
  type ResearchConversationDetail,
  type ResearchConversationSummary,
  type ResearchTurn,
  type SaveModelProfileInput,
  type UserAccount,
} from "../../research-workspace-client";
import { decideComposerAction } from "../domain/composer-decision";
import { workspaceKeys } from "./query-keys";
import {
  clearAccountQueries,
  errorMessage,
  isAuthenticationError,
} from "./workspace-queries";
import { useWorkspaceGateway } from "./workspace-gateway";

type AuthenticationInput =
  | { kind: "login"; email: string; password: string }
  | { kind: "register"; email: string; password: string; displayName: string };

interface IdempotencyEntry {
  fingerprint: string;
  key: string;
}

interface ActionResult<T> {
  ok: boolean;
  value?: T;
  error?: unknown;
}

function replaceTurn(
  conversation: ResearchConversationDetail,
  updated: ResearchTurn,
): ResearchConversationDetail {
  const existing = conversation.turns.findIndex((turn) => turn.turn_id === updated.turn_id);
  const turns = [...conversation.turns];
  if (existing === -1) turns.push(updated);
  else turns[existing] = updated;
  return {
    ...conversation,
    turns,
    turn_count: Math.max(conversation.turn_count, turns.length),
    latest_turn_status: updated.status,
    updated_at: updated.updated_at,
  };
}

function updateSummaryList(
  list: ResearchConversationSummary[] | undefined,
  summary: ResearchConversationSummary,
): ResearchConversationSummary[] {
  const remaining = (list ?? []).filter((item) => item.conversation_id !== summary.conversation_id);
  return [summary, ...remaining].sort((a, b) => b.updated_at - a.updated_at);
}

function clearDraftStorage(): void {
  Object.keys(window.sessionStorage)
    .filter((key) => key.startsWith("research-draft:"))
    .forEach((key) => window.sessionStorage.removeItem(key));
}

export function useAuthActions() {
  const gateway = useWorkspaceGateway();
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation<UserAccount, unknown, AuthenticationInput>({
    mutationFn: (input) => input.kind === "register"
      ? gateway.auth.register(input.email, input.password, input.displayName)
      : gateway.auth.login(input.email, input.password),
  });

  const submit = useCallback(async (input: AuthenticationInput): Promise<UserAccount | undefined> => {
    setError(null);
    try {
      const account = await mutation.mutateAsync(input);
      queryClient.setQueryData(workspaceKeys.session, account);
      return account;
    } catch (reason) {
      setError(errorMessage(reason, "账户请求未能完成"));
      return undefined;
    }
  }, [mutation, queryClient]);

  return {
    submit,
    pending: mutation.isPending,
    error,
    clearError: useCallback(() => setError(null), []),
  };
}

export function useWorkspaceActions(
  accountId: string,
  onAuthenticationExpired: () => void,
) {
  const gateway = useWorkspaceGateway();
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const idempotencyLedger = useRef(new Map<string, IdempotencyEntry>());
  const mutation = useMutation<unknown, unknown, () => Promise<unknown>>({
    mutationFn: (run) => run(),
  });

  const clearLocalAccountState = useCallback(() => {
    clearAccountQueries(queryClient);
    idempotencyLedger.current.clear();
    clearDraftStorage();
  }, [queryClient]);

  const handleError = useCallback((reason: unknown) => {
    if (isAuthenticationError(reason)) {
      clearLocalAccountState();
      onAuthenticationExpired();
      return;
    }
    setError(errorMessage(reason));
  }, [clearLocalAccountState, onAuthenticationExpired]);

  const runAction = useCallback(async <T,>(run: () => Promise<T>): Promise<ActionResult<T>> => {
    setError(null);
    try {
      return { ok: true, value: await mutation.mutateAsync(run) as T };
    } catch (reason) {
      handleError(reason);
      return { ok: false, error: reason };
    }
  }, [handleError, mutation]);

  const keyFor = useCallback((slot: string, fingerprint: string): string => {
    const existing = idempotencyLedger.current.get(slot);
    if (existing?.fingerprint === fingerprint) return existing.key;
    const entry = { fingerprint, key: createIdempotencyKey() };
    idempotencyLedger.current.set(slot, entry);
    return entry.key;
  }, []);

  const completeIdempotentAction = useCallback((slot: string, fingerprint: string) => {
    if (idempotencyLedger.current.get(slot)?.fingerprint === fingerprint) {
      idempotencyLedger.current.delete(slot);
    }
  }, []);

  const invalidateActiveLists = useCallback(async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: workspaceKeys.models(accountId) }),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.conversations(accountId) }),
    ]);
  }, [accountId, queryClient]);

  const createConversation = useCallback(async (modelProfileId: string) => {
    const slot = "create-conversation";
    const fingerprint = modelProfileId;
    const result = await runAction(() => gateway.conversations.create(
      modelProfileId,
      keyFor(slot, fingerprint),
    ));
    if (!result.ok || !result.value) return undefined;
    completeIdempotentAction(slot, fingerprint);
    queryClient.setQueryData(
      workspaceKeys.conversation(accountId, result.value.conversation_id),
      result.value,
    );
    queryClient.setQueryData<ResearchConversationSummary[]>(
      workspaceKeys.conversations(accountId),
      (current) => updateSummaryList(current, result.value!),
    );
    return result.value;
  }, [accountId, completeIdempotentAction, gateway, keyFor, queryClient, runAction]);

  const updateConversation = useCallback(async (
    conversationId: string,
    changes: { title?: string; model_profile_id?: string },
  ): Promise<boolean> => {
    const result = await runAction(() => gateway.conversations.update(conversationId, changes));
    if (!result.ok || !result.value) return false;
    queryClient.setQueryData<ResearchConversationDetail>(
      workspaceKeys.conversation(accountId, conversationId),
      (current) => current ? { ...current, ...result.value } : current,
    );
    queryClient.setQueryData<ResearchConversationSummary[]>(
      workspaceKeys.conversations(accountId),
      (current) => updateSummaryList(current, result.value!),
    );
    return true;
  }, [accountId, gateway, queryClient, runAction]);

  const archiveConversation = useCallback(async (conversationId: string): Promise<boolean> => {
    const result = await runAction(() => gateway.conversations.archive(conversationId));
    if (!result.ok) return false;
    queryClient.removeQueries({ queryKey: workspaceKeys.conversation(accountId, conversationId) });
    queryClient.setQueryData<ResearchConversationSummary[]>(
      workspaceKeys.conversations(accountId),
      (current) => current?.filter((item) => item.conversation_id !== conversationId) ?? [],
    );
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: workspaceKeys.archivedConversations(accountId) }),
    ]);
    return true;
  }, [accountId, gateway, queryClient, runAction]);

  const sendResearchInput = useCallback(async (
    conversation: ResearchConversationDetail,
    text: string,
    answerStyle: ResearchAnswerStyle = "web_first",
  ): Promise<boolean> => {
    const decision = decideComposerAction(conversation);
    const slot = `composer:${conversation.conversation_id}`;
    const fingerprint = decision.kind === "dialogue_message"
      ? `message:${decision.turnId}:${decision.revision}:${text}`
      : `turn:${text}:${answerStyle}`;
    const idempotencyKey = keyFor(slot, fingerprint);
    const result = await runAction(() => decision.kind === "dialogue_message"
      ? gateway.conversations.submitMessage(
          conversation.conversation_id,
          decision.turnId,
          decision.revision,
          text,
          idempotencyKey,
        )
      : gateway.conversations.startTurn(
          conversation.conversation_id,
          text,
          answerStyle,
          idempotencyKey,
        ));
    if (!result.ok || !result.value) {
      if (
        result.error instanceof ResearchWorkspaceRequestError
        && (result.error.code === "dialogue_revision_conflict"
          || result.error.code === "turn_not_accepting_messages")
      ) {
        await queryClient.invalidateQueries({
          queryKey: workspaceKeys.conversation(accountId, conversation.conversation_id),
          exact: true,
        });
      }
      return false;
    }
    completeIdempotentAction(slot, fingerprint);
    const updatedConversation = replaceTurn(conversation, result.value);
    queryClient.setQueryData(
      workspaceKeys.conversation(accountId, conversation.conversation_id),
      updatedConversation,
    );
    queryClient.setQueryData<ResearchConversationSummary[]>(
      workspaceKeys.conversations(accountId),
      (current) => updateSummaryList(current, updatedConversation),
    );
    return true;
  }, [accountId, completeIdempotentAction, gateway, keyFor, queryClient, runAction]);

  const saveModel = useCallback(async (
    profileId: string | null,
    input: SaveModelProfileInput,
  ): Promise<boolean> => {
    const slot = "create-model";
    const fingerprint = JSON.stringify(input);
    const result = await runAction(() => profileId
      ? gateway.models.update(profileId, input)
      : gateway.models.create(input, keyFor(slot, fingerprint)));
    if (!result.ok) return false;
    if (!profileId) completeIdempotentAction(slot, fingerprint);
    await invalidateActiveLists();
    return true;
  }, [completeIdempotentAction, gateway, invalidateActiveLists, keyFor, runAction]);

  const runModelAction = useCallback(async (
    action: "verify" | "set-default" | "archive",
    profileId: string,
  ): Promise<boolean> => {
    const result = await runAction(() => {
      if (action === "verify") return gateway.models.verify(profileId);
      if (action === "set-default") return gateway.models.setDefault(profileId);
      return gateway.models.archive(profileId);
    });
    if (!result.ok) return false;
    await invalidateActiveLists();
    if (action === "archive") {
      await queryClient.invalidateQueries({ queryKey: workspaceKeys.archivedModels(accountId) });
    }
    return true;
  }, [accountId, gateway, invalidateActiveLists, queryClient, runAction]);

  const restoreModel = useCallback(async (profileId: string): Promise<boolean> => {
    const result = await runAction(() => gateway.models.restore(profileId));
    if (!result.ok) return false;
    await Promise.all([
      invalidateActiveLists(),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.archivedModels(accountId) }),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.archivedConversations(accountId) }),
    ]);
    return true;
  }, [accountId, gateway, invalidateActiveLists, queryClient, runAction]);

  const restoreConversation = useCallback(async (
    conversationId: string,
    modelProfileId?: string,
  ) => {
    const result = await runAction(() => gateway.conversations.restore(conversationId, modelProfileId));
    if (!result.ok || !result.value) return undefined;
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: workspaceKeys.conversations(accountId) }),
      queryClient.invalidateQueries({ queryKey: workspaceKeys.archivedConversations(accountId) }),
    ]);
    return result.value;
  }, [accountId, gateway, queryClient, runAction]);

  const logout = useCallback(async (): Promise<void> => {
    const result = await runAction(() => gateway.auth.logout());
    if (result.ok) clearLocalAccountState();
  }, [clearLocalAccountState, gateway, runAction]);

  const conversation = useMemo(() => ({
    create: createConversation,
    update: updateConversation,
    archive: archiveConversation,
    send: sendResearchInput,
    restore: restoreConversation,
  }), [archiveConversation, createConversation, restoreConversation, sendResearchInput, updateConversation]);

  const model = useMemo(() => ({
    save: saveModel,
    verify: (profileId: string) => runModelAction("verify", profileId),
    setDefault: (profileId: string) => runModelAction("set-default", profileId),
    archive: (profileId: string) => runModelAction("archive", profileId),
    restore: restoreModel,
  }), [restoreModel, runModelAction, saveModel]);

  return {
    conversation,
    model,
    logout,
    busy: mutation.isPending,
    error,
    clearError: useCallback(() => setError(null), []),
    handleError,
  };
}
