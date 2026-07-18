import { useEffect, useState } from "react";
import { ArrowLeft, CircleAlert } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import {
  GlobalRail,
  ResearchSidebar,
  WorkspaceTopbar,
} from "./components/Chrome";
import { FirstUseWorkspace } from "./components/FirstUseWorkspace";
import { RecoveryWorkspace } from "./components/RecoveryWorkspace";
import { ResearchWorkspace } from "./components/ResearchWorkspace";
import { demoFixtures } from "./fixtures";
import type {
  AnswerMode,
  DemoScenarioId,
  WorkspaceView,
} from "./types";

function viewFromPath(pathname: string): WorkspaceView {
  if (pathname.endsWith("/execution")) {
    return "execution";
  }
  if (pathname.endsWith("/audit")) {
    return "audit";
  }
  return "answer";
}

interface DemoRoute {
  scenario: DemoScenarioId;
  view: WorkspaceView;
  unavailable: boolean;
}

function routeFromPath(pathname: string): DemoRoute {
  const normalizedPathname = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  if (normalizedPathname === "/" || normalizedPathname === "/research") {
    return { scenario: "research", view: "answer", unavailable: false };
  }
  if (
    normalizedPathname === "/research/new" ||
    normalizedPathname === "/research/new/corpus"
  ) {
    return { scenario: "first-use", view: "answer", unavailable: false };
  }

  const match = normalizedPathname.match(
    /^\/research\/conversations\/([^/]+)\/requests\/([^/]+)(?:\/(answer|execution|audit))?\/?$/,
  );
  if (!match) {
    return { scenario: "research", view: "answer", unavailable: true };
  }

  const [, conversationId, requestId, requestedView] = match;
  const conversation = demoFixtures.conversations.find((item) => item.id === conversationId);
  const request = conversation?.requests.find((item) => item.id === requestId);
  if (!request) {
    return { scenario: "research", view: "answer", unavailable: true };
  }
  if (request.id === demoFixtures.normalRequest.id) {
    return { scenario: "research", view: viewFromPath(normalizedPathname), unavailable: false };
  }
  if (
    request.id === demoFixtures.recoveryRequest.id &&
    (requestedView === undefined || requestedView === "execution")
  ) {
    return { scenario: "recovery", view: "execution", unavailable: false };
  }
  return { scenario: "research", view: "answer", unavailable: true };
}

function pathForScenario(scenario: DemoScenarioId, view: WorkspaceView): string {
  if (scenario === "first-use") {
    return "/research/new";
  }
  const request =
    scenario === "recovery" ? demoFixtures.recoveryRequest : demoFixtures.normalRequest;
  const conversation = demoFixtures.conversations.find((item) =>
    item.requests.some((candidate) => candidate.id === request.id),
  );
  const suffix = scenario === "recovery" ? "execution" : view;
  return `/research/conversations/${conversation?.id ?? "conversation"}/requests/${request.id}/${suffix}`;
}

