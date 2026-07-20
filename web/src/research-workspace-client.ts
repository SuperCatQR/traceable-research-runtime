export type ResearchTurnStatus =
  | "clarifying"
  | "ready"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type DialogueStatus = "thinking" | "awaiting_message" | "research_started" | "failed" | "cancelled";
export type ResearchAnswerStyle = "web_first" | "knowledge_first";

export interface UserAccount {
  user_id: string;
  email: string;
  display_name: string;
  created_at: number;
}

export interface ModelProfile {
  profile_id: string;
  display_name: string;
  api_base_url: string;
  model_id: string;
  revision: number;
  is_default: boolean;
  has_api_key: boolean;
  verified_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface ArchivedModelProfile extends ModelProfile {
  archived_at: number;
}

export interface EvidenceSource {
  url: string;
  title: string;
}

export interface ChatResearchAnswer {
  answer: string;
  sources: EvidenceSource[];
}

export interface DialogueMessage {
  role: "user" | "assistant";
  text: string;
}

export interface TurnDialogue {
  revision: number;
  status: DialogueStatus;
  messages: DialogueMessage[];
  failure: string | null;
}

export interface TraceUnderstanding {
  message: string;
  rationale: string;
}

export interface TraceRoundSummary {
  round: number;
  directions: string[];
  search_result_count: number;
}

export interface TraceSourceSummary {
  title: string;
  url: string;
  rationale: string;
}

export interface ResearchTraceSummary {
  model_id: string;
  understanding: TraceUnderstanding | null;
  rounds: TraceRoundSummary[];
  archived_source_count: number;
  skipped_source_count: number;
  selected_sources: TraceSourceSummary[];
  synthesis_rationale: string | null;
  failure: { stage: string; message: string } | null;
}

export interface TraceAuditEntry {
  sequence: number | null;
  occurred_at: string | null;
  stage: "dialogue" | "setup" | "planning" | "search" | "archive" | "selection" | "synthesis" | "failure";
  label: string;
  detail: string;
  rationale: string | null;
}

export interface ResearchTraceAuditPage {
  next_cursor: number | null;
  entries: TraceAuditEntry[];
}

export interface ResearchTurn {
  turn_id: string;
  turn_number: number;
  user_question: string;
  status: ResearchTurnStatus;
  answer: ChatResearchAnswer | null;
  dialogue: TurnDialogue | null;
  created_at: number;
  updated_at: number;
  completed_at: number | null;
}

export interface ResearchConversationSummary {
  conversation_id: string;
  title: string;
  model_profile_id: string;
  model_profile_name: string;
  turn_count: number;
  latest_turn_status: ResearchTurnStatus | null;
  created_at: number;
  updated_at: number;
}

export interface ResearchConversationDetail extends ResearchConversationSummary {
  turns: ResearchTurn[];
}

export interface ArchivedResearchConversation extends ResearchConversationSummary {
  archived_at: number;
  model_profile_available: boolean;
}

export interface SaveModelProfileInput {
  display_name: string;
  api_base_url: string;
  api_key?: string;
  model_id: string;
  make_default?: boolean;
}

interface ApiErrorPayload {
  code?: string;
  message?: string;
  retryable?: boolean;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
  idempotencyKey?: string;
  signal?: AbortSignal;
}

export function createIdempotencyKey(): string {
  return globalThis.crypto.randomUUID();
}

export class ResearchWorkspaceRequestError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryable: boolean;

  constructor(
    message: string,
    status: number,
    code: string,
    retryable: boolean,
  ) {
    super(message);
    this.name = "ResearchWorkspaceRequestError";
    this.status = status;
    this.code = code;
    this.retryable = retryable;
  }
}

export class ResearchWorkspaceClient {
  private readonly apiBaseUrl: string;

  constructor(apiBaseUrl = "") {
    this.apiBaseUrl = apiBaseUrl;
  }

  currentAccount(): Promise<UserAccount> {
    return this.requestJson("/api/auth/me");
  }

  registerAccount(email: string, password: string, displayName: string): Promise<UserAccount> {
    return this.requestJson("/api/auth/register", {
      method: "POST",
      body: { email, password, display_name: displayName },
    });
  }

  login(email: string, password: string): Promise<UserAccount> {
    return this.requestJson("/api/auth/login", {
      method: "POST",
      body: { email, password },
    });
  }

  logout(): Promise<void> {
    return this.requestWithoutResponse("/api/auth/logout", { method: "POST" });
  }

  listModelProfiles(): Promise<ModelProfile[]> {
    return this.requestJson("/api/model-profiles");
  }

  createModelProfile(
    profile: SaveModelProfileInput,
    idempotencyKey = createIdempotencyKey(),
  ): Promise<ModelProfile> {
    return this.requestJson("/api/model-profiles", {
      method: "POST",
      body: profile,
      idempotencyKey,
    });
  }

  updateModelProfile(profileId: string, profile: SaveModelProfileInput): Promise<ModelProfile> {
    const { make_default: _ignored, ...editableFields } = profile;
    return this.requestJson(`/api/model-profiles/${encodeURIComponent(profileId)}`, {
      method: "PATCH",
      body: editableFields,
    });
  }

  setDefaultModelProfile(profileId: string): Promise<void> {
    return this.requestWithoutResponse(
      `/api/model-profiles/${encodeURIComponent(profileId)}/default`,
      { method: "POST" },
    );
  }

