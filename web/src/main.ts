import {
  Archive,
  ChevronRight,
  Check,
  CircleAlert,
  CircleCheck,
  KeyRound,
  LoaderCircle,
  LogOut,
  Menu,
  PanelRightOpen,
  Pencil,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Send,
  Settings2,
  ShieldCheck,
  SquarePen,
  X,
  createIcons,
} from "lucide";
import {
  ResearchWorkspaceRequestError,
  researchWorkspaceClient,
  type ModelProfile,
  type ResearchConversationDetail,
  type ResearchConversationSummary,
  type ResearchTraceAuditPage,
  type ResearchTraceSummary,
  type ResearchTurn,
  type ResearchTurnStatus,
  type UserAccount,
} from "./research-workspace-client";
import { ResearchTraceAuditCache } from "./research-trace-audit-cache";
import "./styles.css";

type AuthenticationMode = "login" | "register";
type ResearchInspectorView = "summary" | "audit";

interface WorkspaceState {
  account: UserAccount | null;
  modelProfiles: ModelProfile[];
  conversations: ResearchConversationSummary[];
  activeConversation: ResearchConversationDetail | null;
  authenticationMode: AuthenticationMode;
  editingModelProfileId: string | null;
  modelSettingsAreOpen: boolean;
  conversationSidebarIsOpen: boolean;
  conversationTitleIsBeingEdited: boolean;
  activeOperation: string | null;
  errorMessage: string | null;
  isBooting: boolean;
  researchInspectorIsOpen: boolean;
  researchInspectorTurnId: string | null;
  researchInspectorView: ResearchInspectorView;
  researchTraceSummaries: Map<string, ResearchTraceSummary>;
  researchTraceAudits: ResearchTraceAuditCache<ResearchTraceAuditPage>;
  researchTraceLoading: boolean;
  researchTraceError: string | null;
  researchTraceAuditStage: string;
}

const queriedApplicationRoot = document.querySelector<HTMLElement>("#app");
if (!queriedApplicationRoot) throw new Error("Missing #app");
const applicationRoot: HTMLElement = queriedApplicationRoot;

const workspaceState: WorkspaceState = {
  account: null,
  modelProfiles: [],
  conversations: [],
  activeConversation: null,
  authenticationMode: "login",
  editingModelProfileId: null,
  modelSettingsAreOpen: false,
  conversationSidebarIsOpen: false,
  conversationTitleIsBeingEdited: false,
  activeOperation: null,
  errorMessage: null,
  isBooting: true,
  researchInspectorIsOpen: false,
  researchInspectorTurnId: null,
  researchInspectorView: "summary",
  researchTraceSummaries: new Map(),
  researchTraceAudits: new ResearchTraceAuditCache(),
  researchTraceLoading: false,
  researchTraceError: null,
  researchTraceAuditStage: "",
};

const statusLabels: Record<ResearchTurnStatus, string> = {
  clarifying: "理解中",
  ready: "即将开始",
  running: "检索中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
};

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>"']/g,
    (character) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#039;" })[
        character
      ]!,
  );
}

function safeEvidenceUrl(value: string): string | null {
  try {
    const parsedUrl = new URL(value);
    return ["http:", "https:"].includes(parsedUrl.protocol) ? parsedUrl.href : null;
  } catch {
    return null;
  }
}

function clearLoadedResearchTraces(): void {
  workspaceState.researchInspectorIsOpen = false;
  workspaceState.researchInspectorTurnId = null;
  workspaceState.researchInspectorView = "summary";
  workspaceState.researchTraceSummaries.clear();
  workspaceState.researchTraceAudits.clear();
  workspaceState.researchTraceLoading = false;
  workspaceState.researchTraceError = null;
  workspaceState.researchTraceAuditStage = "";
}

function renderWorkspaceWithoutLosingResearchDraft(): void {
  const composer = applicationRoot.querySelector<HTMLTextAreaElement>("#research-question");
  const draft = composer?.value;
  const composerHadFocus = composer === document.activeElement;
  const selectionStart = composer?.selectionStart ?? 0;
  const selectionEnd = composer?.selectionEnd ?? 0;
  const transcript = applicationRoot.querySelector<HTMLElement>("#conversation-transcript");
  const transcriptScrollTop = transcript?.scrollTop;
  renderApplication();

  const replacementComposer = applicationRoot.querySelector<HTMLTextAreaElement>("#research-question");
  if (draft !== undefined && replacementComposer) {
    replacementComposer.value = draft;
    replacementComposer.style.height = "auto";
    replacementComposer.style.height = `${Math.min(replacementComposer.scrollHeight, 144)}px`;
    if (composerHadFocus && !replacementComposer.disabled) {
      replacementComposer.focus();
      replacementComposer.setSelectionRange(selectionStart, selectionEnd);
    }
  }
  if (transcriptScrollTop !== undefined) {
    const replacementTranscript = applicationRoot.querySelector<HTMLElement>("#conversation-transcript");
    if (replacementTranscript) replacementTranscript.scrollTop = transcriptScrollTop;
  }
}

function formatActivityDate(timestamp: number): string {
  const activityDate = new Date(timestamp * 1000);
  const today = new Date();
  const sharesCalendarDate = activityDate.toDateString() === today.toDateString();
  return new Intl.DateTimeFormat("zh-CN", sharesCalendarDate
    ? { hour: "2-digit", minute: "2-digit" }
    : { month: "short", day: "numeric" }).format(activityDate);
}

function renderApplication(): void {
  if (workspaceState.isBooting) {
    applicationRoot.innerHTML = renderBootScreen();
  } else if (!workspaceState.account) {
    applicationRoot.innerHTML = renderAuthenticationScreen();
  } else {
    applicationRoot.innerHTML = renderResearchWorkspace();
  }
  createIcons({
    icons: {
      Archive,
      Check,
      CircleAlert,
      CircleCheck,
      KeyRound,
      LoaderCircle,
      LogOut,
      Menu,
      PanelRightOpen,
      ChevronRight,
      Pencil,
      Plus,
      RefreshCw,
      RotateCcw,
      Search,
      Send,
      Settings2,
      ShieldCheck,
      SquarePen,
      X,
    },
  });
}

function renderBootScreen(): string {
  return `<main class="boot-screen" aria-label="正在恢复工作区">
    <span class="brand-mark" aria-hidden="true"></span>
    <i class="spin" data-lucide="loader-circle" aria-hidden="true"></i>
  </main>`;
}

