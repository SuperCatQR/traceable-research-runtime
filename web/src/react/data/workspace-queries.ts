import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { ResearchWorkspaceRequestError, type UserAccount } from "../../research-workspace-client";
import { activeConversationPollDelay } from "../domain/turn-presentation";
import { useDocumentVisibility } from "../shared/use-document-visibility";
import { workspaceKeys } from "./query-keys";
import { useWorkspaceGateway } from "./workspace-gateway";

export function isAuthenticationError(error: unknown): boolean {
  return error instanceof ResearchWorkspaceRequestError && error.status === 401;
}

export function errorMessage(error: unknown, fallback = "请求未能完成"): string {
  return error instanceof Error ? error.message : fallback;
}

export function clearAccountQueries(queryClient: QueryClient): void {
  queryClient.removeQueries({ queryKey: ["workspace"], exact: false });
  queryClient.setQueryData<UserAccount | null>(workspaceKeys.session, null);
}

export function useSessionQuery() {
  const gateway = useWorkspaceGateway();
  return useQuery({
    queryKey: workspaceKeys.session,
    queryFn: async (): Promise<UserAccount | null> => {
      try {
        return await gateway.auth.current();
      } catch (error) {
        if (isAuthenticationError(error)) return null;
        throw error;
      }
    },
    retry: false,
    staleTime: 60_000,
  });
}

export function useModelsQuery(accountId: string) {
  const gateway = useWorkspaceGateway();
  return useQuery({
    queryKey: workspaceKeys.models(accountId),
    queryFn: () => gateway.models.list(),
    enabled: Boolean(accountId),
  });
}

export function useConversationsQuery(accountId: string) {
  const gateway = useWorkspaceGateway();
  return useQuery({
    queryKey: workspaceKeys.conversations(accountId),
    queryFn: () => gateway.conversations.list(),
    enabled: Boolean(accountId),
  });
}

export function useConversationQuery(accountId: string, conversationId: string | undefined) {
  const gateway = useWorkspaceGateway();
  const pageIsHidden = useDocumentVisibility();
  return useQuery({
    queryKey: workspaceKeys.conversation(accountId, conversationId ?? "none"),
    queryFn: () => gateway.conversations.load(conversationId!),
    enabled: Boolean(accountId && conversationId),
    refetchInterval: (query) => activeConversationPollDelay(query.state.data, pageIsHidden),
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: true,
  });
}

export function useArchivedModelsQuery(accountId: string, enabled: boolean) {
  const gateway = useWorkspaceGateway();
  return useQuery({
    queryKey: workspaceKeys.archivedModels(accountId),
    queryFn: () => gateway.models.listArchived(),
    enabled: Boolean(accountId && enabled),
  });
}

export function useArchivedConversationsQuery(accountId: string, enabled: boolean) {
  const gateway = useWorkspaceGateway();
  return useQuery({
    queryKey: workspaceKeys.archivedConversations(accountId),
    queryFn: () => gateway.conversations.listArchived(),
    enabled: Boolean(accountId && enabled),
  });
}

export function useTraceSummaryQuery(
  accountId: string,
  conversationId: string | undefined,
  turnId: string | undefined,
  enabled: boolean,
  refreshToken?: number,
) {
  const gateway = useWorkspaceGateway();
  const queryClient = useQueryClient();
  const lastObserved = useRef<{ turnId?: string; refreshToken?: number }>({});
  useEffect(() => {
    if (!enabled || !turnId) return;
    const previous = lastObserved.current;
    lastObserved.current = { turnId, refreshToken };
    if (previous.turnId !== turnId || previous.refreshToken === refreshToken) return;
    void queryClient.invalidateQueries({
      queryKey: workspaceKeys.traceSummary(accountId, conversationId ?? "none", turnId),
      exact: true,
    });
  }, [accountId, conversationId, enabled, queryClient, refreshToken, turnId]);
  return useQuery({
    queryKey: workspaceKeys.traceSummary(accountId, conversationId ?? "none", turnId ?? "none"),
    queryFn: ({ signal }) => gateway.trace.summary(conversationId!, turnId!, signal),
    enabled: Boolean(accountId && conversationId && turnId && enabled),
    retry: 1,
  });
}

export function useTraceAuditQuery(
  accountId: string,
  conversationId: string | undefined,
  turnId: string | undefined,
  stage: string,
  enabled: boolean,
) {
  const gateway = useWorkspaceGateway();
  return useInfiniteQuery({
    queryKey: workspaceKeys.traceAudit(accountId, conversationId ?? "none", turnId ?? "none", stage),
    initialPageParam: undefined as number | undefined,
    queryFn: ({ pageParam, signal }) => gateway.trace.audit(conversationId!, turnId!, {
      stage: stage || undefined,
      cursor: pageParam,
      signal,
    }),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: Boolean(accountId && conversationId && turnId && enabled),
    retry: 1,
  });
}
