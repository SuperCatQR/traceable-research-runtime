export type DemoScenarioId = "first-use" | "research" | "recovery";

export type WorkspaceView = "answer" | "execution" | "audit";

export type AnswerMode = "evidence-first" | "model-led";

export type RequestStatus =
  | "awaiting-clarification"
  | "ready"
  | "running"
  | "completed"
  | "interrupted"
  | "failed"
  | "cancelled";

export type PhaseStatus = "complete" | "current" | "pending" | "attention";

export type AnswerSourceKind = "evidence" | "model" | "hybrid";

export interface CorpusSnapshot {
  id: string;
  name: string;
  versionLabel: string;
  publishedAt: string;
  documentCount: number;
  contentHash: string;
  availability: "available" | "unavailable";
}

export interface ResearchPhase {
  id: string;
  label: string;
  detail: string;
  status: PhaseStatus;
}

export interface ResearchCounts {
  navigationBranches: number;
  selectedDocuments: number;
  segmentReads: number;
  acceptedEvidence: number;
}

export interface Citation {
  id: string;
  claimId: string;
  documentId: string;
  documentTitle: string;
  sectionHeading: string;
  quote: string;
  versionHash: string;
  traceSequence: number;
}

export interface AnswerBlock {
  id: string;
  label: string;
  kind: AnswerSourceKind;
  text: string;
  citationIds: string[];
  modelNotice?: string;
}

export interface CoverageGap {
  id: string;
  priority: "high" | "medium";
  status: "disclosed" | "unresolved" | "resolved";
  question: string;
  explanation: string;
}

export interface ResearchAnswer {
  mode: AnswerMode;
  title: string;
  summary: string;
  blocks: AnswerBlock[];
}

export interface AuditItem {
  sequence: number;
  eventType: string;
  summary: string;
  time: string;
}

export interface ResearchRequest {
  id: string;
  number: number;
  shortTitle: string;
  originalQuestion: string;
  clarifiedQuestion: string;
  status: RequestStatus;
  statusLabel: string;
  snapshot: CorpusSnapshot;
  requestedModes: AnswerMode[];
  phases: ResearchPhase[];
  counts: ResearchCounts;
  selectedNavigationLabels: string[];
  selectedDocumentTitles: string[];
  stopReason: string;
  answers: ResearchAnswer[];
  citations: Citation[];
  coverageGaps: CoverageGap[];
  audit: AuditItem[];
  updatedAt: string;
}

export interface ResearchConversation {
  id: string;
  title: string;
  requests: ResearchRequest[];
}

export interface DemoFixtures {
  snapshots: CorpusSnapshot[];
  conversations: ResearchConversation[];
  normalRequest: ResearchRequest;
  recoveryRequest: ResearchRequest;
}