function renderAuthenticationScreen(): string {
  const isRegistering = workspaceState.authenticationMode === "register";
  return `<main class="authentication-screen">
    <section class="authentication-panel" aria-labelledby="authentication-title">
      <div class="authentication-brand">
        <span class="brand-mark" aria-hidden="true"></span>
        <div>
          <p>Traceable Research</p>
          <span>source-grounded workspace</span>
        </div>
      </div>
      <div class="authentication-heading">
        <h1 id="authentication-title">${isRegistering ? "创建研究账户" : "返回研究工作区"}</h1>
      </div>
      <div class="authentication-tabs" role="tablist" aria-label="账户操作">
        <button type="button" role="tab" aria-selected="${!isRegistering}" data-action="switch-to-login">登录</button>
        <button type="button" role="tab" aria-selected="${isRegistering}" data-action="switch-to-register">注册</button>
      </div>
      <form id="authentication-form" class="stacked-form">
        ${isRegistering ? `<label>显示名称<input name="display-name" required maxlength="80" autocomplete="name" /></label>` : ""}
        <label>邮箱<input name="email" type="email" required maxlength="320" autocomplete="email" /></label>
        <label>密码<input name="password" type="password" required minlength="12" maxlength="200" autocomplete="${isRegistering ? "new-password" : "current-password"}" /></label>
        ${renderInlineError()}
        <button class="primary-command" type="submit" ${workspaceState.activeOperation ? "disabled" : ""}>
          ${workspaceState.activeOperation ? '<i class="spin" data-lucide="loader-circle"></i>' : '<i data-lucide="key-round"></i>'}
          ${isRegistering ? "创建账户" : "登录"}
        </button>
      </form>
    </section>
  </main>`;
}

function renderResearchWorkspace(): string {
  return `<main class="workspace-shell ${workspaceState.researchInspectorIsOpen ? "has-inspector" : ""}">
    ${renderConversationSidebar()}
    ${workspaceState.conversationSidebarIsOpen ? '<button class="sidebar-scrim" type="button" data-action="close-sidebar" aria-label="关闭对话列表"></button>' : ""}
    <section class="research-workspace" aria-label="研究工作区">
      ${renderWorkspaceHeader()}
      ${renderConversationTranscript()}
      ${renderResearchComposer()}
    </section>
    ${renderResearchInspector()}
    ${renderModelSettings()}
    ${renderGlobalError()}
  </main>`;
}

function renderConversationSidebar(): string {
  const conversationItems = workspaceState.conversations.length
    ? workspaceState.conversations.map(renderConversationListItem).join("")
    : `<div class="sidebar-empty"><p>尚无研究对话</p></div>`;
  return `<aside class="conversation-sidebar ${workspaceState.conversationSidebarIsOpen ? "is-open" : ""}" aria-label="研究对话">
    <header class="sidebar-header">
      <div class="product-identity">
        <span class="brand-mark" aria-hidden="true"></span>
        <div><strong>Traceable</strong><span>Research</span></div>
      </div>
      <button class="icon-button mobile-only" type="button" data-action="close-sidebar" aria-label="关闭对话列表" title="关闭">
        <i data-lucide="x"></i>
      </button>
    </header>
    <button class="new-conversation-command" type="button" data-action="new-conversation" ${workspaceState.activeOperation ? "disabled" : ""}>
      <i data-lucide="square-pen"></i><span>新研究</span>
    </button>
    <label class="conversation-search">
      <i data-lucide="search" aria-hidden="true"></i>
      <span class="sr-only">搜索研究对话</span>
      <input id="conversation-search" type="search" placeholder="搜索对话" autocomplete="off" />
    </label>
    <nav class="conversation-list" aria-label="对话列表">${conversationItems}</nav>
    <footer class="sidebar-footer">
      <button type="button" class="account-command" data-action="open-model-settings">
        <span class="account-monogram">${escapeHtml(workspaceState.account!.display_name.slice(0, 1).toUpperCase())}</span>
        <span><strong>${escapeHtml(workspaceState.account!.display_name)}</strong><small>${escapeHtml(workspaceState.account!.email)}</small></span>
        <i data-lucide="settings-2" aria-hidden="true"></i>
      </button>
    </footer>
  </aside>`;
}

function renderConversationListItem(conversation: ResearchConversationSummary): string {
  const isActive = workspaceState.activeConversation?.conversation_id === conversation.conversation_id;
  const status = conversation.latest_turn_status;
  const searchableText = `${conversation.title} ${conversation.model_profile_name}`.toLocaleLowerCase();
  return `<button type="button" class="conversation-list-item ${isActive ? "is-active" : ""}"
    data-action="select-conversation" data-conversation-id="${escapeHtml(conversation.conversation_id)}"
    data-search-text="${escapeHtml(searchableText)}" aria-current="${isActive ? "page" : "false"}">
    <span class="conversation-list-title">${escapeHtml(conversation.title)}</span>
    <span class="conversation-list-meta">
      <span>${conversation.turn_count} 轮${status ? ` · ${statusLabels[status]}` : ""}</span>
      <time datetime="${new Date(conversation.updated_at * 1000).toISOString()}">${formatActivityDate(conversation.updated_at)}</time>
    </span>
  </button>`;
}

