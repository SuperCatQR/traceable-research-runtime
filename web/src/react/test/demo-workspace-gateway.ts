import type {
  ArchivedModelProfile,
  ArchivedResearchConversation,
  ModelProfile,
  ResearchConversationDetail,
  ResearchConversationSummary,
  ResearchTraceAuditPage,
  ResearchTraceSummary,
  ResearchTurn,
  SaveModelProfileInput,
  UserAccount,
} from "../../research-workspace-client";
import type { WorkspaceGateway } from "../data/workspace-gateway";

const now = Math.floor(Date.now() / 1000);

const account: UserAccount = {
  user_id: "demo-account",
  email: "researcher@example.com",
  display_name: "研究者",
  created_at: now - 86400 * 30,
};

const initialModel: ModelProfile = {
  profile_id: "profile-primary",
  display_name: "GPT-5 Research",
  api_base_url: "https://api.example.com/v1/",
  model_id: "gpt-5-research",
  revision: 3,
  is_default: true,
  has_api_key: true,
  verified_at: now - 3600,
  created_at: now - 86400 * 20,
  updated_at: now - 3600,
};

const turnSeeds = [
  [
    "四天工作制更适合哪些类型的组织？",
    "证据更支持知识密集、目标可量化且团队自主性较高的组织。能否缩减会议、重排工作流程，比单纯减少一天出勤更关键。",
  ],
  [
    "现有试点是否覆盖制造业和公共服务？",
    "覆盖存在但明显少于专业服务行业。制造、医疗和客服通常需要连续排班，实施时更依赖轮班重构，而不是全员统一休息日。",
  ],
  [
    "生产率提升通常用什么指标衡量？",
    "研究常用营业收入、单位工时产出、项目交付速度和缺勤率。不同研究的指标口径不统一，因此更适合比较方向，不能直接合并成一个百分比。",
  ],
  [
    "哪些结论还不能直接外推？",
    "自愿参加试点的企业可能本来就更适合改革，且多数观察期不足两年。长期晋升、公平性和客户覆盖成本仍缺少稳定证据。",
  ],
  [
    "综合现有证据，给出实施前的判断清单。",
    "## 结论\n\n四天工作制不是简单压缩工时，而是一次工作系统重构。实施前应先验证任务可重排程度、连续服务约束、指标质量和管理授权。\n\n### 建议检查\n\n- 先用 8–12 周小规模试点建立基线。\n- 同时跟踪产出、质量、客户响应和员工负荷。\n- 为连续岗位设计错峰轮班，不强求统一休息日。\n- 预先定义退出条件，避免只保留正向指标。",
  ],
] as const;

function completeTurn(index: number): ResearchTurn {
  const [question, answer] = turnSeeds[index];
  return {
    turn_id: `turn-${index + 1}`,
    turn_number: index + 1,
    user_question: question,
    status: "completed",
    dialogue: {
      revision: 2,
      status: "research_started",
      messages: [
        { role: "user", text: question },
        { role: "assistant", text: "我会按组织类型、岗位连续性和证据质量拆分核查。" },
      ],
      failure: null,
    },
    answer: {
      answer,
      sources: [
        { title: "The results are in: the UK's four-day week pilot", url: "https://autonomy.work/portfolio/uk4dwpilotresults/" },
        { title: "Four-Day Workweek Trial", url: "https://www.nature.com/articles/s41562-024-01924-0" },
        { title: "Working Time Reduction", url: "https://www.ilo.org/" },
      ],
    },
    created_at: now - (5 - index) * 7200,
    updated_at: now - (5 - index) * 7000,
    completed_at: now - (5 - index) * 6900,
  };
}

const detail: ResearchConversationDetail = {
  conversation_id: "conversation-four-day-week",
  title: "四天工作制的适用边界",
  model_profile_id: initialModel.profile_id,
  model_profile_name: initialModel.display_name,
  turn_count: turnSeeds.length,
  latest_turn_status: "completed",
  created_at: now - 86400,
  updated_at: now - 1200,
  turns: turnSeeds.map((_, index) => completeTurn(index)),
};

