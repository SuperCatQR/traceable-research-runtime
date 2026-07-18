# 已完成：模型主导的对话式问题校准与自动研究

> Historical completion record. ADR 0006 is the current normative decision.

- 状态：已实施
- 提出日期：2026-07-16
- 完成日期：2026-07-16
- 正式决策：[ADR 0006](../../adr/0006-model-led-dialogue-and-tiered-trace-disclosure.md)

## 已确认的产品行为

用户只使用自然语言对话。模型在每次理解时生成一条给用户看的自然语言回复，同时通过提示词
生成内部结构化 `ResearchBrief`；后者是模型的语义工作产物，不是用户填写、查看、编辑或确认的
表单。

模型自主决定：

- `continue_dialogue`：把当前理解以自然语言告诉用户，等待下一条普通消息；
- `start_research`：自动冻结当前模型生成的 Brief，并立即开始研究。

因此不存在确认按钮、修改 Brief、选择题、专用澄清回复、手动“开始检索”按钮或业务上的追问
次数上限。模型可以自然表达仍需要了解的内容，但不需要把它编码成系统固定的问题格式。

## 实施契约

1. `src/clarification.rs` 以 schema v5 保存 `DialogueMessage` 与
   `ModelUnderstanding`；模型输出固定为 `decision`、`rationale`、`assistant_message` 和
   `brief_draft`。
2. `src/runtime.rs` 只接受 `submit_dialogue_message`；旧的专用澄清回复入口已删除，不保留
   API 兼容分支。
3. `demo-host` 只公开 `POST .../turns` 和 `POST .../turns/{turn_id}/messages` 来推进聊天。
   当模型状态为 `ResearchReady` 时，Host 自动准备和执行研究；浏览器没有确认或执行端点。
4. 聊天按时间顺序显示用户消息与模型的 `assistant_message`。内部 Brief、系统提示词、隐藏推理
   和模型原始输入不会显示。
5. 模型请求或结构化输出失败时，已有对话仍被保留。后续自然消息可重新触发理解；核心运行时也
   保留受控重试和取消能力。

## 部署边界

Clarification schema v5 不兼容旧 v2/v3/v4 `ask | complete` / 专用问题事件。该需求明确不考虑历史
未完成 Turn 的兼容或迁移；发布时必须配置新的运行数据目录或持久卷，而不是在旧 `intake` 日志
上继续写入。

## 后续不在本项范围内的议题

- 研究正在执行时如何处理用户的新消息（排队、取消重启或创建新 Turn）需要单独定义生命周期；
  当前同一 Conversation 仍只允许一个未完成 Turn。
- Google 优先、Bing 回退的搜索策略另见
  [搜索计划](../plans/2026-07-16-google-first-searxng-search-fallback.md)。
