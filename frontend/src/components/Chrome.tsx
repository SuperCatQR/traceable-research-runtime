import {
  BookOpenText,
  ChevronsLeft,
  ChevronsRight,
  CircleAlert,
  FilePlus2,
  History,
  Menu,
  PanelLeftClose,
  PanelLeftOpen,
  ShieldCheck,
  X,
} from "lucide-react";
import type { DemoScenarioId, ResearchConversation } from "../types";

const scenarioOptions: Array<{ id: DemoScenarioId; label: string }> = [
  { id: "first-use", label: "首次" },
  { id: "research", label: "研究" },
  { id: "recovery", label: "恢复" },
];

interface GlobalRailProps {
  collapsed: boolean;
  onToggleSidebar: () => void;
  onScenarioChange: (scenario: DemoScenarioId) => void;
}

export function GlobalRail({
  collapsed,
  onToggleSidebar,
  onScenarioChange,
}: GlobalRailProps) {
  return (
    <aside className="global-rail" aria-label="全局导航">
      <button
        className="brand-mark"
        type="button"
        aria-label="打开研究记录"
        title="研究记录"
        onClick={() => onScenarioChange("research")}
      >
        迹
      </button>
      <nav className="rail-actions" aria-label="快捷入口">
        <button
          className="icon-button is-active"
          type="button"
          aria-label="研究记录"
          title="研究记录"
          onClick={() => onScenarioChange("research")}
        >
          <BookOpenText aria-hidden="true" size={19} />
        </button>
        <button
          className="icon-button"
          type="button"
          aria-label="新建研究"
          title="新建研究"
          onClick={() => onScenarioChange("first-use")}
        >
          <FilePlus2 aria-hidden="true" size={19} />
        </button>
        <button
          className="icon-button"
          type="button"
          aria-label="恢复研究"
          title="恢复研究"
          onClick={() => onScenarioChange("recovery")}
        >
          <CircleAlert aria-hidden="true" size={19} />
        </button>
      </nav>
      <button
        className="icon-button rail-collapse"
        type="button"
        aria-label={collapsed ? "展开侧栏" : "收起侧栏"}
        title={collapsed ? "展开侧栏" : "收起侧栏"}
        onClick={onToggleSidebar}
      >
        {collapsed ? (
          <ChevronsRight aria-hidden="true" size={19} />
        ) : (
          <ChevronsLeft aria-hidden="true" size={19} />
        )}
      </button>
    </aside>
  );
}

interface ResearchSidebarProps {
  conversations: ResearchConversation[];
  scenario: DemoScenarioId;
  collapsed: boolean;
  mobileOpen: boolean;
  onScenarioChange: (scenario: DemoScenarioId) => void;
  onCloseMobile: () => void;
}

export function ResearchSidebar({
  conversations,
  scenario,
  collapsed,
  mobileOpen,
  onScenarioChange,
  onCloseMobile,
}: ResearchSidebarProps) {
  return (
    <>
      {mobileOpen ? (
        <button
          className="sidebar-scrim"
          type="button"
          aria-label="关闭侧栏"
          onClick={onCloseMobile}
        />
      ) : null}
      <aside
        className={`research-sidebar${collapsed ? " is-collapsed" : ""}${
          mobileOpen ? " is-mobile-open" : ""
        }`}
        aria-label="研究侧栏"
      >
        <div className="sidebar-heading">
          <div>
            <span className="utility-label">TRACEABLE RESEARCH</span>
            <strong>迹研</strong>
          </div>
          <button
            className="icon-button mobile-only"
            type="button"
            aria-label="关闭侧栏"
            title="关闭侧栏"
            onClick={onCloseMobile}
          >
            <X aria-hidden="true" size={19} />
          </button>
        </div>

        <button
          className="primary-command sidebar-new"
          type="button"
          onClick={() => {
            onScenarioChange("first-use");
            onCloseMobile();
          }}
        >
          <FilePlus2 aria-hidden="true" size={17} />
          新建研究
        </button>

        <div className="sidebar-section-label">
          <History aria-hidden="true" size={14} />
          研究记录
        </div>
        <div className="conversation-list">
          {conversations.map((conversation, conversationIndex) => (
            <section className="conversation-group" key={conversation.id}>
              <h2>{conversation.title}</h2>
              <div className="request-list">
                {conversation.requests.slice(0, conversationIndex === 0 ? 4 : 2).map((request) => {
                  const targetScenario: DemoScenarioId =
                    request.status === "interrupted" ? "recovery" : "research";
                  const interactive =
                    request.id === conversations[0]?.requests[0]?.id ||
                    request.status === "interrupted";
                  const isCurrent =
                    (scenario === "research" && targetScenario === "research" && interactive) ||
                    (scenario === "recovery" && targetScenario === "recovery");
                  const content = (
                    <>
                      <span className={`status-dot status-${request.status}`} aria-hidden="true" />
                      <span className="request-copy">
                        <strong>请求 #{request.number}</strong>
                        <small>{request.statusLabel}</small>
                      </span>
                    </>
                  );
                  return interactive ? (
                    <button
                      className={`request-row${isCurrent ? " is-current" : ""}`}
                      type="button"
                      key={request.id}
                      onClick={() => {
                        onScenarioChange(targetScenario);
                        onCloseMobile();
                      }}
                    >
                      {content}
                    </button>
                  ) : (
                    <div className="request-row is-static" key={request.id}>
                      {content}
                    </div>
                  );
                })}
              </div>
            </section>
          ))}
        </div>

        <div className="snapshot-footer">
          <div className="snapshot-footer-icon">
            <ShieldCheck aria-hidden="true" size={17} />
          </div>
          <div>
            <span>默认资料版本</span>
            <strong>劳动用工规则库 · 2026.07</strong>
          </div>
        </div>
      </aside>
    </>
  );
}

interface WorkspaceTopbarProps {
  scenario: DemoScenarioId;
  onScenarioChange: (scenario: DemoScenarioId) => void;
  onOpenMobileSidebar: () => void;
}

export function WorkspaceTopbar({
  scenario,
  onScenarioChange,
  onOpenMobileSidebar,
}: WorkspaceTopbarProps) {
  return (
    <header className="workspace-topbar">
      <button
        className="icon-button mobile-menu"
        type="button"
        aria-label="打开研究侧栏"
        title="打开研究侧栏"
        onClick={onOpenMobileSidebar}
      >
        <Menu aria-hidden="true" size={20} />
      </button>
      <div className="topbar-context">
        <span className="local-fixture-indicator">LOCAL FIXTURE</span>
        <span>劳动用工法规与裁判规则库</span>
      </div>
      <div className="scenario-control" aria-label="演示场景">
        {scenarioOptions.map((option) => (
          <button
            key={option.id}
            type="button"
            className={scenario === option.id ? "is-selected" : ""}
            aria-pressed={scenario === option.id}
            onClick={() => onScenarioChange(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </header>
  );
}

export function MobileSidebarToggle({
  open,
  onToggle,
}: {
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      className="icon-button"
      type="button"
      aria-label={open ? "关闭侧栏" : "打开侧栏"}
      title={open ? "关闭侧栏" : "打开侧栏"}
      onClick={onToggle}
    >
      {open ? (
        <PanelLeftClose aria-hidden="true" size={19} />
      ) : (
        <PanelLeftOpen aria-hidden="true" size={19} />
      )}
    </button>
  );
}