function renderWorkspaceHeader(): string {
  const conversation = workspaceState.activeConversation;
  const modelOptions = workspaceState.modelProfiles
    .map((profile) => `<option value="${escapeHtml(profile.profile_id)}" ${conversation?.model_profile_id === profile.profile_id ? "selected" : ""}>${escapeHtml(profile.display_name)} · ${escapeHtml(profile.model_id)}</option>`)
    .join("");
  const title = conversation
    ? workspaceState.conversationTitleIsBeingEdited
      ? `<form id="conversation-title-form" class="title-editor">
          <input name="title" value="${escapeHtml(conversation.title)}" maxlength="200" required aria-label="对话标题" />
          <button class="icon-button" type="submit" aria-label="保存标题" title="保存"><i data-lucide="check"></i></button>
          <button class="icon-button" type="button" data-action="cancel-title-edit" aria-label="取消编辑" title="取消"><i data-lucide="x"></i></button>
        </form>`
      : `<div class="conversation-heading"><h1>${escapeHtml(conversation.title)}</h1>
          <button class="icon-button subtle" type="button" data-action="edit-conversation-title" aria-label="重命名对话" title="重命名"><i data-lucide="pencil"></i></button>
        </div>`
    : `<div class="conversation-heading"><h1>研究工作区</h1></div>`;
  return `<header class="workspace-header">
    <button class="icon-button mobile-only" type="button" data-action="open-sidebar" aria-label="打开对话列表" title="对话列表"><i data-lucide="menu"></i></button>
    ${title}
    <div class="workspace-header-actions">
      ${conversation && workspaceState.modelProfiles.length ? `<label class="model-profile-selector"><span class="sr-only">当前模型配置</span><select id="conversation-model-profile" ${workspaceState.activeOperation ? "disabled" : ""}>${modelOptions}</select></label>` : ""}
      ${conversation?.turns.length ? `<button class="icon-button" type="button" data-action="toggle-research-inspector" aria-label="打开研究概览" title="研究概览"><i data-lucide="panel-right-open"></i></button>` : ""}
      <button class="icon-button" type="button" data-action="open-model-settings" aria-label="模型配置" title="模型配置"><i data-lucide="settings-2"></i></button>
      ${conversation ? `<button class="icon-button danger-hover" type="button" data-action="archive-conversation" aria-label="归档对话" title="归档"><i data-lucide="archive"></i></button>` : ""}
    </div>
  </header>`;
}

function selectedInspectorTurn(): ResearchTurn | null {
  const turns = workspaceState.activeConversation?.turns ?? [];
  return turns.find((turn) => turn.turn_id === workspaceState.researchInspectorTurnId)
    ?? turns.at(-1)
    ?? null;
}

function renderResearchInspector(): string {
  if (!workspaceState.researchInspectorIsOpen) return "";
  const conversation = workspaceState.activeConversation;
  const selectedTurn = selectedInspectorTurn();
  if (!conversation || !selectedTurn) return "";
  const turnOptions = conversation.turns.map((turn) => `<option value="${escapeHtml(turn.turn_id)}" ${turn.turn_id === selectedTurn.turn_id ? "selected" : ""}>第 ${turn.turn_number} 轮 · ${statusLabels[turn.status]}</option>`).join("");
  const summary = workspaceState.researchTraceSummaries.get(selectedTurn.turn_id);
  const audit = workspaceState.researchTraceAudits.get(
    selectedTurn.turn_id,
    workspaceState.researchTraceAuditStage,
  );
  const isSummary = workspaceState.researchInspectorView === "summary";
  const body = workspaceState.researchTraceLoading
    ? `<div class="inspector-status"><i class="spin" data-lucide="loader-circle"></i><span>正在读取研究记录</span></div>`
    : workspaceState.researchTraceError
      ? `<div class="inspector-error"><p>${escapeHtml(workspaceState.researchTraceError)}</p><button class="icon-button" type="button" data-action="reload-research-inspector" aria-label="重试" title="重试"><i data-lucide="refresh-cw"></i></button></div>`
      : isSummary
        ? renderResearchTraceSummary(summary)
        : renderResearchTraceAudit(audit);
  return `<aside class="research-inspector" aria-label="研究检查器">
    <header class="research-inspector-header">
      <div><h2>研究过程</h2></div>
      <button class="icon-button" type="button" data-action="close-research-inspector" aria-label="关闭研究过程" title="关闭"><i data-lucide="x"></i></button>
    </header>
    <div class="inspector-turn-selector"><label class="sr-only" for="research-inspector-turn">选择研究轮次</label><select id="research-inspector-turn">${turnOptions}</select></div>
    <div class="inspector-tabs" role="tablist" aria-label="研究记录层级">
      <button type="button" role="tab" aria-selected="${isSummary}" data-action="show-trace-summary">概览</button>
      <button type="button" role="tab" aria-selected="${!isSummary}" data-action="show-trace-audit">审计详情</button>
    </div>
    <div class="research-inspector-content">${body}</div>
  </aside>`;
}

function renderResearchTraceSummary(summary: ResearchTraceSummary | undefined): string {
  if (!summary) return '<div class="inspector-status"><span>当前轮次尚无可展示的研究概览。</span></div>';
  const understanding = summary.understanding
    ? `<section class="inspector-section"><h3>问题理解</h3><p>${escapeHtml(summary.understanding.message)}</p><small>${escapeHtml(summary.understanding.rationale)}</small></section>`
    : "";
  const rounds = summary.rounds.length
    ? `<section class="inspector-section"><h3>检索覆盖</h3>${summary.rounds.map((round) => `<div class="trace-round"><strong>第 ${round.round} 轮</strong><span>${round.search_result_count} 条导航结果</span><ul>${round.directions.map((direction) => `<li>${escapeHtml(direction)}</li>`).join("")}</ul></div>`).join("")}<small>已归档 ${summary.archived_source_count} 个来源${summary.skipped_source_count ? `，跳过 ${summary.skipped_source_count} 个` : ""}</small></section>`
    : "";
  const sources = summary.selected_sources.length
    ? `<section class="inspector-section"><h3>主要来源</h3><ul class="inspector-source-list">${summary.selected_sources.map((source) => {
        const safeUrl = safeEvidenceUrl(source.url);
        const title = escapeHtml(source.title);
        const sourceTitle = safeUrl ? `<a href="${escapeHtml(safeUrl)}" target="_blank" rel="noreferrer">${title}</a>` : `<span>${title}</span>`;
        return `<li>${sourceTitle}<small>${escapeHtml(source.rationale)}</small></li>`;
      }).join("")}</ul></section>`
    : "";
  const synthesis = summary.synthesis_rationale
    ? `<section class="inspector-section"><h3>结论综合</h3><p>${escapeHtml(summary.synthesis_rationale)}</p></section>`
    : "";
  const failure = summary.failure
    ? `<section class="inspector-section inspector-failure"><h3>运行状态</h3><p>${escapeHtml(summary.failure.stage)}：${escapeHtml(summary.failure.message)}</p></section>`
    : "";
  const sections = `${understanding}${rounds}${sources}${synthesis}${failure}`;
  return sections || '<div class="inspector-status"><span>当前轮次尚未产生研究记录。</span></div>';
}