  verifyModelProfile(profileId: string): Promise<void> {
    return this.requestWithoutResponse(
      `/api/model-profiles/${encodeURIComponent(profileId)}/verify`,
      { method: "POST" },
    );
  }

  archiveModelProfile(profileId: string): Promise<void> {
    return this.requestWithoutResponse(`/api/model-profiles/${encodeURIComponent(profileId)}`, {
      method: "DELETE",
    });
  }

  listArchivedModelProfiles(): Promise<ArchivedModelProfile[]> {
    return this.requestJson("/api/archives/model-profiles");
  }

  restoreModelProfile(profileId: string): Promise<ModelProfile> {
    return this.requestJson(`/api/model-profiles/${encodeURIComponent(profileId)}/restore`, {
      method: "POST",
    });
  }

  listResearchConversations(): Promise<ResearchConversationSummary[]> {
    return this.requestJson("/api/conversations");
  }

  createResearchConversation(
    modelProfileId?: string,
    idempotencyKey = createIdempotencyKey(),
  ): Promise<ResearchConversationDetail> {
    return this.requestJson("/api/conversations", {
      method: "POST",
      body: { model_profile_id: modelProfileId },
      idempotencyKey,
    });
  }

  loadResearchConversation(conversationId: string): Promise<ResearchConversationDetail> {
    return this.requestJson(`/api/conversations/${encodeURIComponent(conversationId)}`);
  }

  updateResearchConversation(
    conversationId: string,
    changes: { title?: string; model_profile_id?: string },
  ): Promise<ResearchConversationSummary> {
    return this.requestJson(`/api/conversations/${encodeURIComponent(conversationId)}`, {
      method: "PATCH",
      body: changes,
    });
  }

  archiveResearchConversation(conversationId: string): Promise<void> {
    return this.requestWithoutResponse(`/api/conversations/${encodeURIComponent(conversationId)}`, {
      method: "DELETE",
    });
  }

  listArchivedResearchConversations(): Promise<ArchivedResearchConversation[]> {
    return this.requestJson("/api/archives/conversations");
  }

  restoreResearchConversation(
    conversationId: string,
    modelProfileId?: string,
  ): Promise<ResearchConversationSummary> {
    return this.requestJson(`/api/conversations/${encodeURIComponent(conversationId)}/restore`, {
      method: "POST",
      body: { model_profile_id: modelProfileId },
    });
  }

  startResearchTurn(
    conversationId: string,
    question: string,
    answerStyle: ResearchAnswerStyle,
    idempotencyKey = createIdempotencyKey(),
  ): Promise<ResearchTurn> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns`,
      { method: "POST", body: { question, answer_style: answerStyle }, idempotencyKey },
    );
  }

  submitDialogueMessage(
    conversationId: string,
    turnId: string,
    revision: number,
    message: string,
    idempotencyKey = createIdempotencyKey(),
  ): Promise<ResearchTurn> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/messages`,
      { method: "POST", body: { revision, message }, idempotencyKey },
    );
  }

  loadResearchTraceSummary(
    conversationId: string,
    turnId: string,
    signal?: AbortSignal,
  ): Promise<ResearchTraceSummary> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/trace/summary`,
      { signal },
    );
  }

  loadResearchTraceAudit(
    conversationId: string,
    turnId: string,
    options: { stage?: string; cursor?: number; limit?: number; signal?: AbortSignal } = {},
  ): Promise<ResearchTraceAuditPage> {
    const query = new URLSearchParams();
    if (options.stage) query.set("stage", options.stage);
    if (options.cursor !== undefined) query.set("cursor", String(options.cursor));
    if (options.limit !== undefined) query.set("limit", String(options.limit));
    const suffix = query.size ? `?${query.toString()}` : "";
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/trace/audit${suffix}`,
      { signal: options.signal },
    );
  }

  private async requestJson<T>(
    requestPath: string,
    options: RequestOptions = {},
  ): Promise<T> {
    const response = await this.send(requestPath, options);
    return response.json() as Promise<T>;
  }

  private async requestWithoutResponse(
    requestPath: string,
    options: RequestOptions = {},
  ): Promise<void> {
    await this.send(requestPath, options);
  }

  private async send(
    requestPath: string,
    options: RequestOptions,
  ): Promise<Response> {
    const headers = new Headers();
    if (options.body !== undefined) headers.set("Content-Type", "application/json");
    if (options.idempotencyKey) headers.set("Idempotency-Key", options.idempotencyKey);
    let response: Response;
    try {
      response = await fetch(`${this.apiBaseUrl}${requestPath}`, {
        method: options.method ?? "GET",
        credentials: "same-origin",
        headers,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
        signal: options.signal,
      });
    } catch (error) {
      if (options.signal?.aborted) throw error;
      throw new ResearchWorkspaceRequestError(
        "网络不可用，请检查连接后重试",
        0,
        "network_unavailable",
        true,
      );
    }
    if (response.ok) return response;

    const fallbackMessage = `请求失败（HTTP ${response.status}）`;
    const payload = await response.json().catch(() => ({})) as ApiErrorPayload;
    throw new ResearchWorkspaceRequestError(
      typeof payload.message === "string" ? payload.message : fallbackMessage,
      response.status,
      typeof payload.code === "string" ? payload.code : "request_failed",
      payload.retryable === true,
    );
  }
}

const configuredApiBaseUrl =
  (import.meta.env?.VITE_API_BASE_URL as string | undefined)?.replace(/\/$/, "") ?? "";

export const researchWorkspaceClient = new ResearchWorkspaceClient(configuredApiBaseUrl);
