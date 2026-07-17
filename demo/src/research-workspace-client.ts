export type ResearchTurnStatus =
  | "clarifying"
  | "ready"
  | "running"
  | "completed"
  | "failed"
  | "cancelled";

export type DialogueStatus = "thinking" | "awaiting_message" | "research_started" | "failed" | "cancelled";
export type ResearchAnswerStyle = "web_first" | "knowledge_first";
export type ResearchClaimOrigin = "model_knowledge" | "web_evidence";
export type RationaleAuditStatus = "legacy_unverified" | "required_and_validated";

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

export interface EvidenceSource {
  url: string;
  title: string;
}

export interface ResearchClaim {
  text: string;
  origin: ResearchClaimOrigin;
  rationale: string;
  sources: EvidenceSource[];
}

export interface ResearchAnswer {
  answer_style: ResearchAnswerStyle;
  answer: string;
  knowledge_draft: {
    answer: string;
    claims: string[];
    uncertainty: string;
    basis_summary: string;
  };
  comparison: {
    agreements: string[];
    differences: string[];
    synthesis_rationale: string;
  };
  claims: ResearchClaim[];
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
  run_id: string | null;
  clarification_rationale_audit_status: RationaleAuditStatus;
  research_rationale_audit_status: RationaleAuditStatus | null;
  understanding: TraceUnderstanding | null;
  rounds: TraceRoundSummary[];
  archived_source_count: number;
  skipped_source_count: number;
  selected_sources: TraceSourceSummary[];
  synthesis_rationale: string | null;
  failure: { stage: string; message: string } | null;
}

export interface TraceAuditEntry {
  stage: "dialogue" | "setup" | "planning" | "search" | "archive" | "selection" | "synthesis" | "failure";
  label: string;
  detail: string;
  rationale: string | null;
}

export interface ResearchTraceAuditPage {
  run_id: string | null;
  next_cursor: number | null;
  entries: TraceAuditEntry[];
}

export interface ResearchTurn {
  turn_id: string;
  turn_number: number;
  run_id: string | null;
  user_question: string;
  status: ResearchTurnStatus;
  answer_style: ResearchAnswerStyle;
  model_profile_id: string;
  model_api_base_url: string;
  model_id: string;
  answer: ResearchAnswer | null;
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

export class ResearchWorkspaceRequestError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly retryable: boolean,
  ) {
    super(message);
    this.name = "ResearchWorkspaceRequestError";
  }
}

export class ResearchWorkspaceClient {
  constructor(private readonly apiBaseUrl = "") {}

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

  createModelProfile(profile: SaveModelProfileInput): Promise<ModelProfile> {
    return this.requestJson("/api/model-profiles", { method: "POST", body: profile });
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

  listResearchConversations(): Promise<ResearchConversationSummary[]> {
    return this.requestJson("/api/conversations");
  }

  createResearchConversation(modelProfileId?: string): Promise<ResearchConversationDetail> {
    return this.requestJson("/api/conversations", {
      method: "POST",
      body: { model_profile_id: modelProfileId },
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

  startResearchTurn(
    conversationId: string,
    question: string,
    answerStyle: ResearchAnswerStyle,
  ): Promise<ResearchTurn> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns`,
      { method: "POST", body: { question, answer_style: answerStyle } },
    );
  }

  submitDialogueMessage(
    conversationId: string,
    turnId: string,
    revision: number,
    message: string,
  ): Promise<ResearchTurn> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/messages`,
      { method: "POST", body: { revision, message } },
    );
  }

  loadResearchTraceSummary(conversationId: string, turnId: string): Promise<ResearchTraceSummary> {
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/trace/summary`,
    );
  }

  loadResearchTraceAudit(
    conversationId: string,
    turnId: string,
    options: { stage?: string; cursor?: number } = {},
  ): Promise<ResearchTraceAuditPage> {
    const query = new URLSearchParams();
    if (options.stage) query.set("stage", options.stage);
    if (options.cursor !== undefined) query.set("cursor", String(options.cursor));
    const suffix = query.size ? `?${query.toString()}` : "";
    return this.requestJson(
      `/api/conversations/${encodeURIComponent(conversationId)}/turns/${encodeURIComponent(turnId)}/trace/audit${suffix}`,
    );
  }

  private async requestJson<T>(
    requestPath: string,
    options: { method?: string; body?: unknown } = {},
  ): Promise<T> {
    const response = await this.send(requestPath, options);
    return response.json() as Promise<T>;
  }

  private async requestWithoutResponse(
    requestPath: string,
    options: { method?: string; body?: unknown } = {},
  ): Promise<void> {
    await this.send(requestPath, options);
  }

  private async send(
    requestPath: string,
    options: { method?: string; body?: unknown },
  ): Promise<Response> {
    const response = await fetch(`${this.apiBaseUrl}${requestPath}`, {
      method: options.method ?? "GET",
      credentials: "same-origin",
      headers: options.body === undefined ? undefined : { "Content-Type": "application/json" },
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
    });
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
  (import.meta.env.VITE_API_BASE_URL as string | undefined)?.replace(/\/$/, "") ?? "";

export const researchWorkspaceClient = new ResearchWorkspaceClient(configuredApiBaseUrl);