function summaryFrom(conversation: ResearchConversationDetail): ResearchConversationSummary {
  const { turns: _turns, ...summary } = conversation;
  return summary;
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

export type DemoWorkspaceScenario = "complete" | "running" | "empty" | "error" | "long" | "setup";

export function resolveDemoWorkspaceScenario(value: string | null): DemoWorkspaceScenario {
  if (value === "running" || value === "empty" || value === "error" || value === "long" || value === "setup") return value;
  return "complete";
}

function detailForScenario(scenario: DemoWorkspaceScenario): ResearchConversationDetail {
  const conversation = clone(detail);
  const lastTurn = conversation.turns.at(-1);
  if (!lastTurn) return conversation;

  if (scenario === "running") {
    lastTurn.status = "running";
    lastTurn.answer = null;
    lastTurn.completed_at = null;
    lastTurn.updated_at = now;
    conversation.latest_turn_status = "running";
    conversation.updated_at = now;
  } else if (scenario === "error") {
    lastTurn.status = "failed";
    lastTurn.answer = null;
    lastTurn.dialogue = {
      revision: (lastTurn.dialogue?.revision ?? 0) + 1,
      status: "failed",
      messages: lastTurn.dialogue?.messages ?? [],
      failure: "Demo research failed before synthesis.",
    };
    lastTurn.completed_at = null;
    lastTurn.updated_at = now;
    conversation.latest_turn_status = "failed";
    conversation.updated_at = now;
  } else if (scenario === "long") {
    const longQuestion = "在跨国、跨时区并同时覆盖制造、医疗、客户支持与知识工作的组织中，如果必须维持连续服务、审计合规和员工公平，四天工作制的试点应如何分层设计、选择指标并设置退出条件？";
    conversation.title = "跨行业连续服务组织的四天工作制试点边界与长期评估";
    lastTurn.user_question = longQuestion;
    if (lastTurn.dialogue) {
      lastTurn.dialogue.messages = lastTurn.dialogue.messages.map((message, index) => (
        index === 0 && message.role === "user" ? { ...message, text: longQuestion } : message
      ));
    }
    if (lastTurn.answer) {
      lastTurn.answer.answer += "\n\n### 分层实施与退出条件\n\n先按岗位的连续服务约束、交接成本和结果可测量程度分层，不应让所有团队共享同一个休息日。每层都要同时记录产出、质量、客户响应、加班转移和员工负荷，并在试点前写明停止条件。\n\n跨时区团队还应检查工作是否只是被转移到未记录时段。若响应时延、缺陷率或高风险岗位的疲劳指标持续恶化，即使总工时下降，也不应扩大试点。";
    }
  }

  return conversation;
}

export function createDemoWorkspaceGateway(
  scenario: DemoWorkspaceScenario = "complete",
): WorkspaceGateway {
  let models = scenario === "setup" ? [] : [clone(initialModel)];
  let archivedModels: ArchivedModelProfile[] = [{
    ...clone(initialModel),
    profile_id: "profile-archive",
    display_name: "旧研究模型",
    model_id: "research-preview-v1",
    is_default: false,
    archived_at: now - 86400,
  }];
  let conversations = scenario === "empty" || scenario === "setup" ? [] : [detailForScenario(scenario)];
  let archivedConversations: ArchivedResearchConversation[] = [{
    ...summaryFrom(detail),
    conversation_id: "conversation-archive",
    title: "家庭储能购买风险",
    turn_count: 3,
    archived_at: now - 3600,
    model_profile_available: true,
  }];

  const findConversation = (id: string) => {
    const found = conversations.find((conversation) => conversation.conversation_id === id);
    if (!found) throw new Error("未找到请求的内容");
    return found;
  };

  return {
    auth: {
      current: async () => clone(account),
      register: async () => clone(account),
      login: async () => clone(account),
      logout: async () => undefined,
    },
    models: {
      list: async () => clone(models),
      create: async (input: SaveModelProfileInput) => {
        const created: ModelProfile = {
          profile_id: `profile-${models.length + 1}`,
          display_name: input.display_name,
          api_base_url: input.api_base_url,
          model_id: input.model_id,
          revision: 1,
          is_default: input.make_default === true || models.length === 0,
          has_api_key: Boolean(input.api_key),
          verified_at: null,
          created_at: now,
          updated_at: now,
        };
        if (created.is_default) models = models.map((model) => ({ ...model, is_default: false }));
        models = [created, ...models];
        return clone(created);
      },
      update: async (id, input) => {
        models = models.map((model) => model.profile_id === id ? {
          ...model,
          ...input,
          has_api_key: model.has_api_key || Boolean(input.api_key),
          revision: model.revision + 1,
          verified_at: null,
          updated_at: now,
        } : model);
        return clone(models.find((model) => model.profile_id === id)!);
      },
      setDefault: async (id) => {
        models = models.map((model) => ({ ...model, is_default: model.profile_id === id }));
      },
      verify: async (id) => {
        models = models.map((model) => model.profile_id === id ? { ...model, verified_at: now } : model);
      },
      archive: async (id) => {
        const model = models.find((candidate) => candidate.profile_id === id);
        if (!model) return;
        models = models.filter((candidate) => candidate.profile_id !== id);
        archivedModels = [{ ...model, is_default: false, archived_at: now }, ...archivedModels];
      },
      listArchived: async () => clone(archivedModels),
      restore: async (id) => {
        const archived = archivedModels.find((model) => model.profile_id === id)!;
        archivedModels = archivedModels.filter((model) => model.profile_id !== id);
        const restored = { ...archived, is_default: models.length === 0 };
        models = [restored, ...models];
        return clone(restored);
      },
    },
    conversations: {
      list: async () => conversations.map(summaryFrom).map(clone),
      create: async (profileId) => {
        const model = models.find((candidate) => candidate.profile_id === profileId) ?? models[0];
        const created: ResearchConversationDetail = {
          conversation_id: `conversation-${conversations.length + 1}`,
          title: "新的研究",
          model_profile_id: model.profile_id,
          model_profile_name: model.display_name,
          turn_count: 0,
          latest_turn_status: null,
          created_at: now,
          updated_at: now,
          turns: [],
        };
        conversations = [created, ...conversations];
        return clone(created);
      },
      load: async (id) => clone(findConversation(id)),
      update: async (id, changes) => {
        const model = changes.model_profile_id
          ? models.find((candidate) => candidate.profile_id === changes.model_profile_id)
          : undefined;
        conversations = conversations.map((conversation) => conversation.conversation_id === id ? {
          ...conversation,
          ...changes,
          ...(model ? { model_profile_name: model.display_name } : {}),
          updated_at: now,
        } : conversation);
        return clone(summaryFrom(findConversation(id)));
      },
      archive: async (id) => {
        const conversation = findConversation(id);
        conversations = conversations.filter((candidate) => candidate.conversation_id !== id);
        archivedConversations = [{ ...summaryFrom(conversation), archived_at: now, model_profile_available: true }, ...archivedConversations];
      },
      listArchived: async () => clone(archivedConversations),
      restore: async (id, profileId) => {
        const archived = archivedConversations.find((conversation) => conversation.conversation_id === id)!;
        const model = models.find((candidate) => candidate.profile_id === (profileId ?? archived.model_profile_id)) ?? models[0];
        archivedConversations = archivedConversations.filter((conversation) => conversation.conversation_id !== id);
        const restored: ResearchConversationDetail = {
          ...archived,
          model_profile_id: model.profile_id,
          model_profile_name: model.display_name,
          turns: [],
        };
        conversations = [restored, ...conversations];
        return clone(summaryFrom(restored));
      },
      startTurn: async (id, question) => {
        const conversation = findConversation(id);
        const turn: ResearchTurn = {
          turn_id: `turn-${conversation.turns.length + 1}`,
          turn_number: conversation.turns.length + 1,
          user_question: question,
          status: "clarifying",
          answer: null,
          dialogue: {
            revision: 1,
            status: "awaiting_message",
            messages: [
              { role: "user", text: question },
              { role: "assistant", text: "你希望我优先比较长期证据，还是先整理近期可执行建议？" },
            ],
            failure: null,
          },
          created_at: now,
          updated_at: now,
          completed_at: null,
        };
        conversation.turns.push(turn);
        conversation.turn_count = conversation.turns.length;
        conversation.latest_turn_status = turn.status;
        return clone(turn);
      },
      submitMessage: async (id, turnId, _revision, message) => {
        const conversation = findConversation(id);
        const turn = conversation.turns.find((candidate) => candidate.turn_id === turnId)!;
        turn.dialogue = {
          revision: (turn.dialogue?.revision ?? 0) + 1,
          status: "research_started",
          messages: [...(turn.dialogue?.messages ?? []), { role: "user", text: message }, { role: "assistant", text: "信息已经足够，我会开始研究。" }],
          failure: null,
        };
        turn.status = "ready";
        turn.updated_at = now;
        conversation.latest_turn_status = "ready";
        return clone(turn);
      },
    },
    trace: {
      summary: async (): Promise<ResearchTraceSummary> => ({
        model_id: initialModel.model_id,
        understanding: {
          message: "比较四天工作制在不同组织与岗位条件下的适用边界",
          rationale: "按组织类型、连续服务约束、衡量指标与外推限制拆分。",
        },
        rounds: [
          { round: 1, directions: ["跨国试点的组织类型", "连续服务岗位的实施方式"], search_result_count: 18 },
          { round: 2, directions: ["长期生产率指标", "样本选择与外推限制"], search_result_count: 14 },
        ],
        archived_source_count: 11,
        skipped_source_count: 5,
        selected_sources: [
          { title: "UK four-day week pilot results", url: "https://autonomy.work/", rationale: "提供多组织试点的共同指标。" },
          { title: "International Labour Organization", url: "https://www.ilo.org/", rationale: "用于核对工时与连续服务约束。" },
        ],
        synthesis_rationale: "将效果证据与行业限制并列，避免把知识型团队的结果直接外推到连续岗位。",
        failure: null,
      }),
      audit: async (_conversationId, _turnId, options): Promise<ResearchTraceAuditPage> => ({
        next_cursor: options?.cursor === undefined ? 3 : null,
        entries: options?.cursor === undefined ? [
          { sequence: 1, occurred_at: new Date((now - 300) * 1000).toISOString(), stage: "dialogue", label: "问题理解完成", detail: "识别组织类型、岗位连续性与证据质量三个比较维度。", rationale: "这些维度决定结论能否外推。" },
          { sequence: 2, occurred_at: new Date((now - 240) * 1000).toISOString(), stage: "search", label: "完成首轮检索", detail: "Google 返回 18 条导航结果。", rationale: null },
          { sequence: 3, occurred_at: new Date((now - 180) * 1000).toISOString(), stage: "selection", label: "选定主要来源", detail: "选定 3 个覆盖试点、劳动政策与长期指标的来源。", rationale: "来源之间职责互补。" },
        ] : [
          { sequence: 4, occurred_at: new Date((now - 120) * 1000).toISOString(), stage: "synthesis", label: "完成结论综合", detail: "答案区分支持证据、适用条件与外推限制。", rationale: "避免把试点相关性表述为普遍因果。" },
        ],
      }),
    },
  };
}
