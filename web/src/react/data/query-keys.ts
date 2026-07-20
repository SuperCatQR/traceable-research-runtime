export const workspaceKeys = {
  session: ["workspace", "session"] as const,
  models: (accountId: string) => ["workspace", accountId, "models"] as const,
  archivedModels: (accountId: string) => ["workspace", accountId, "archived-models"] as const,
  conversations: (accountId: string) => ["workspace", accountId, "conversations"] as const,
  conversation: (accountId: string, conversationId: string) => (
    ["workspace", accountId, "conversation", conversationId] as const
  ),
  archivedConversations: (accountId: string) => (
    ["workspace", accountId, "archived-conversations"] as const
  ),
  traceSummary: (accountId: string, conversationId: string, turnId: string) => (
    ["workspace", accountId, "trace-summary", conversationId, turnId] as const
  ),
  traceAudit: (accountId: string, conversationId: string, turnId: string, stage: string) => (
    ["workspace", accountId, "trace-audit", conversationId, turnId, stage] as const
  ),
};
