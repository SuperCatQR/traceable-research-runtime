import {
  Archive,
  ArrowUpRight,
  BookOpenText,
  Check,
  ChevronDown,
  CircleAlert,
  Clock3,
  FileSearch,
  Menu,
  Moon,
  MessageSquareText,
  MoreHorizontal,
  PanelRightOpen,
  Plus,
  RefreshCw,
  Search,
  Send,
  Settings2,
  ShieldCheck,
  Sparkles,
  Sun,
  X,
  createIcons,
} from "lucide";
import "./demo.css";

type DemoState = "complete" | "running" | "empty" | "error";
type InspectorTab = "overview" | "audit";
type Theme = "light" | "dark";

interface ConversationItem {
  id: string;
  title: string;
  meta: string;
  state: DemoState;
}

interface DemoStore {
  state: DemoState;
  activeConversationId: string;
  inspectorOpen: boolean;
  inspectorTab: InspectorTab;
  sidebarOpen: boolean;
  selectedSource: number | null;
  submittedQuestion: string;
  theme: Theme;
}

function shouldOpenInspector(): boolean {
  return window.innerWidth > 880;
}

function initialTheme(): Theme {
  try {
    const savedTheme = window.localStorage.getItem("traceable-demo-theme");
    if (savedTheme === "light" || savedTheme === "dark") return savedTheme;
  } catch {
    // Theme persistence is optional in the standalone demo.
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

const queriedRoot = document.querySelector<HTMLElement>("#app");
if (!queriedRoot) throw new Error("Missing #app");
const root: HTMLElement = queriedRoot;

const conversations: ConversationItem[] = [
  {
    id: "four-day-week",
    title: "四天工作制的组织影响",
    meta: "3 轮 · 刚刚",
    state: "complete",
  },
  {
    id: "ai-search",
    title: "AI 搜索的引用透明度",
    meta: "研究中 · 8 分钟前",
    state: "running",
  },
  {
    id: "remote-work",
    title: "远程办公与创新效率",
    meta: "需要处理 · 昨天",
    state: "error",
  },
  {
    id: "consumer-trust",
    title: "消费者如何判断信息可信度",
    meta: "2 轮 · 7 月 14 日",
    state: "complete",
  },
];

const store: DemoStore = {
  state: "complete",
  activeConversationId: "four-day-week",
  inspectorOpen: shouldOpenInspector(),
  inspectorTab: "overview",
  sidebarOpen: false,
  selectedSource: null,
  submittedQuestion: "四天工作制对知识型团队的绩效和员工健康有什么影响？请区分短期试点与长期证据。",
  theme: initialTheme(),
};

const stateLabels: Record<DemoState, string> = {
  complete: "完成态",
  running: "研究中",
  empty: "空状态",
  error: "错误态",
};

function render(): void {
  document.documentElement.dataset.theme = store.theme;
  root.innerHTML = `
    <main class="demo-shell ${store.inspectorOpen ? "has-inspector" : ""}">
      ${renderSidebar()}
      ${store.sidebarOpen ? '<button class="mobile-scrim" data-action="close-sidebar" aria-label="关闭研究列表"></button>' : ""}
      <section class="workspace" aria-label="Traceable Research 演示工作区">
        ${renderHeader()}
        ${renderMainContent()}
        ${renderComposer()}
      </section>
      ${renderInspector()}
      <div class="demo-badge"><span></span>本地演示数据</div>
    </main>`;

  createIcons({
    icons: {
      Archive,
      ArrowUpRight,
      BookOpenText,
      Check,
      ChevronDown,
      CircleAlert,
      Clock3,
      FileSearch,
      Menu,
      Moon,
      MessageSquareText,
      MoreHorizontal,
      PanelRightOpen,
      Plus,
      RefreshCw,
      Search,
      Send,
      Settings2,
      ShieldCheck,
      Sparkles,
      Sun,
      X,
    },
  });
  syncDemoSwitcher();
}

function renderSidebar(): string {
  return `
    <aside class="sidebar ${store.sidebarOpen ? "is-open" : ""}">
      <header class="brand-row">
        <div class="brand-lockup">
          <span class="brand-symbol" aria-hidden="true"><i></i><i></i><i></i></span>
          <span><strong>Traceable</strong><small>RESEARCH</small></span>
        </div>
        <button class="icon-button mobile-only" data-action="close-sidebar" aria-label="关闭研究列表"><i data-lucide="x"></i></button>
      </header>

      <button class="new-research" data-action="new-research">
        <i data-lucide="plus"></i><span>新研究</span><kbd>⌘ N</kbd>
      </button>

      <label class="sidebar-search">
        <i data-lucide="search"></i>
        <input type="search" id="conversation-filter" placeholder="搜索研究" autocomplete="off" />
      </label>

      <div class="sidebar-section-label"><span>最近研究</span><span>${conversations.length}</span></div>
      <nav class="conversation-list" aria-label="研究对话">
        ${conversations.map(renderConversationItem).join("")}
      </nav>

      <div class="sidebar-spacer"></div>
      <button class="archive-link"><i data-lucide="archive"></i><span>已归档研究</span></button>
      <footer class="account-card">
        <span class="avatar">陈</span>
        <span><strong>Chosen Echo</strong><small>个人研究空间</small></span>
        <i data-lucide="settings-2"></i>
      </footer>
    </aside>`;
}

function renderConversationItem(item: ConversationItem): string {
  const active = item.id === store.activeConversationId && store.state !== "empty";
  const stateClass = item.state === "running" ? "is-running" : item.state === "error" ? "has-error" : "";
  return `
    <button class="conversation-item ${active ? "is-active" : ""} ${stateClass}"
      data-action="select-conversation" data-id="${item.id}"
      data-filter-text="${item.title.toLocaleLowerCase()}">
      <span class="conversation-title">${item.title}</span>
      <span class="conversation-meta"><i></i>${item.meta}</span>
    </button>`;
}

function renderHeader(): string {
  const title = store.state === "empty" ? "新的研究" : conversations.find((item) => item.id === store.activeConversationId)?.title ?? "研究工作区";
  return `
    <header class="workspace-header">
      <button class="icon-button mobile-only" data-action="open-sidebar" aria-label="打开研究列表"><i data-lucide="menu"></i></button>
      <div class="document-title">
        <span class="title-kicker">研究案卷</span>
        <h1>${title}</h1>
      </div>
      <div class="header-actions">
        ${store.state !== "empty" ? renderStateStatus() : ""}
        <button class="model-button"><span>GPT-5 Research</span><i data-lucide="chevron-down"></i></button>
        <button class="icon-button theme-toggle" data-action="toggle-theme" aria-label="切换到${store.theme === "light" ? "深色" : "浅色"}主题" title="切换主题"><i data-lucide="${store.theme === "light" ? "moon" : "sun"}"></i></button>
        <button class="icon-button inspector-trigger ${store.inspectorOpen ? "is-active" : ""}" data-action="toggle-inspector" aria-label="打开研究概览" title="研究概览"><i data-lucide="panel-right-open"></i></button>
        <button class="icon-button desktop-only" aria-label="更多操作"><i data-lucide="more-horizontal"></i></button>
      </div>
    </header>`;
}

function renderStateStatus(): string {
  const copy: Record<DemoState, string> = {
    complete: "证据已核对",
    running: "正在研究",
    empty: "",
    error: "研究中断",
  };
  return `<span class="state-status status-${store.state}"><i></i>${copy[store.state]}</span>`;
}

function renderMainContent(): string {
  if (store.state === "empty") return renderEmptyState();
  return `
    <div class="document-scroll" id="document-scroll">
      <article class="research-document ${store.state === "running" ? "is-running" : ""}">
        ${renderQuestionBlock()}
        ${store.state === "complete" ? renderCompletedAnswer() : ""}
        ${store.state === "running" ? renderRunningState() : ""}
        ${store.state === "error" ? renderErrorState() : ""}
      </article>
    </div>`;
}

function renderQuestionBlock(): string {
  return `
    <section class="question-block">
      <div class="question-index">Q</div>
      <div>
        <p class="section-eyebrow">你的问题</p>
        <h2>${store.submittedQuestion}</h2>
        <div class="question-meta"><span>今天 14:32</span><span>·</span><span>要求区分证据期限</span></div>
      </div>
    </section>`;
}

function renderCompletedAnswer(): string {
  return `
    <section class="answer-block">
      <header class="answer-heading">
        <div>
          <p class="section-eyebrow">研究结论</p>
          <h2>短期收益较一致，长期效果仍取决于工作重构</h2>
        </div>
        <span class="answer-date">3 个主要来源</span>
      </header>

      <div class="finding-callout">
        <span class="finding-mark"><i data-lucide="sparkles"></i></span>
        <p>现有证据更支持“减少低价值工作后的四天制”，而不是把五天任务压缩进四天。试点通常改善倦怠、留任与主观生产力，但超过一年的对照证据仍然有限。</p>
      </div>

      <div class="answer-prose">
        <h3>可以较有把握地说什么</h3>
        <p>多国组织试点普遍观察到员工倦怠下降、工作满意度上升，同时多数参与企业的收入或服务指标没有明显恶化。较大规模的研究也发现，改善并不只来自“少上一天班”，而是来自会议缩减、异步协作和目标重新排序。<button class="citation" data-action="open-source" data-source="1" aria-label="查看来源 1">1</button><button class="citation" data-action="open-source" data-source="2" aria-label="查看来源 2">2</button></p>

        <h3>需要谨慎解释的地方</h3>
        <p>许多数据来自主动报名的企业，存在选择偏差；试点期间的关注效应也可能抬高短期结果。制造、医疗、客服等需要连续覆盖的岗位，实施成本通常高于知识型团队，因此不能直接外推。<button class="citation" data-action="open-source" data-source="3" aria-label="查看来源 3">3</button></p>

        <div class="comparison-grid">
          <div><span>短期（3–6 个月）</span><strong>健康与留任改善证据较强</strong><p>适合用试点验证本组织的会议、交接与服务覆盖设计。</p></div>
          <div><span>长期（12 个月以上）</span><strong>证据数量仍然不足</strong><p>重点观察绩效回落、隐性加班和新员工培养是否受到影响。</p></div>
        </div>

        <h3>给决策者的建议</h3>
        <ol class="recommendation-list">
          <li><span>01</span><p>先选边界清楚的团队做 12–16 周试点，不要求所有岗位使用同一排班。</p></li>
          <li><span>02</span><p>试点前同时记录业务指标、员工健康和客户响应，避免只看满意度。</p></li>
          <li><span>03</span><p>把“停止哪些工作”写进方案；如果只是压缩工时，收益很可能被工作强度抵消。</p></li>
        </ol>
      </div>

      <footer class="answer-footer">
        <button data-action="toggle-inspector"><i data-lucide="file-search"></i>查看研究概览</button>
        <span>回答由示例数据生成，仅用于界面评审</span>
      </footer>
    </section>`;
}

function renderRunningState(): string {
  return `
    <section class="live-research-card" aria-live="polite">
      <div class="live-orbit" aria-hidden="true"><span></span><i></i></div>
      <div>
        <p class="section-eyebrow">研究正在进行</p>
        <h2>正在查找并核对公开来源</h2>
        <p>完成后答案会出现在这里。你可以离开这个页面，研究会继续运行。</p>
      </div>
      <span class="elapsed"><i data-lucide="clock-3"></i>约 2 分钟前开始</span>
    </section>
    <section class="reading-placeholder" aria-hidden="true">
      <span></span><span></span><span></span><span></span>
    </section>`;
}

function renderErrorState(): string {
  return `
    <section class="error-state-card">
      <span class="error-icon"><i data-lucide="circle-alert"></i></span>
      <div>
        <p class="section-eyebrow">研究未完成</p>
        <h2>模型连接在检索阶段中断</h2>
        <p>已保留你的问题和当前研究记录。检查模型配置后，可以从本轮重新开始。</p>
        <div class="error-actions">
          <button class="primary-small" data-action="retry-research"><i data-lucide="refresh-cw"></i>重新研究</button>
          <button class="secondary-small"><i data-lucide="settings-2"></i>检查模型配置</button>
        </div>
      </div>
    </section>`;
}

function renderEmptyState(): string {
  return `
    <div class="empty-state">
      <div class="empty-art" aria-hidden="true">
        <span class="paper-sheet"></span><span class="evidence-tab">SOURCE</span><i></i><i></i><i></i>
      </div>
      <p class="section-eyebrow">新的研究</p>
      <h2>写下一个值得查证的问题</h2>
      <p>描述你想知道什么、用于什么决策；系统会在需要时追问，并在信息足够后自动开始研究。</p>
      <div class="prompt-examples">
        <button data-action="use-prompt">比较三个主流密码管理器的安全模型和家庭共享能力</button>
        <button data-action="use-prompt">整理远程办公对创新效率的长期研究，区分行业差异</button>
        <button data-action="use-prompt">购买家用储能前，需要核查哪些成本和安全风险？</button>
      </div>
    </div>`;
}

function renderComposer(): string {
  const disabled = store.state === "running";
  return `
    <footer class="composer-wrap">
      <form class="composer" id="demo-composer">
        <textarea id="research-input" rows="1" maxlength="1200" placeholder="${disabled ? "本轮研究完成后可以继续追问" : "继续追问，或开始一个相关研究…"}" ${disabled ? "disabled" : ""}></textarea>
        <div class="composer-bottom">
          <span><kbd>Enter</kbd> 发送 · <kbd>Shift Enter</kbd> 换行</span>
          <button type="submit" aria-label="发送问题" ${disabled ? "disabled" : ""}><i data-lucide="send"></i></button>
        </div>
      </form>
    </footer>`;
}

function renderInspector(): string {
  if (!store.inspectorOpen) return "";
  return `
    <aside class="inspector" aria-label="研究过程">
      <header class="inspector-header">
        <div><span>TRACE / 003</span><h2>研究过程</h2></div>
        <button class="icon-button" data-action="close-inspector" aria-label="关闭研究过程"><i data-lucide="x"></i></button>
      </header>
      <div class="inspector-tabs" role="tablist">
        <button role="tab" aria-selected="${store.inspectorTab === "overview"}" data-action="show-overview">研究概览</button>
        <button role="tab" aria-selected="${store.inspectorTab === "audit"}" data-action="show-audit">审计详情</button>
      </div>
      ${store.state === "complete" ? (store.inspectorTab === "overview" ? renderOverview() : renderAudit()) : renderInspectorState()}
    </aside>`;
}

function renderOverview(): string {
  const sources = [
    ["1", "Nature Human Behaviour", "跨国组织试点的员工健康与绩效变化", "2025 · 研究论文"],
    ["2", "University of Cambridge", "英国四天工作制试点跟踪", "2023 · 研究报告"],
    ["3", "International Labour Organization", "工作时间、生产力与岗位差异", "2022 · 综合报告"],
  ];
  return `
    <div class="inspector-scroll overview-panel">
      <section class="trace-section trace-understanding">
        <div class="trace-label"><span>01</span>问题理解</div>
        <p>评估四天工作制对知识型团队绩效与健康的影响，并明确短期试点不能代替长期因果证据。</p>
        <div class="scope-tags"><span>知识型团队</span><span>短期 / 长期</span><span>绩效 + 健康</span></div>
      </section>
      <section class="trace-section">
        <div class="trace-label"><span>02</span>检索覆盖</div>
        <div class="coverage-grid">
          <div><strong>18</strong><span>查看结果</span></div>
          <div><strong>7</strong><span>归档来源</span></div>
          <div><strong>3</strong><span>主要证据</span></div>
        </div>
        <ul class="direction-list"><li>跨国组织试点与追踪研究</li><li>工作时间与生产力的系统综述</li><li>岗位覆盖和实施限制</li></ul>
      </section>
      <section class="trace-section source-section">
        <div class="trace-label"><span>03</span>主要来源</div>
        <div class="source-rail">
          ${sources.map(([number, title, description, meta]) => `
            <button class="source-card ${store.selectedSource === Number(number) ? "is-selected" : ""}" data-action="select-source" data-source="${number}">
              <span class="source-number">${number}</span>
              <span class="source-copy"><strong>${title}</strong><span>${description}</span><small>${meta}</small></span>
              <i data-lucide="arrow-up-right"></i>
            </button>`).join("")}
        </div>
      </section>
      <section class="trace-section trace-synthesis">
        <div class="trace-label"><span>04</span>综合说明</div>
        <p>结论优先采用跨组织研究与综合报告；企业个案只用于补充实施细节，没有作为长期效果的主要依据。</p>
      </section>
    </div>`;
}

function renderAudit(): string {
  const entries = [
    ["14:32:08", "理解问题", "识别出需要同时比较绩效、员工健康与证据期限。"],
    ["14:32:15", "规划检索", "建立三条检索方向，并优先查找同行评审研究和公共机构报告。"],
    ["14:33:02", "选择来源", "保留 7 个来源，排除 4 篇只有企业宣传信息的材料。"],
    ["14:34:26", "综合结论", "将短期试点证据与长期不确定性分开表述。"],
  ];
  return `
    <div class="inspector-scroll audit-panel">
      <div class="audit-note"><i data-lucide="shield-check"></i><p><strong>安全审计投影</strong><span>只展示可复核事件，不包含隐藏推理、系统提示词或模型原始输入。</span></p></div>
      <label class="audit-filter"><span>阶段</span><select><option>全部阶段</option><option>理解</option><option>检索</option><option>选源</option><option>结论</option></select></label>
      <ol class="audit-list">
        ${entries.map(([time, title, description]) => `<li><time>${time}</time><span class="audit-node"></span><div><strong>${title}</strong><p>${description}</p></div></li>`).join("")}
      </ol>
      <button class="load-more">加载更早记录</button>
    </div>`;
}

function renderInspectorState(): string {
  if (store.state === "running") {
    return `<div class="inspector-message"><span class="mini-loader"></span><h3>概览将在研究完成后生成</h3><p>运行期间只显示已确认的状态，不推测进度或虚构研究步骤。</p></div>`;
  }
  if (store.state === "error") {
    return `<div class="inspector-message is-error"><i data-lucide="circle-alert"></i><h3>检索阶段中断</h3><p>问题理解已经保存，但尚无足够来源生成研究概览。</p><button data-action="retry-research">重新研究</button></div>`;
  }
  return `<div class="inspector-message"><i data-lucide="book-open-text"></i><h3>还没有研究记录</h3><p>提交问题后，这里会按需显示研究覆盖和主要来源。</p></div>`;
}

function setState(nextState: DemoState): void {
  store.state = nextState;
  if (nextState === "empty") {
    store.activeConversationId = "";
    store.inspectorOpen = false;
  }
  render();
}

function simulateResearch(question?: string): void {
  if (question?.trim()) store.submittedQuestion = question.trim();
  store.activeConversationId = "ai-search";
  store.state = "running";
  store.inspectorOpen = shouldOpenInspector();
  store.inspectorTab = "overview";
  render();

  window.setTimeout(() => {
    store.activeConversationId = "four-day-week";
    store.state = "complete";
    render();
  }, 2200);
}

root.addEventListener("click", (event) => {
  const target = (event.target as HTMLElement).closest<HTMLElement>("[data-action]");
  if (!target) return;
  const action = target.dataset.action;

  if (action === "open-sidebar") store.sidebarOpen = true;
  if (action === "close-sidebar") store.sidebarOpen = false;
  if (action === "toggle-theme") {
    store.theme = store.theme === "light" ? "dark" : "light";
    try {
      window.localStorage.setItem("traceable-demo-theme", store.theme);
    } catch {
      // The demo remains usable if browser storage is unavailable.
    }
    render();
    return;
  }
  if (action === "toggle-inspector") store.inspectorOpen = !store.inspectorOpen;
  if (action === "close-inspector") store.inspectorOpen = false;
  if (action === "show-overview") store.inspectorTab = "overview";
  if (action === "show-audit") store.inspectorTab = "audit";
  if (action === "new-research") setState("empty");
  if (action === "retry-research") simulateResearch();

  if (action === "select-conversation") {
    const item = conversations.find((conversation) => conversation.id === target.dataset.id);
    if (item) {
      store.activeConversationId = item.id;
      store.state = item.state;
      store.submittedQuestion = item.state === "running"
        ? "主流 AI 搜索产品如何让用户判断一句结论来自哪个网页？"
        : item.state === "error"
          ? "远程办公是否会降低团队产生突破性创新的概率？"
          : "四天工作制对知识型团队的绩效和员工健康有什么影响？请区分短期试点与长期证据。";
      store.sidebarOpen = false;
      store.inspectorOpen = item.state !== "empty" && shouldOpenInspector();
    }
  }

  if (action === "open-source" || action === "select-source") {
    store.selectedSource = Number(target.dataset.source);
    store.inspectorOpen = true;
    store.inspectorTab = "overview";
  }

  if (action === "use-prompt") {
    const input = root.querySelector<HTMLTextAreaElement>("#research-input");
    if (input) {
      input.value = target.textContent?.trim() ?? "";
      input.focus();
    }
    return;
  }

  render();
});

root.addEventListener("submit", (event) => {
  if (!(event.target instanceof HTMLFormElement) || event.target.id !== "demo-composer") return;
  event.preventDefault();
  const input = event.target.querySelector<HTMLTextAreaElement>("#research-input");
  if (!input?.value.trim()) {
    input?.focus();
    return;
  }
  simulateResearch(input.value);
});

root.addEventListener("input", (event) => {
  const input = event.target;
  if (input instanceof HTMLInputElement && input.id === "conversation-filter") {
    const query = input.value.trim().toLocaleLowerCase();
    root.querySelectorAll<HTMLElement>(".conversation-item").forEach((item) => {
      item.hidden = !item.dataset.filterText?.includes(query);
    });
  }
  if (input instanceof HTMLTextAreaElement && input.id === "research-input") {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 150)}px`;
  }
});

root.addEventListener("keydown", (event) => {
  if (!(event.target instanceof HTMLTextAreaElement) || event.target.id !== "research-input") return;
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    event.target.form?.requestSubmit();
  }
});

const demoSwitcher = document.createElement("div");
demoSwitcher.className = "demo-switcher";
demoSwitcher.setAttribute("aria-label", "切换演示状态");
demoSwitcher.innerHTML = `
  <span>演示状态</span>
  ${(["complete", "running", "empty", "error"] as DemoState[]).map((state) => `<button data-demo-state="${state}" class="${state === store.state ? "is-active" : ""}">${stateLabels[state]}</button>`).join("")}`;
document.body.append(demoSwitcher);

function syncDemoSwitcher(): void {
  demoSwitcher.querySelectorAll<HTMLButtonElement>("[data-demo-state]").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.demoState === store.state);
  });
}

demoSwitcher.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-demo-state]");
  if (!button) return;
  const state = button.dataset.demoState as DemoState;
  if (state === "complete") store.activeConversationId = "four-day-week";
  if (state === "running") {
    store.activeConversationId = "ai-search";
    store.submittedQuestion = "主流 AI 搜索产品如何让用户判断一句结论来自哪个网页？";
  }
  if (state === "error") {
    store.activeConversationId = "remote-work";
    store.submittedQuestion = "远程办公是否会降低团队产生突破性创新的概率？";
  }
  store.state = state;
  store.inspectorOpen = state !== "empty" && shouldOpenInspector();
  render();
});

render();
