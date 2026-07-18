import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { BrowserRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

function renderApp(path = "/") {
  window.history.replaceState(null, "", path);
  return render(
    <BrowserRouter>
      <App />
    </BrowserRouter>,
  );
}

describe("traceable research demo", () => {
  beforeEach(() => {
    vi.useRealTimers();
    window.history.replaceState(null, "", "/");
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("shows source-bound answer sections and supports switching and comparing answers", () => {
    renderApp();

    expect(screen.getByRole("heading", { name: "研究答案" })).toBeInTheDocument();
    expect(screen.getByText("支付前提")).toBeInTheDocument();
    expect(screen.getAllByText("文档关联结论")).toHaveLength(2);
    expect(screen.getByText("文档结论 + 模型补充")).toBeInTheDocument();
    expect(screen.getByText("模型补充")).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "引用 1" })[0]);
    const sourceLedger = screen.getByLabelText("来源账页");
    expect(within(sourceLedger).getByRole("heading", { name: "逐字来源" })).toBeInTheDocument();
    expect(within(sourceLedger).getByText("中华人民共和国劳动合同法")).toBeInTheDocument();

    const answerModeControl = screen.getByLabelText("回答方式");
    fireEvent.click(within(answerModeControl).getByRole("button", { name: "综合解读" }));
    expect(screen.getByText("先说结论")).toBeInTheDocument();
    expect(screen.queryByText("支付前提")).not.toBeInTheDocument();

    const compareButton = screen.getByRole("button", { name: "对照" });
    fireEvent.click(compareButton);
    expect(compareButton).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("先说结论")).toBeInTheDocument();
    expect(screen.getByText("支付前提")).toBeInTheDocument();
  });

  it("completes the first-use flow from snapshot selection to the finished result", () => {
    renderApp("/research/new");

    expect(
      screen.getByRole("heading", { name: "选择 Markdown Corpus Snapshot" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: /2025-01-01 历史冻结版/ }));
    fireEvent.click(screen.getByRole("button", { name: "下一步" }));

    expect(screen.getByRole("heading", { name: "提出研究问题" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "研究问题" }), {
      target: { value: "上海解除劳动合同时，经济补偿工资基数如何计算？" },
    });
    fireEvent.click(screen.getByRole("button", { name: "评估问题" }));

    expect(
      screen.getByRole("heading", { name: "补充会改变研究范围的条件" }),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByRole("textbox", { name: "补充说明" }), {
      target: { value: "工作地为上海，计划于 2026 年解除劳动合同。" },
    });
    fireEvent.click(screen.getByRole("button", { name: "提交补充" }));

    expect(
      screen.getByRole("heading", { name: "问题已明确，可以冻结本次输入" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/2025-01-01 历史冻结版/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "开始研究" }));

    expect(
      screen.getByRole("heading", { name: "正在锁定的资料中研究" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "查看完成结果" }));

    expect(screen.getByRole("heading", { name: "研究答案" })).toBeInTheDocument();
    expect(screen.getByText("支付前提")).toBeInTheDocument();
  });

  it("resumes an interrupted request and reaches completion after the recovery timer", () => {
    vi.useFakeTimers();
    renderApp(
      "/research/conversations/conversation-compensation-recovery/requests/request-economic-compensation-002",
    );

    expect(
      screen.getByRole("heading", { name: "研究暂时中断，检查点已保存" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续研究" }));
    expect(screen.getByRole("heading", { name: "正在从检查点继续" })).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1_100);
    });

    expect(screen.getByRole("heading", { name: "研究已继续并完成" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查看答案" })).toBeInTheDocument();
  });

  it("requires confirmation before cancellation and then exposes only the terminal path", () => {
    renderApp(
      "/research/conversations/conversation-compensation-recovery/requests/request-economic-compensation-002/execution",
    );

    const cancelTrigger = screen.getByRole("button", { name: "取消请求" });
    fireEvent.click(cancelTrigger);
    const firstDialog = screen.getByRole("dialog", { name: "取消这个 Request？" });
    expect(within(firstDialog).getByText(/之后不能恢复/)).toBeInTheDocument();
    expect(within(firstDialog).getByRole("button", { name: "返回" })).toHaveFocus();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(cancelTrigger).toHaveFocus();
    expect(screen.getByRole("button", { name: "继续研究" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "取消请求" }));
    fireEvent.click(
      within(screen.getByRole("dialog", { name: "取消这个 Request？" })).getByRole("button", {
        name: "确认取消",
      }),
    );

    const cancelledHeading = screen.getByRole("heading", { name: "请求 #2 已取消" });
    expect(cancelledHeading).toBeInTheDocument();
    expect(screen.getByText(/该 Request 不再恢复/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "继续研究" })).not.toBeInTheDocument();
    expect(
      within(cancelledHeading.parentElement!).getByRole("button", { name: "新建研究" }),
    ).toBeInTheDocument();
  });

  it("shows a uniform unavailable state for unknown research object IDs", () => {
    const unknownPath =
      "/research/conversations/unknown-conversation/requests/unknown-request/answer";
    renderApp(unknownPath);

    expect(screen.getByRole("heading", { name: "内容不可用" })).toBeInTheDocument();
    expect(window.location.pathname).toBe(unknownPath);

    fireEvent.click(screen.getByRole("button", { name: "返回研究记录" }));
    expect(screen.getByRole("heading", { name: "研究答案" })).toBeInTheDocument();
    expect(window.location.pathname).toMatch(
      /^\/research\/conversations\/[^/]+\/requests\/[^/]+\/answer$/,
    );
  });
});
