import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ResearchWorkspaceRequestError } from "../../research-workspace-client";
import { WorkspaceGatewayProvider, type WorkspaceGateway } from "../data/workspace-gateway";
import { createDemoWorkspaceGateway, type DemoWorkspaceScenario } from "../test/demo-workspace-gateway";
import { App } from "./App";

function renderApp(
  path = "/research",
  gateway: WorkspaceGateway = createDemoWorkspaceGateway(),
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity }, mutations: { retry: false } },
  });
  render(
    <MemoryRouter initialEntries={[path]}>
      <QueryClientProvider client={queryClient}>
        <WorkspaceGatewayProvider gateway={gateway}>
          <App />
        </WorkspaceGatewayProvider>
      </QueryClientProvider>
    </MemoryRouter>,
  );
}

function renderScenario(scenario: DemoWorkspaceScenario, path = "/research") {
  renderApp(path, createDemoWorkspaceGateway(scenario));
}

afterEach(() => {
  cleanup();
  window.sessionStorage.clear();
});

describe("React workspace", () => {
  it("restores an authenticated multi-turn workspace", async () => {
    renderApp();
    await waitFor(() => expect(screen.getAllByText("四天工作制的适用边界").length).toBeGreaterThan(0));
    expect(await screen.findByRole("navigation", { name: "对话回合快速跳转" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /综合现有证据/ })).toBeInTheDocument();
  });

  it("loads the research overview only after the user opens it", async () => {
    const user = userEvent.setup();
    renderApp("/research/conversation-four-day-week");
    const trigger = await screen.findByTitle("研究概览");
    expect(screen.queryByText("问题理解")).not.toBeInTheDocument();
    await user.click(trigger);
    expect(await screen.findByText("问题理解")).toBeInTheDocument();
    expect(screen.getByText("gpt-5-research")).toBeInTheDocument();
  });

  it("paginates and filters audit records on demand", async () => {
    const user = userEvent.setup();
    renderApp("/research/conversation-four-day-week");
    await user.click(await screen.findByTitle("研究概览"));
    await user.click(screen.getByRole("tab", { name: "审计详情" }));
    expect(await screen.findByText("问题理解完成")).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText("阶段"), "search");
    expect(screen.getByLabelText("阶段")).toHaveValue("search");
    await user.click(await screen.findByRole("button", { name: "加载更多审计记录" }));

    expect(await screen.findByText("完成结论综合")).toBeInTheDocument();
  });

  it.each([
    ["running" as const, "正在检索、锁定快照并核验来源"],
    ["error" as const, "研究未完成。你可以继续提出新的研究问题。"],
  ])("renders the %s turn state", async (scenario, expected) => {
    renderScenario(scenario);
    expect(await screen.findByText(expected)).toBeInTheDocument();
  });

  it("creates the first conversation from an empty authenticated workspace", async () => {
    const user = userEvent.setup();
    renderScenario("empty");
    const heading = await screen.findByRole("heading", { name: "开始一项研究" });

    await user.click(within(heading.closest(".workspace-empty")!).getByRole("button", { name: "新研究" }));

    expect(await screen.findByRole("heading", { name: "写下需要查证的问题" })).toBeInTheDocument();
    await user.type(screen.getByLabelText("研究问题"), "比较长期试点结果");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("你希望我优先比较长期证据，还是先整理近期可执行建议？")).toBeInTheDocument();

    await user.type(screen.getByLabelText("继续对话"), "优先比较长期证据");
    await user.click(screen.getByRole("button", { name: "发送" }));
    expect(await screen.findByText("正在检索、锁定快照并核验来源")).toBeInTheDocument();
  });

  it("logs in through the auth action seam and enters the workspace", async () => {
    const user = userEvent.setup();
    const gateway = createDemoWorkspaceGateway();
    gateway.auth.current = async () => {
      throw new ResearchWorkspaceRequestError("需要登录", 401, "authentication_required", false);
    };
    renderApp("/login", gateway);
    expect(await screen.findByRole("heading", { name: "返回研究工作区" })).toBeInTheDocument();

    await user.type(screen.getByLabelText("邮箱"), "researcher@example.com");
    await user.type(screen.getByLabelText("密码"), "long-enough-password");
    await user.click(screen.getByRole("button", { name: "登录" }));

    expect(await screen.findByRole("heading", { name: "四天工作制的适用边界" })).toBeInTheDocument();
  });

  it("registers a new account through the same auth action seam", async () => {
    const user = userEvent.setup();
    const gateway = createDemoWorkspaceGateway();
    gateway.auth.current = async () => {
      throw new ResearchWorkspaceRequestError("需要登录", 401, "authentication_required", false);
    };
    renderApp("/register", gateway);
    expect(await screen.findByRole("heading", { name: "创建研究账户" })).toBeInTheDocument();

    await user.type(screen.getByLabelText("显示名称"), "新研究者");
    await user.type(screen.getByLabelText("邮箱"), "new@example.com");
    await user.type(screen.getByLabelText("密码"), "long-enough-password");
    await user.click(screen.getByRole("button", { name: "创建账户" }));

    expect(await screen.findByRole("heading", { name: "四天工作制的适用边界" })).toBeInTheDocument();
  });

  it("routes an account without model profiles to first-time setup", async () => {
    const user = userEvent.setup();
    const gateway = createDemoWorkspaceGateway("setup");
    renderApp("/research", gateway);

    expect(await screen.findByRole("dialog", { name: "模型配置" })).toBeInTheDocument();
    await user.type(screen.getByLabelText("配置名称"), "主要研究模型");
    await user.type(screen.getByLabelText("API 地址"), "https://api.example.com/v1/");
    await user.type(screen.getByLabelText("模型 ID"), "research-model");
    await user.type(screen.getByLabelText("API Key"), "secret-key");
    await user.click(screen.getByRole("button", { name: "保存配置" }));

    expect(await screen.findByText("主要研究模型")).toBeInTheDocument();
  });

  it("renames, archives, and restores a conversation", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    renderApp("/research/conversation-four-day-week");
    await screen.findByRole("heading", { name: "四天工作制的适用边界" });

    await user.click(screen.getByRole("button", { name: "重命名对话" }));
    const title = screen.getByLabelText("对话标题");
    await user.clear(title);
    await user.type(title, "四天工作制复核");
    await user.click(screen.getByRole("button", { name: "保存标题" }));
    expect(await screen.findByRole("heading", { name: "四天工作制复核" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "归档对话" }));
    expect(await screen.findByRole("heading", { name: "开始一项研究" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "归档" }));
    const archivedTitle = await screen.findByText("四天工作制复核");
    await user.click(within(archivedTitle.closest(".archive-row")!).getByRole("button", { name: "恢复" }));

    expect(await screen.findByRole("heading", { name: "四天工作制复核" })).toBeInTheDocument();
  });

  it("shows a retryable boot failure when account restoration cannot reach the server", async () => {
    const gateway = createDemoWorkspaceGateway();
    gateway.auth.current = async () => {
      throw new Error("network unavailable");
    };
    renderApp("/research", gateway);

    expect(await screen.findByRole("heading", { name: "无法恢复工作区" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重新连接" })).toBeInTheDocument();
  });
});
