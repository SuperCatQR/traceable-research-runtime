import type { ResearchTurnStatus } from "../../../research-workspace-client";

export const statusLabels: Record<ResearchTurnStatus, string> = {
  clarifying: "理解中",
  ready: "即将开始",
  running: "检索中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
};