export function App() {
  const location = useLocation();
  const navigate = useNavigate();
  const initialRoute = routeFromPath(window.location.pathname);
  const [scenario, setScenario] = useState<DemoScenarioId>(initialRoute.scenario);
  const [activeView, setActiveView] = useState<WorkspaceView>(initialRoute.view);
  const [routeUnavailable, setRouteUnavailable] = useState(initialRoute.unavailable);
  const [answerMode, setAnswerMode] = useState<AnswerMode>("evidence-first");
  const [compareAnswers, setCompareAnswers] = useState(false);
  const [selectedCitationId, setSelectedCitationId] = useState<string | null>(
    demoFixtures.normalRequest.citations[0]?.id ?? null,
  );
  const [sourceLedgerOpen, setSourceLedgerOpen] = useState(
    () => window.innerWidth >= 1200,
  );
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);

  useEffect(() => {
    const route = routeFromPath(location.pathname);
    setRouteUnavailable(route.unavailable);
    if (route.unavailable) {
      return;
    }
    setScenario(route.scenario);
    setActiveView(route.view);
    const canonicalPath = pathForScenario(route.scenario, route.view);
    if (location.pathname !== canonicalPath) {
      navigate(canonicalPath, { replace: true });
    }
  }, [location.pathname, navigate]);

  useEffect(() => {
    if (routeUnavailable) {
      document.title = "内容不可用 · 迹研";
      return;
    }
    const titles: Record<DemoScenarioId, string> = {
      "first-use": "新建研究",
      research: demoFixtures.normalRequest.shortTitle,
      recovery: "恢复研究",
    };
    document.title = `${titles[scenario]} · 迹研`;
  }, [routeUnavailable, scenario]);

  function changeScenario(nextScenario: DemoScenarioId) {
    const nextView = nextScenario === "recovery" ? "execution" : "answer";
    setScenario(nextScenario);
    setRouteUnavailable(false);
    setMobileSidebarOpen(false);
    if (nextScenario === "research") {
      setActiveView(nextView);
      setAnswerMode("evidence-first");
      setCompareAnswers(false);
      setSelectedCitationId(demoFixtures.normalRequest.citations[0]?.id ?? null);
      setSourceLedgerOpen(window.innerWidth >= 1200);
    } else if (nextScenario === "recovery") {
      setActiveView(nextView);
      setSourceLedgerOpen(false);
    }
    navigate(pathForScenario(nextScenario, nextView));
  }

  function changeView(view: WorkspaceView) {
    setActiveView(view);
    if (view !== "answer") {
      setSourceLedgerOpen(false);
    }
    navigate(pathForScenario(scenario, view));
  }

  return (
    <div className={`app-shell${sidebarCollapsed ? " sidebar-collapsed" : ""}`}>
      <GlobalRail
        collapsed={sidebarCollapsed}
        onToggleSidebar={() => setSidebarCollapsed((collapsed) => !collapsed)}
        onScenarioChange={changeScenario}
      />
      <ResearchSidebar
        conversations={demoFixtures.conversations}
        scenario={scenario}
        collapsed={sidebarCollapsed}
        mobileOpen={mobileSidebarOpen}
        onScenarioChange={changeScenario}
        onCloseMobile={() => setMobileSidebarOpen(false)}
      />
      <div className="app-content">
        <WorkspaceTopbar
          scenario={scenario}
          onScenarioChange={changeScenario}
          onOpenMobileSidebar={() => setMobileSidebarOpen(true)}
        />
        {routeUnavailable ? (
          <UnavailableWorkspace onReturn={() => changeScenario("research")} />
        ) : null}
        {!routeUnavailable && scenario === "first-use" ? (
          <FirstUseWorkspace
            snapshots={demoFixtures.snapshots}
            onOpenCompletedResearch={() => changeScenario("research")}
          />
        ) : null}
        {!routeUnavailable && scenario === "research" ? (
          <ResearchWorkspace
            request={demoFixtures.normalRequest}
            activeView={activeView}
            answerMode={answerMode}
            compareAnswers={compareAnswers}
            selectedCitationId={selectedCitationId}
            sourceLedgerOpen={sourceLedgerOpen}
            onViewChange={changeView}
            onAnswerModeChange={setAnswerMode}
            onCompareChange={(compare) => {
              setCompareAnswers(compare);
              if (compare) {
                setSourceLedgerOpen(false);
              }
            }}
            onCitationSelect={setSelectedCitationId}
            onSourceLedgerOpenChange={(open) => {
              setSourceLedgerOpen(open);
              if (open) {
                setCompareAnswers(false);
              }
            }}
          />
        ) : null}
        {!routeUnavailable && scenario === "recovery" ? (
          <RecoveryWorkspace
            request={demoFixtures.recoveryRequest}
            onOpenCompletedResearch={() => changeScenario("research")}
            onNewResearch={() => changeScenario("first-use")}
          />
        ) : null}
      </div>
    </div>
  );
}

function UnavailableWorkspace({ onReturn }: { onReturn: () => void }) {
  return (
    <div className="terminal-state-workspace">
      <div className="terminal-state-mark">
        <CircleAlert aria-hidden="true" size={25} />
      </div>
      <span className="section-eyebrow">CONTENT UNAVAILABLE</span>
      <h1>内容不可用</h1>
      <p>该研究记录不存在、当前演示数据未包含它，或你无权访问。</p>
      <button className="primary-command" type="button" onClick={onReturn}>
        <ArrowLeft aria-hidden="true" size={16} />
        返回研究记录
      </button>
    </div>
  );
}
