import { createContext, type ReactNode, useContext } from "react";
import {
  researchWorkspaceClient,
  type ArchivedModelProfile,
  type ArchivedResearchConversation,
  type ModelProfile,
  type ResearchAnswerStyle,
  type ResearchConversationDetail,
  type ResearchConversationSummary,
  type ResearchTraceAuditPage,
  type ResearchTraceSummary,
  type ResearchTurn,
  type SaveModelProfileInput,
  type UserAccount,
} from "../../research-workspace-client";

export interface AuthOperations {
  current(): Promise<UserAccount>;
  register(email: string, password: string, displayName: string): Promise<UserAccount>;
  login(email: string, password: string): Promise<UserAccount>;
  logout(): Promise<void>;
}

export interface ModelOperations {
  list(): Promise<ModelProfile[]>;
  create(input: SaveModelProfileInput, idempotencyKey: string): Promise<ModelProfile>;
  update(profileId: string, input: SaveModelProfileInput): Promise<ModelProfile>;
  setDefault(profileId: string): Promise<void>;
  verify(profileId: string): Promise<void>;
  archive(profileId: string): Promise<void>;
  listArchived(): Promise<ArchivedModelProfile[]>;
  restore(profileId: string): Promise<ModelProfile>;
}

export interface ConversationOperations {
  list(): Promise<ResearchConversationSummary[]>;
  create(modelProfileId: string | undefined, idempotencyKey: string): Promise<ResearchConversationDetail>;
  load(conversationId: string): Promise<ResearchConversationDetail>;
  update(
    conversationId: string,
    changes: { title?: string; model_profile_id?: string },
  ): Promise<ResearchConversationSummary>;
  archive(conversationId: string): Promise<void>;
  listArchived(): Promise<ArchivedResearchConversation[]>;
  restore(conversationId: string, modelProfileId?: string): Promise<ResearchConversationSummary>;
  startTurn(
    conversationId: string,
    question: string,
    answerStyle: ResearchAnswerStyle,
    idempotencyKey: string,
  ): Promise<ResearchTurn>;
  submitMessage(
    conversationId: string,
    turnId: string,
    revision: number,
    message: string,
    idempotencyKey: string,
  ): Promise<ResearchTurn>;
}

export interface TraceOperations {
  summary(conversationId: string, turnId: string, signal?: AbortSignal): Promise<ResearchTraceSummary>;
  audit(
    conversationId: string,
    turnId: string,
    options?: { stage?: string; cursor?: number; limit?: number; signal?: AbortSignal },
  ): Promise<ResearchTraceAuditPage>;
}

export interface WorkspaceGateway {
  auth: AuthOperations;
  models: ModelOperations;
  conversations: ConversationOperations;
  trace: TraceOperations;
}

export const httpWorkspaceGateway: WorkspaceGateway = {
  auth: {
    current: () => researchWorkspaceClient.currentAccount(),
    register: (email, password, displayName) => researchWorkspaceClient.registerAccount(email, password, displayName),
    login: (email, password) => researchWorkspaceClient.login(email, password),
    logout: () => researchWorkspaceClient.logout(),
  },
  models: {
    list: () => researchWorkspaceClient.listModelProfiles(),
    create: (input, idempotencyKey) => researchWorkspaceClient.createModelProfile(input, idempotencyKey),
    update: (profileId, input) => researchWorkspaceClient.updateModelProfile(profileId, input),
    setDefault: (profileId) => researchWorkspaceClient.setDefaultModelProfile(profileId),
    verify: (profileId) => researchWorkspaceClient.verifyModelProfile(profileId),
    archive: (profileId) => researchWorkspaceClient.archiveModelProfile(profileId),
    listArchived: () => researchWorkspaceClient.listArchivedModelProfiles(),
    restore: (profileId) => researchWorkspaceClient.restoreModelProfile(profileId),
  },
  conversations: {
    list: () => researchWorkspaceClient.listResearchConversations(),
    create: (modelProfileId, idempotencyKey) => researchWorkspaceClient.createResearchConversation(modelProfileId, idempotencyKey),
    load: (conversationId) => researchWorkspaceClient.loadResearchConversation(conversationId),
    update: (conversationId, changes) => researchWorkspaceClient.updateResearchConversation(conversationId, changes),
    archive: (conversationId) => researchWorkspaceClient.archiveResearchConversation(conversationId),
    listArchived: () => researchWorkspaceClient.listArchivedResearchConversations(),
    restore: (conversationId, modelProfileId) => researchWorkspaceClient.restoreResearchConversation(conversationId, modelProfileId),
    startTurn: (conversationId, question, answerStyle, idempotencyKey) => (
      researchWorkspaceClient.startResearchTurn(conversationId, question, answerStyle, idempotencyKey)
    ),
    submitMessage: (conversationId, turnId, revision, message, idempotencyKey) => (
      researchWorkspaceClient.submitDialogueMessage(
        conversationId,
        turnId,
        revision,
        message,
        idempotencyKey,
      )
    ),
  },
  trace: {
    summary: (conversationId, turnId, signal) => (
      researchWorkspaceClient.loadResearchTraceSummary(conversationId, turnId, signal)
    ),
    audit: (conversationId, turnId, options) => (
      researchWorkspaceClient.loadResearchTraceAudit(conversationId, turnId, options)
    ),
  },
};

const WorkspaceGatewayContext = createContext<WorkspaceGateway | null>(null);

export function WorkspaceGatewayProvider({
  gateway,
  children,
}: {
  gateway: WorkspaceGateway;
  children: ReactNode;
}) {
  return (
    <WorkspaceGatewayContext.Provider value={gateway}>
      {children}
    </WorkspaceGatewayContext.Provider>
  );
}

export function useWorkspaceGateway(): WorkspaceGateway {
  const gateway = useContext(WorkspaceGatewayContext);
  if (!gateway) throw new Error("WorkspaceGatewayProvider is missing");
  return gateway;
}
