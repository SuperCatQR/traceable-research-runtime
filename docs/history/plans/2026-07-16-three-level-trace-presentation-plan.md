# 三级 Trace 展示计划

> Historical implementation plan. The completed Trace v7 follow-up is archived in `2026-07-16-trace-boundary-hardening-plan.md`.

- 状态：已实施
- 日期：2026-07-16
- 正式决策：[ADR 0006](../../adr/0006-model-led-dialogue-and-tiered-trace-disclosure.md)

## 决策目标

聊天应该保持自然、简洁；用户不应因为可审计性而被查询词、原始事件或模型过程淹没。Trace
仍完整落盘，但浏览器按用途和敏感度分三层获取。任何层级都不披露系统提示词、隐藏推理、API
key、模型原始输入或完整快照正文。

## 已实施的层级

| 等级 | 名称与位置 | 内容 | 不包含 |
| --- | --- | --- | --- |
| L1 | 聊天正文 | 用户/助手自然消息、研究状态、最终答案和必要来源 | 查询、选源理由、模型知识草稿、主张理由、原始事件 |
| L2 | 右侧“研究概览” | 当前 Turn 的理解摘要、每轮检索方向和结果数、归档/跳过数、最多六个主要来源、合成理由和失败摘要 | 原始事件、所有搜索结果、完整快照正文 |
| L3 | 右侧“审计详情” | 可按阶段筛选、分页的审阅安全事件：自然对话、准备、规划、搜索、归档、选源、合成和失败 | 系统提示词、隐藏推理、密钥、原始模型输入、完整快照正文 |

L1 不显示“Trace”标签；它是普通对话。L2 面向需要理解研究过程的用户，L3 面向调试、复盘和
审计。L3 是审阅安全事件详情，不是模型的 chain-of-thought。

## 数据来源与投影

核心仍写入两类 append-only JSONL：

- `data/intake/<clarification_id>.jsonl`：schema v5，保存原问题、普通用户消息、模型理解、模型
  的 `continue_dialogue | start_research` 决定、自动研究准备、准备失败、准备后的运行失败、取消和失败；
- `data/traces/<run_id>.jsonl`：schema v6，保存运行头、模型调用、知识草稿、查询、搜索结果、
  归档/跳过、导航摘录、选源、主张、最终答案、轮次检查点和失败。

Demo Host 从受拥有权保护的日志读取数据：

1. L2 使用服务端 `project_trace_summary` 白名单投影。它只返回理解、覆盖和主要来源摘要，而不
   返回 `clarification_events` 或 `research_events` 数组。
2. L3 使用 `project_audit_entries` 生成审阅安全字段 `stage`、`label`、`detail` 和可选
   `rationale`。大集合先按阶段过滤，再按 `cursor` / `limit` 分页，单页最多 100 项。
3. 对尚未开始 Research Run 的 Turn，L2/L3 仍可读取 Clarification 日志；`run_id` 与研究
   理由审计状态为空。

## HTTP 契约

```text
GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/summary
GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/audit?stage=&cursor=&limit=
```

可用 `stage` 为 `dialogue`、`setup`、`planning`、`search`、`archive`、`selection`、`synthesis`
和 `failure`。两个端点都必须先验证登录用户拥有 Conversation 和 Turn：未登录返回 `401`，其他
用户的对象返回 `404`。前端只在打开右侧检查器时懒加载 L2；切换到“审计详情”才加载 L3，并在
切换 Turn、注销或刷新时丢弃不再拥有的缓存。

## 前端行为

- 聊天中不再出现“查看决策记录”、模型知识草稿、主张理由或手动研究按钮。
- 右侧检查器默认打开“研究概览”，可切换到“审计详情”；审计提供阶段筛选、加载、空、失败和
  分页状态。
- 窄屏时检查器以右侧全宽抽屉呈现，不压缩聊天正文。外部文本按普通文本转义后渲染。

## 已完成的验证目标

- L2 字段白名单、L3 分页与阶段过滤均由服务端测试；L2 不泄露原始事件集合。
- 未认证和非所有者访问 summary/audit 被拒绝。
- 自然对话 `ModelUnderstanding` 的可见消息位于 L1；对应决定及其审阅理由位于 L2/L3。
- 桌面与窄屏聊天正文不再内联过程性 Trace，右侧检查器负责按需披露。

## 复核记录

- 2026-07-16：L1 进一步收紧为普通对话消息、必要的研究进行状态、最终答案和来源。轮次号、模型
  标识和过程性标签不再出现在聊天正文；仅承担视觉引导的样式命名为
  `assistant-message-accent`，避免把普通聊天装饰误称为 Trace。
- 2026-07-16：L3 前端筛选补齐 `setup`（研究准备与运行初始化），与服务端审计契约保持一一对应。
  自动准备与初始化失败的可审阅标签统一为“研究准备失败”和“研究运行初始化失败”。
- 2026-07-16：L2 失败摘要区分 `preparation`（尚未生成运行标识）与 `initialization`（已准备后运行
  初始化失败），不再用“准备”笼统描述两种不同状态。