function renderResearchTraceAudit(audit: ResearchTraceAuditPage | undefined): string {
  const entries = audit?.entries ?? [];
  const stage = workspaceState.researchTraceAuditStage;
  const filter = `<label class="audit-filter"><span>阶段</span><select id="research-trace-audit-stage"><option value="" ${stage === "" ? "selected" : ""}>全部</option><option value="dialogue" ${stage === "dialogue" ? "selected" : ""}>理解</option><option value="setup" ${stage === "setup" ? "selected" : ""}>准备</option><option value="planning" ${stage === "planning" ? "selected" : ""}>规划</option><option value="search" ${stage === "search" ? "selected" : ""}>搜索</option><option value="archive" ${stage === "archive" ? "selected" : ""}>归档</option><option value="selection" ${stage === "selection" ? "selected" : ""}>选源</option><option value="synthesis" ${stage === "synthesis" ? "selected" : ""}>结论</option><option value="failure" ${stage === "failure" ? "selected" : ""}>失败</option></select></label>`;
  const rows = entries.length
    ? `<ol class="audit-entry-list">${entries.map((entry) => {
        const metadata = [
          entry.sequence === null ? null : `#${entry.sequence}`,
          entry.occurred_at ? formatTraceTimestamp(entry.occurred_at) : null,
        ].filter((value): value is string => value !== null).join(" · ");
        return `<li>${metadata ? `<time>${escapeHtml(metadata)}</time>` : ""}<span>${escapeHtml(entry.label)}</span><p>${escapeHtml(entry.detail)}</p>${entry.rationale ? `<small>${escapeHtml(entry.rationale)}</small>` : ""}</li>`;
      }).join("")}</ol>`
    : '<div class="inspector-status"><span>这个筛选条件下没有审计事件。</span></div>';
  const more = audit?.next_cursor !== null && audit?.next_cursor !== undefined
    ? '<button class="icon-button audit-more" type="button" data-action="load-more-trace-audit" aria-label="加载更多审计记录" title="加载更多"><i data-lucide="chevron-right"></i></button>'
    : "";
  return `${filter}${rows}${more}`;
}

function formatTraceTimestamp(value: string): string {
  const timestamp = new Date(value);
  if (Number.isNaN(timestamp.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(timestamp);
}

function renderConversationTranscript(): string {
  const conversation = workspaceState.activeConversation;
  if (!conversation) {
    const hasProfiles = workspaceState.modelProfiles.length > 0;
    return `<div class="workspace-empty">
      <span class="empty-ledger-mark" aria-hidden="true"></span>
      <h2>${hasProfiles ? "开始一项研究" : "先连接一个模型"}</h2>
      <button class="secondary-command" type="button" data-action="${hasProfiles ? "new-conversation" : "open-model-settings"}">
        <i data-lucide="${hasProfiles ? "plus" : "settings-2"}"></i>${hasProfiles ? "新研究" : "添加模型配置"}
      </button>
    </div>`;
  }
  if (conversation.turns.length === 0) {
    return `<div class="workspace-empty transcript-empty">
      <span class="empty-ledger-mark" aria-hidden="true"></span>
      <h2>写下需要查证的问题</h2>
    </div>`;
  }
  return `<div class="conversation-transcript" id="conversation-transcript" aria-live="polite">
    <div class="transcript-inner">${conversation.turns.map(renderResearchTurn).join("")}</div>
  </div>`;
}

function renderResearchTurn(turn: ResearchTurn): string {
  const isActiveOperation = workspaceState.activeOperation?.endsWith(turn.turn_id) === true;
  const dialogue = turn.dialogue?.messages.length
    ? turn.dialogue.messages
    : [{ role: "user" as const, text: turn.user_question }];
  const lastAssistantIndex = dialogue.map((message) => message.role).lastIndexOf("assistant");
  const messages = dialogue.map((message, index) => renderDialogueMessage(
    turn,
    message.role,
    message.text,
    index === lastAssistantIndex,
  )).join("");
  const fallback = lastAssistantIndex === -1 ? renderTurnOutcome(turn, isActiveOperation) : "";
  return `<section class="research-turn" data-turn-id="${escapeHtml(turn.turn_id)}">
    ${messages}${fallback}
  </section>`;
}

function renderDialogueMessage(
  turn: ResearchTurn,
  role: "user" | "assistant",
  text: string,
  isLatestAssistant: boolean,
): string {
  if (role === "user") {
    return `<article class="transcript-message user-message"><div class="message-body"><p>${escapeHtml(text)}</p></div></article>`;
  }
  const outcome = isLatestAssistant
    ? renderTurnOutcome(turn, workspaceState.activeOperation?.endsWith(turn.turn_id) === true)
    : "";
  return `<article class="transcript-message assistant-message status-${turn.status}">
    <div class="assistant-message-accent" aria-hidden="true"><span></span></div>
    <div class="assistant-response">
      <div class="message-body"><p>${escapeHtml(text)}</p></div>${outcome}
    </div>
  </article>`;
}

function renderResearchAnswer(turn: ResearchTurn): string {
  const answer = turn.answer!;
  const sources = answer.sources;
  const sourceList = sources.length
    ? `<details class="answer-sources"><summary>来源 ${sources.length}</summary><ul>${sources.map((source) => {
        const safeUrl = safeEvidenceUrl(source.url);
        const title = escapeHtml(source.title);
        return `<li>${safeUrl ? `<a href="${escapeHtml(safeUrl)}" target="_blank" rel="noreferrer">${title}</a>` : `<span>${title}</span>`}</li>`;
      }).join("")}</ul></details>`
    : "";
  return `<div class="research-answer"><p>${escapeHtml(answer.answer)}</p>${sourceList}</div>`;
}

function renderTurnOutcome(turn: ResearchTurn, isActiveOperation: boolean): string {
  if (turn.answer) return renderResearchAnswer(turn);
  if (isActiveOperation || turn.status === "running" || turn.status === "ready") return renderResearchProgress();
  if (turn.status === "failed") return '<div class="turn-failure"><p>研究未完成。你可以继续提出新的研究问题。</p></div>';
  if (turn.status === "cancelled") return '<div class="turn-failure"><p>这轮研究已取消。</p></div>';
  if (turn.dialogue?.status === "failed") {
    return `<div class="turn-failure"><p>${escapeHtml(turn.dialogue.failure ?? "模型暂时无法继续理解该问题。")}</p></div>`;
  }
  return "";
}

function renderResearchProgress(): string {
  return `<div class="research-progress" role="status"><i class="spin" data-lucide="loader-circle"></i><span>正在检索、锁定快照并核验来源</span></div>`;
}

function renderResearchComposer(): string {
  const conversation = workspaceState.activeConversation;
  const lastTurn = conversation?.turns.at(-1);
  const acceptsDialogueMessage = lastTurn?.status === "clarifying"
    && (lastTurn.dialogue?.status === "awaiting_message" || lastTurn.dialogue?.status === "failed");
  const hasBlockingTurn = lastTurn && ["clarifying", "ready", "running"].includes(lastTurn.status) && !acceptsDialogueMessage;
  const isDisabled = !conversation || Boolean(workspaceState.activeOperation) || Boolean(hasBlockingTurn);
  const placeholder = workspaceState.activeOperation
    ? "研究进行中…"
    : acceptsDialogueMessage
      ? "继续补充或纠正我的理解…"
      : hasBlockingTurn
        ? "先完成当前研究轮次"
        : "输入需要查证的研究问题…";
  return `<footer class="composer-region">
    <form id="research-composer" class="research-composer">
      <label class="sr-only" for="research-question">${acceptsDialogueMessage ? "继续对话" : "研究问题"}</label>
      <textarea id="research-question" name="research-question" rows="1" maxlength="4000" placeholder="${placeholder}" ${isDisabled ? "disabled" : ""}></textarea>
      <button type="submit" class="send-command" aria-label="发送" title="发送" ${isDisabled ? "disabled" : ""}><i data-lucide="send"></i></button>
    </form>
  </footer>`;
}

function renderModelSettings(): string {
  if (!workspaceState.modelSettingsAreOpen) return "";
  const editingProfile = workspaceState.modelProfiles.find(
    (profile) => profile.profile_id === workspaceState.editingModelProfileId,
  ) ?? null;
  const profileRows = workspaceState.modelProfiles.length
    ? workspaceState.modelProfiles.map((profile) => `<button type="button" class="model-profile-row ${editingProfile?.profile_id === profile.profile_id ? "is-selected" : ""}" data-action="edit-model-profile" data-profile-id="${escapeHtml(profile.profile_id)}">
        <span><strong>${escapeHtml(profile.display_name)}</strong><code>${escapeHtml(profile.model_id)}</code></span>
        <span class="profile-state">${profile.is_default ? "默认" : ""}${profile.verified_at ? '<i data-lucide="circle-check" aria-label="已验证"></i>' : ""}</span>
      </button>`).join("")
    : `<div class="settings-empty"><p>尚无模型配置</p></div>`;
  return `<div class="modal-scrim" role="presentation">
    <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="model-settings-title">
      <header class="settings-header">
        <div><span>Workspace settings</span><h2 id="model-settings-title">模型配置</h2></div>
        <button class="icon-button" type="button" data-action="close-model-settings" aria-label="关闭模型配置" title="关闭"><i data-lucide="x"></i></button>
      </header>
      <div class="settings-layout">
        <aside class="model-profile-list">
          <div class="settings-list-heading"><span>可用配置</span><button class="icon-button" type="button" data-action="new-model-profile" aria-label="添加模型配置" title="添加"><i data-lucide="plus"></i></button></div>
          ${profileRows}
        </aside>
        ${renderModelProfileEditor(editingProfile)}
      </div>
    </section>
  </div>`;
}

function renderModelProfileEditor(profile: ModelProfile | null): string {
  const isExistingProfile = profile !== null;
  return `<div class="model-profile-editor">
    <div class="profile-editor-heading">
      <div><span>${isExistingProfile ? `Revision ${profile.revision}` : "New profile"}</span><h3>${isExistingProfile ? escapeHtml(profile.display_name) : "添加模型配置"}</h3></div>
      ${isExistingProfile ? `<span class="verification-status ${profile.verified_at ? "is-verified" : ""}"><i data-lucide="${profile.verified_at ? "shield-check" : "circle-alert"}"></i>${profile.verified_at ? "已验证" : "未验证"}</span>` : ""}
    </div>
    <form id="model-profile-form" class="stacked-form profile-form">
      <label>配置名称<input name="display-name" required maxlength="80" value="${escapeHtml(profile?.display_name ?? "")}" placeholder="主要模型" /></label>
      <label>API 地址<input name="api-base-url" type="url" required maxlength="2048" value="${escapeHtml(profile?.api_base_url ?? "")}" placeholder="https://api.example.com/v1/" /></label>
      <label>模型 ID<input name="model-id" required maxlength="200" value="${escapeHtml(profile?.model_id ?? "")}" placeholder="model-name" /></label>
      <label>API Key<input name="api-key" type="password" ${isExistingProfile ? "" : "required"} maxlength="4096" autocomplete="new-password" placeholder="${isExistingProfile ? "留空以保留当前密钥" : "输入 API Key"}" /></label>
      ${!isExistingProfile && workspaceState.modelProfiles.length > 0 ? '<label class="checkbox-field"><input name="make-default" type="checkbox" />设为默认配置</label>' : ""}
      <p class="credential-note"><i data-lucide="key-round"></i>密钥加密保存，保存后不可查看</p>
      ${renderInlineError()}
      <div class="profile-form-actions">
        <button class="primary-command" type="submit" ${workspaceState.activeOperation ? "disabled" : ""}>${workspaceState.activeOperation === "save-model-profile" ? '<i class="spin" data-lucide="loader-circle"></i>' : '<i data-lucide="check"></i>'}${isExistingProfile ? "保存更改" : "保存配置"}</button>
        ${isExistingProfile ? `<button class="secondary-command" type="button" data-action="verify-model-profile" data-profile-id="${escapeHtml(profile.profile_id)}"><i data-lucide="shield-check"></i>验证连接</button>` : ""}
      </div>
    </form>
    ${isExistingProfile ? `<div class="profile-management-actions">
      ${profile.is_default ? "" : `<button class="text-command" type="button" data-action="set-default-model-profile" data-profile-id="${escapeHtml(profile.profile_id)}">设为默认</button>`}
      <button class="text-command danger" type="button" data-action="archive-model-profile" data-profile-id="${escapeHtml(profile.profile_id)}"><i data-lucide="archive"></i>归档配置</button>
    </div>` : ""}
    <button class="logout-command" type="button" data-action="logout"><i data-lucide="log-out"></i>退出 ${escapeHtml(workspaceState.account!.display_name)}</button>
  </div>`;
}

function renderInlineError(): string {
  return workspaceState.errorMessage
    ? `<div class="inline-error" role="alert"><i data-lucide="circle-alert"></i><span>${escapeHtml(workspaceState.errorMessage)}</span></div>`
    : "";
}

function renderGlobalError(): string {
  if (!workspaceState.errorMessage || workspaceState.modelSettingsAreOpen) return "";
  return `<div class="error-toast" role="alert"><i data-lucide="circle-alert"></i><span>${escapeHtml(workspaceState.errorMessage)}</span><button class="icon-button" type="button" data-action="dismiss-error" aria-label="关闭错误"><i data-lucide="x"></i></button></div>`;
}

function handleWorkspaceError(error: unknown): void {
  if (error instanceof ResearchWorkspaceRequestError && error.status === 401) {
    workspaceState.account = null;
    workspaceState.activeConversation = null;
    workspaceState.conversations = [];
    workspaceState.modelProfiles = [];
    clearLoadedResearchTraces();
    workspaceState.modelSettingsAreOpen = false;
    workspaceState.errorMessage = "登录已失效，请重新登录";
    return;
  }
  workspaceState.errorMessage = error instanceof Error ? error.message : "请求失败";
}

async function restoreAuthenticatedWorkspace(): Promise<void> {
  workspaceState.isBooting = true;
  renderApplication();
  try {
    workspaceState.account = await researchWorkspaceClient.currentAccount();
    await reloadWorkspaceData(true);
  } catch (error) {
    if (!(error instanceof ResearchWorkspaceRequestError && error.status === 401)) {
      handleWorkspaceError(error);
    }
    workspaceState.account = null;
  } finally {
    workspaceState.isBooting = false;
    renderApplication();
  }
}

async function reloadWorkspaceData(loadMostRecentConversation: boolean): Promise<void> {
  const [modelProfiles, conversations] = await Promise.all([
    researchWorkspaceClient.listModelProfiles(),
    researchWorkspaceClient.listResearchConversations(),
  ]);
  workspaceState.modelProfiles = modelProfiles;
  workspaceState.conversations = conversations;
  if (modelProfiles.length === 0) {
    workspaceState.modelSettingsAreOpen = true;
    workspaceState.editingModelProfileId = null;
  }
  const preferredConversationId = loadMostRecentConversation
    ? conversations[0]?.conversation_id
    : workspaceState.activeConversation?.conversation_id;
  workspaceState.activeConversation = preferredConversationId
    ? await researchWorkspaceClient.loadResearchConversation(preferredConversationId)
    : null;
}

async function refreshConversationSummaries(): Promise<void> {
  workspaceState.conversations = await researchWorkspaceClient.listResearchConversations();
  synchronizeActiveConversationSummary();
}

function synchronizeActiveConversationSummary(): void {
  const activeConversation = workspaceState.activeConversation;
  if (!activeConversation) return;
  const summary = workspaceState.conversations.find(
    (conversation) => conversation.conversation_id === activeConversation.conversation_id,
  );
  if (!summary) return;
  Object.assign(activeConversation, summary);
}

async function authenticateFromForm(form: HTMLFormElement): Promise<void> {
  const fields = new FormData(form);
  const email = String(fields.get("email") ?? "");
  const password = String(fields.get("password") ?? "");
  const displayName = String(fields.get("display-name") ?? "");
  workspaceState.activeOperation = "authenticate";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    workspaceState.account = workspaceState.authenticationMode === "register"
      ? await researchWorkspaceClient.registerAccount(email, password, displayName)
      : await researchWorkspaceClient.login(email, password);
    clearLoadedResearchTraces();
    await reloadWorkspaceData(true);
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function createResearchConversation(): Promise<void> {
  if (workspaceState.modelProfiles.length === 0) {
    workspaceState.modelSettingsAreOpen = true;
    workspaceState.editingModelProfileId = null;
    renderApplication();
    return;
  }
  workspaceState.activeOperation = "create-conversation";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    const defaultProfile = workspaceState.modelProfiles.find((profile) => profile.is_default)
      ?? workspaceState.modelProfiles[0];
    workspaceState.activeConversation = await researchWorkspaceClient.createResearchConversation(
      defaultProfile?.profile_id,
    );
    workspaceState.conversationSidebarIsOpen = false;
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
    focusResearchComposer();
  }
}

async function selectResearchConversation(conversationId: string): Promise<void> {
  if (workspaceState.activeConversation?.conversation_id === conversationId) {
    workspaceState.conversationSidebarIsOpen = false;
    renderApplication();
    return;
  }
  workspaceState.activeOperation = "load-conversation";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    workspaceState.activeConversation = await researchWorkspaceClient.loadResearchConversation(conversationId);
    workspaceState.conversationSidebarIsOpen = false;
    workspaceState.researchInspectorTurnId = null;
    workspaceState.researchInspectorIsOpen = false;
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function refreshActiveResearchConversation(): Promise<void> {
  const conversationId = workspaceState.activeConversation?.conversation_id;
  if (!conversationId || workspaceState.activeOperation) return;
  workspaceState.activeOperation = "refresh-conversation";
  renderApplication();
  try {
    workspaceState.activeConversation = await researchWorkspaceClient.loadResearchConversation(conversationId);
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function openResearchInspector(turnId?: string): Promise<void> {
  const conversation = workspaceState.activeConversation;
  const selectedTurnId = turnId ?? conversation?.turns.at(-1)?.turn_id;
  if (!conversation || !selectedTurnId) return;
  workspaceState.researchInspectorIsOpen = true;
  workspaceState.researchInspectorTurnId = selectedTurnId;
  workspaceState.researchInspectorView = "summary";
  workspaceState.researchTraceError = null;
  await loadResearchInspectorData();
}

async function loadResearchInspectorData(options: { force?: boolean; append?: boolean } = {}): Promise<void> {
  const conversation = workspaceState.activeConversation;
  const turn = selectedInspectorTurn();
  if (!conversation || !turn) return;
  const turnId = turn.turn_id;
  const isAudit = workspaceState.researchInspectorView === "audit";
  if (!options.force && !isAudit && workspaceState.researchTraceSummaries.has(turnId)) {
    renderWorkspaceWithoutLosingResearchDraft();
    return;
  }
  if (
    !options.force
    && isAudit
    && workspaceState.researchTraceAudits.has(turnId, workspaceState.researchTraceAuditStage)
    && !options.append
  ) {
    renderWorkspaceWithoutLosingResearchDraft();
    return;
  }
  workspaceState.researchTraceLoading = true;
  workspaceState.researchTraceError = null;
  renderWorkspaceWithoutLosingResearchDraft();
  const accountId = workspaceState.account?.user_id;
  try {
    if (isAudit) {
      const existing = workspaceState.researchTraceAudits.get(
        turnId,
        workspaceState.researchTraceAuditStage,
      );
      const page = await researchWorkspaceClient.loadResearchTraceAudit(conversation.conversation_id, turnId, {
        stage: workspaceState.researchTraceAuditStage || undefined,
        cursor: options.append ? existing?.next_cursor ?? undefined : undefined,
      });
      if (workspaceState.account?.user_id !== accountId) return;
      workspaceState.researchTraceAudits.set(
        turnId,
        workspaceState.researchTraceAuditStage,
        options.append && existing
          ? { ...page, entries: [...existing.entries, ...page.entries] }
          : page,
      );
    } else {
      const summary = await researchWorkspaceClient.loadResearchTraceSummary(conversation.conversation_id, turnId);
      if (workspaceState.account?.user_id !== accountId) return;
      workspaceState.researchTraceSummaries.set(turnId, summary);
    }
  } catch (error) {
    if (workspaceState.account?.user_id !== accountId) return;
    if (error instanceof ResearchWorkspaceRequestError && error.status === 401) {
      handleWorkspaceError(error);
    } else {
      workspaceState.researchTraceError = error instanceof Error ? error.message : "无法读取研究记录";
    }
  } finally {
    workspaceState.researchTraceLoading = false;
    renderWorkspaceWithoutLosingResearchDraft();
  }
}

async function updateConversationTitle(form: HTMLFormElement): Promise<void> {
  const conversation = workspaceState.activeConversation;
  if (!conversation) return;
  const title = String(new FormData(form).get("title") ?? "").trim();
  if (!title) return;
  workspaceState.activeOperation = "rename-conversation";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.updateResearchConversation(conversation.conversation_id, { title });
    conversation.title = title;
    workspaceState.conversationTitleIsBeingEdited = false;
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function updateConversationModelProfile(modelProfileId: string): Promise<void> {
  const conversation = workspaceState.activeConversation;
  if (!conversation || modelProfileId === conversation.model_profile_id) return;
  workspaceState.activeOperation = "change-conversation-model";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.updateResearchConversation(conversation.conversation_id, {
      model_profile_id: modelProfileId,
    });
    conversation.model_profile_id = modelProfileId;
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function archiveActiveResearchConversation(): Promise<void> {
  const conversation = workspaceState.activeConversation;
  if (!conversation || !window.confirm(`归档“${conversation.title}”？`)) return;
  workspaceState.activeOperation = "archive-conversation";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.archiveResearchConversation(conversation.conversation_id);
    workspaceState.activeConversation = null;
    await reloadWorkspaceData(true);
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

function replaceResearchTurn(updatedTurn: ResearchTurn): void {
  const conversation = workspaceState.activeConversation;
  if (!conversation) return;
  const existingIndex = conversation.turns.findIndex((turn) => turn.turn_id === updatedTurn.turn_id);
  if (existingIndex === -1) conversation.turns.push(updatedTurn);
  else conversation.turns[existingIndex] = updatedTurn;
}

async function submitResearchComposer(form: HTMLFormElement): Promise<void> {
  const conversation = workspaceState.activeConversation;
  if (!conversation || workspaceState.activeOperation) return;
  const formData = new FormData(form);
  const submittedText = String(formData.get("research-question") ?? "").trim();
  if (!submittedText) return;
  const lastTurn = conversation.turns.at(-1);
  const isDialogueMessage = lastTurn?.status === "clarifying"
    && (lastTurn.dialogue?.status === "awaiting_message" || lastTurn.dialogue?.status === "failed");
  workspaceState.activeOperation = `${isDialogueMessage ? "dialogue" : "start"}:${lastTurn?.turn_id ?? "new"}`;
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    let updatedTurn: ResearchTurn;
    if (isDialogueMessage && lastTurn?.dialogue) {
      updatedTurn = await researchWorkspaceClient.submitDialogueMessage(
        conversation.conversation_id,
        lastTurn.turn_id,
        lastTurn.dialogue.revision,
        submittedText,
      );
    } else {
      updatedTurn = await researchWorkspaceClient.startResearchTurn(
        conversation.conversation_id,
        submittedText,
        "web_first",
      );
    }
    replaceResearchTurn(updatedTurn);
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
    focusResearchComposer();
  }
}

async function saveModelProfile(form: HTMLFormElement): Promise<void> {
  const formData = new FormData(form);
  const apiKey = String(formData.get("api-key") ?? "");
  const profileInput = {
    display_name: String(formData.get("display-name") ?? ""),
    api_base_url: String(formData.get("api-base-url") ?? ""),
    model_id: String(formData.get("model-id") ?? ""),
    ...(apiKey ? { api_key: apiKey } : {}),
    make_default: formData.get("make-default") === "on",
  };
  const editingProfileId = workspaceState.editingModelProfileId;
  workspaceState.activeOperation = "save-model-profile";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    const savedProfile = editingProfileId
      ? await researchWorkspaceClient.updateModelProfile(editingProfileId, profileInput)
      : await researchWorkspaceClient.createModelProfile(profileInput);
    workspaceState.modelProfiles = await researchWorkspaceClient.listModelProfiles();
    workspaceState.editingModelProfileId = savedProfile.profile_id;
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function verifyModelProfile(profileId: string): Promise<void> {
  workspaceState.activeOperation = "verify-model-profile";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.verifyModelProfile(profileId);
    workspaceState.modelProfiles = await researchWorkspaceClient.listModelProfiles();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function setDefaultModelProfile(profileId: string): Promise<void> {
  workspaceState.activeOperation = "set-default-model-profile";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.setDefaultModelProfile(profileId);
    workspaceState.modelProfiles = await researchWorkspaceClient.listModelProfiles();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function archiveModelProfile(profileId: string): Promise<void> {
  const profile = workspaceState.modelProfiles.find((candidate) => candidate.profile_id === profileId);
  if (!profile || !window.confirm(`归档模型配置“${profile.display_name}”？`)) return;
  workspaceState.activeOperation = "archive-model-profile";
  workspaceState.errorMessage = null;
  renderApplication();
  try {
    await researchWorkspaceClient.archiveModelProfile(profileId);
    workspaceState.modelProfiles = await researchWorkspaceClient.listModelProfiles();
    workspaceState.editingModelProfileId = workspaceState.modelProfiles[0]?.profile_id ?? null;
    await refreshConversationSummaries();
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

async function logout(): Promise<void> {
  workspaceState.activeOperation = "logout";
  renderApplication();
  try {
    await researchWorkspaceClient.logout();
    workspaceState.account = null;
    workspaceState.modelProfiles = [];
    workspaceState.conversations = [];
    workspaceState.activeConversation = null;
    clearLoadedResearchTraces();
    workspaceState.modelSettingsAreOpen = false;
    workspaceState.errorMessage = null;
  } catch (error) {
    handleWorkspaceError(error);
  } finally {
    workspaceState.activeOperation = null;
    renderApplication();
  }
}

function focusResearchComposer(): void {
  window.requestAnimationFrame(() => {
    applicationRoot.querySelector<HTMLTextAreaElement>("#research-question:not(:disabled)")?.focus();
  });
}

applicationRoot.addEventListener("click", (event) => {
  const actionElement = (event.target as Element).closest<HTMLElement>("[data-action]");
  if (!actionElement) return;
  const action = actionElement.dataset.action;
  const conversationId = actionElement.dataset.conversationId;
  const profileId = actionElement.dataset.profileId;
  const turnId = actionElement.dataset.turnId;
  switch (action) {
    case "switch-to-login":
      workspaceState.authenticationMode = "login";
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "switch-to-register":
      workspaceState.authenticationMode = "register";
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "new-conversation":
      void createResearchConversation();
      break;
    case "select-conversation":
      if (conversationId) void selectResearchConversation(conversationId);
      break;
    case "open-sidebar":
      workspaceState.conversationSidebarIsOpen = true;
      renderApplication();
      break;
    case "close-sidebar":
      workspaceState.conversationSidebarIsOpen = false;
      renderApplication();
      break;
    case "open-model-settings":
      workspaceState.modelSettingsAreOpen = true;
      workspaceState.editingModelProfileId ??= workspaceState.modelProfiles[0]?.profile_id ?? null;
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "close-model-settings":
      workspaceState.modelSettingsAreOpen = false;
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "new-model-profile":
      workspaceState.editingModelProfileId = null;
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "edit-model-profile":
      workspaceState.editingModelProfileId = profileId ?? null;
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "verify-model-profile":
      if (profileId) void verifyModelProfile(profileId);
      break;
    case "set-default-model-profile":
      if (profileId) void setDefaultModelProfile(profileId);
      break;
    case "archive-model-profile":
      if (profileId) void archiveModelProfile(profileId);
      break;
    case "edit-conversation-title":
      workspaceState.conversationTitleIsBeingEdited = true;
      renderApplication();
      applicationRoot.querySelector<HTMLInputElement>("#conversation-title-form input")?.select();
      break;
    case "cancel-title-edit":
      workspaceState.conversationTitleIsBeingEdited = false;
      renderApplication();
      break;
    case "archive-conversation":
      void archiveActiveResearchConversation();
      break;
    case "toggle-research-inspector":
      void openResearchInspector();
      break;
    case "close-research-inspector":
      workspaceState.researchInspectorIsOpen = false;
      renderApplication();
      break;
    case "show-trace-summary":
      workspaceState.researchInspectorView = "summary";
      void loadResearchInspectorData();
      break;
    case "show-trace-audit":
      workspaceState.researchInspectorView = "audit";
      void loadResearchInspectorData();
      break;
    case "reload-research-inspector":
      void loadResearchInspectorData({ force: true });
      break;
    case "load-more-trace-audit":
      void loadResearchInspectorData({ append: true });
      break;
    case "dismiss-error":
      workspaceState.errorMessage = null;
      renderApplication();
      break;
    case "logout":
      void logout();
      break;
  }
});

applicationRoot.addEventListener("submit", (event) => {
  event.preventDefault();
  const form = event.target as HTMLFormElement;
  if (form.id === "authentication-form") void authenticateFromForm(form);
  if (form.id === "conversation-title-form") void updateConversationTitle(form);
  if (form.id === "model-profile-form") void saveModelProfile(form);
  if (form.id === "research-composer") void submitResearchComposer(form);
});

applicationRoot.addEventListener("change", (event) => {
  const target = event.target as HTMLSelectElement;
  if (target.id === "conversation-model-profile") void updateConversationModelProfile(target.value);
  if (target.id === "research-inspector-turn") {
    workspaceState.researchInspectorTurnId = target.value;
    workspaceState.researchTraceError = null;
    void loadResearchInspectorData();
  }
  if (target.id === "research-trace-audit-stage") {
    workspaceState.researchTraceAuditStage = target.value;
    const turnId = workspaceState.researchInspectorTurnId;
    if (turnId) {
      workspaceState.researchTraceAudits.delete(turnId, workspaceState.researchTraceAuditStage);
    }
    void loadResearchInspectorData({ force: true });
  }
});

applicationRoot.addEventListener("input", (event) => {
  const target = event.target as HTMLInputElement | HTMLTextAreaElement;
  if (target.id === "conversation-search") {
    const searchQuery = target.value.trim().toLocaleLowerCase();
    applicationRoot.querySelectorAll<HTMLElement>(".conversation-list-item").forEach((item) => {
      item.hidden = !item.dataset.searchText?.includes(searchQuery);
    });
  }
  if (target.id === "research-question") {
    target.style.height = "auto";
    target.style.height = `${Math.min(target.scrollHeight, 144)}px`;
  }
});

applicationRoot.addEventListener("keydown", (event) => {
  const target = event.target as HTMLTextAreaElement;
  if (target.id === "research-question" && event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    target.form?.requestSubmit();
  }
  if (event.key === "Escape" && workspaceState.modelSettingsAreOpen) {
    workspaceState.modelSettingsAreOpen = false;
    workspaceState.errorMessage = null;
    renderApplication();
  }
});

window.setInterval(() => {
  const lastTurn = workspaceState.activeConversation?.turns.at(-1);
  if (lastTurn && ["ready", "running"].includes(lastTurn.status) && !workspaceState.activeOperation) {
    void refreshActiveResearchConversation();
  }
}, 5_000);

void restoreAuthenticatedWorkspace();
