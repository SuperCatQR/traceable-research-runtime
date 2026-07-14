# Research Intake 对话体验修正计划

> 状态：待实施
>
> 日期：2026-07-14
>
> 依据：[`web-search-architecture.md`](./web-search-architecture.md) §4.4
>
> 基线：`18f430e`。现有后端已实现“模型生成 Brief、每轮至多一个质询、累计至多五问、用户可随时确认”；本计划只修正 WebUI 将其呈现成单问表单而非连续对话的问题。

## 1. 问题与目标

当前 `IntakeSession` 已保存 `original_question`、按顺序排列的 `questions` 与 `answers`、当前 `brief_draft`，模型每次回复后也会修订 Brief。缺口在 HTTP 响应未投影历史、前端只显示当前问题，用户看不到自己与模型如何逐步确定 Brief。

目标流程：

```text
用户提出研究问题
  -> 页面显示用户消息
  -> 模型生成 Brief，并显示一个质询（若有）
  -> 用户在同一对话区回答
  -> 页面保留旧消息，追加用户回答与模型新质询
  -> 右侧/下方持续显示模型生成的当前 Brief
  -> 用户任一时刻确认当前 Brief
  -> 进入既有 Research 流程
```

完成后，“对话”是既有 Intake 状态的可见投影，不另建聊天子系统。

## 2. 最小实现原则

1. 复用 `IntakeSession.questions` 与 `IntakeSession.answers`，不新增事件、不改 JSONL schema、不迁移数据。
2. 保留现有四类 Intake 写操作及请求体；用户回答仍只对应当前 pending question，不开放任意轮次编辑。
3. Brief 仍由模型生成、客户端只读；不恢复人工填写 `brief_draft`。
4. 不增加 WebSocket/SSE、前端框架、数据库表或新依赖；每次写请求仍返回完整 Intake 投影。
5. 保持五问上限、显式确认、revision/hash 并发保护、失败恢复与确认幂等语义不变。

懒惰替代：只改 CSS 把现表单画成聊天样式虽更短，但无法显示历史，不能满足“在对话中确定 Brief”，故不采用。

## 3. 影响范围

### 3.1 必改

| 文件 | 最小改动 | 理由 |
|---|---|---|
| `src/web.rs` | 在 `IntakeResponse` 增加只读 `messages`；由现有 `original_question + questions + answers` 生成有序消息 | 页面刷新及每轮响应均可重建完整对话，无需前端另存状态 |
| `src/web/index.html` | 将当前单问区改成消息流；保留当前回答输入、选项、确认、取消、重试及最小 Brief 操作 | 修复用户可见体验，复用全部既有 API |

### 3.2 仅测试随同修改

测试优先放回上述文件的既有 `#[cfg(test)]` 模块或静态页面断言中，不新建测试框架。只有现有测试结构无法清楚覆盖时，才新增一个最小测试文件。

### 3.3 明确不改

- `src/intake.rs`：事件、reducer、五问计数、revision/hash、恢复逻辑已足够。
- `src/app.rs`：模型调用已携带完整问答史，服务命令无需变化。
- `src/backend.rs`：模型输出协议仍为 `brief_draft + question + ready_to_confirm`。
- Research 执行、搜索、抓取、快照与 trace。
- `data/intake/*.jsonl` 和现网历史数据。
- `docs/web-search-architecture.md`：本次按其既有契约补齐实现，不重写架构。

若实施中发现必须改上述边界才能显示历史，先停止扩面并复核现状，不静默升级协议。

## 4. 响应投影设计

### 4.1 最小 DTO

在 `src/web.rs` 增加仅用于序列化的消息 DTO：

```rust
struct IntakeMessage<'a> {
    role: &'static str, // "user" | "assistant"
    kind: &'static str, // "original_question" | "clarification" | "answer"
    text: &'a str,
}
```

`IntakeResponse.messages` 顺序固定为：

1. 原始问题：`user / original_question`。
2. 第一个质询：`assistant / clarification`。
3. 该质询的回答：`user / answer`。
4. 后续质询与回答依次交替。
5. 当前尚未回答的 pending question 位于末尾。

`question` 字段暂时保留，供现有输入控件取得 `id/options`，避免把消息 DTO 扩成另一套问题模型。`brief_draft` 仍独立返回并持续展示，不把每个 Brief revision 复制进消息史。

### 4.2 配对规则

按 `ClarificationAnswer.question_id` 与 `ClarificationQuestion.id` 配对，不依赖数组下标。历史兼容规则：

- 有问题无回答：显示问题。
- 有匹配回答：紧随对应问题显示。
- 旧日志中的多个问题：按 `questions` 原顺序显示各问题及匹配回答。
- 孤立回答属于损坏/遗留异常状态，不暴露为无上下文消息；reducer 的既有校验仍是数据正确性的主防线。

投影函数保持纯函数，便于单元测试；不写盘、不改变 session。

## 5. WebUI 设计

### 5.1 页面结构

保留当前单页应用，只调整 Intake 区：

- 对话流：按 `messages` 渲染用户与模型消息，使用语义化列表和清晰的角色标签。
- 回答区：仅在存在 `session.question` 时显示；继续支持预设选项与“其他”文本输入。
- Brief 区：显示当前模型草案及 revision；保持只读。
- 操作区：`确认 Brief` 始终受既有 status、revision、content_hash 约束；取消、失败重试、使用最小 Brief 保持现状。

### 5.2 交互规则

1. 创建成功即渲染原始问题、首个模型质询和当前 Brief。
2. 回复成功后用服务端返回的完整 `messages` 重绘，不在浏览器本地猜测追加结果。
3. 新消息出现后滚动对话容器至末尾，但不抢走回答输入焦点。
4. `READY_TO_CONFIRM` 时若无新问题，消息流保留历史，回答区隐藏，确认操作可见。
5. `NEEDS_INPUT` 时用户仍可跳过回答、直接确认当前 Brief。
6. 刷新恢复暂不新增 GET Intake API；本计划不扩大现有页面生命周期。服务端重启回放能力保持，但浏览器刷新后恢复当前 Intake 属独立需求。
7. 所有动态文本继续通过 `textContent`/DOM 属性写入，禁止拼接未转义 HTML。

### 5.3 可访问性与响应式

- 对话区用 `role="log"`、`aria-live="polite"`、`aria-relevant="additions text"`。
- 用户和模型不只靠颜色区分，须有可见角色名。
- 选项仍使用原生 radio，文本回答仍有 `<label>`，错误状态保持可读。
- 移动端单列：对话、回答、Brief、操作依次排列；桌面可用两列，但不嵌套卡片。
- 对话区设稳定的 `max-height` 与滚动，不使长历史挤出确认操作。

## 6. 实施步骤

### P0：锁定现状

1. 记录当前 `IntakeResponse` JSON 字段及页面关键选择器。
2. 运行现有 Web/Intake 定向测试，确保基线为绿。
3. 增加失败测试：包含两轮问答的 session 必须投影为“原问、质询、回答、质询”的有序消息。

完成判据：新增测试先因缺少 `messages` 失败，既有测试无回归。

### P1：增加只读消息投影

1. 在 `src/web.rs` 定义最小 `IntakeMessage` DTO 和纯投影函数。
2. 将 `messages` 加入所有 Intake 响应，不改任何请求结构与路由。
3. 覆盖首问、两轮、pending 末问、旧多问事件回放的顺序测试。
4. 更新既有 API shape 断言，仅允许新增 `messages`。

完成判据：Web 定向测试通过；创建、回复、重试、最小 Brief 均返回一致消息史。

### P2：把 Intake 呈现改为对话流

1. 用一个消息列表替换“只显示当前质询”的视觉区域。
2. `renderSession` 先渲染 `session.messages`，再依据 `session.question` 渲染回答控件。
3. 保留既有 `post()`、`act()`、confirm/cancel/retry/minimal-brief 调用，不复制 API 状态机。
4. 将 Brief 标题明确为“当前 Brief”，并显示模型每轮更新后的最新值。
5. 补 `role=log`、角色标签、移动端布局和长文本换行。

完成判据：连续两轮后页面同时可见原问、两次模型质询、第一次用户回答及当前 Brief；所有既有按钮仍工作。

### P3：回归与浏览器验收

1. 运行格式、静态检查、全量测试及 release build。
2. 用真实服务完成：创建、回答两轮、直接确认、进入 Research。
3. 另验：首轮直接确认、第五问后待确认、失败重试、最小 Brief、取消。
4. 在桌面与移动视口截图检查：消息顺序、长文本、滚动、按钮无重叠。
5. 检查网络请求：回复体不含客户端提交的 Brief；确认仍携带当前 revision/hash。

完成判据：质量门全绿，浏览器流程可见且无控制台错误；部署前保留验收记录。

## 7. 测试矩阵

| 场景 | 预期 |
|---|---|
| 创建后模型提出一问 | 原始用户消息、模型质询、当前 Brief 同时可见 |
| 回答一问后模型继续问 | 旧问答保留，新质询追加，Brief 更新 |
| 模型不再提问 | 历史保留，回答区隐藏，可确认 |
| `NEEDS_INPUT` 直接确认 | 不必回答当前问题，进入既有 Research |
| 回答第五问 | 不产生第六问，历史完整，等待人工确认 |
| stale revision/hash | 仍由服务端拒绝，页面显示错误，不伪造成功消息 |
| 模型失败后重试 | 既有历史不丢，重试结果重绘 |
| 使用最小 Brief | 消息史保留，Brief 更新为服务端结果 |
| 旧多问 JSONL 回放 | 消息按问题顺序和 question_id 配对显示 |
| 恶意文本 | 作为纯文本显示，不执行 HTML/脚本 |
| 移动端长问答 | 可换行、可滚动、确认按钮不被遮挡 |

## 8. 质量门与命令

```bash
cargo fmt --all -- --check
cargo test --locked web::tests
cargo test --locked intake::tests
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked
git diff --check
```

浏览器验收必须使用实际 HTTP 服务，不以静态 HTML 截图代替。计划阶段不部署；实现验收全绿后，方可构建新镜像并切换现网。

## 9. 风险与回退

- **消息错配**：只按 `question_id` 配对，并以旧多问 fixture 锁定兼容行为。
- **API 兼容**：只新增响应字段，保留 `question` 及全部现有字段；旧客户端可忽略 `messages`。
- **XSS**：消息文本仅经安全 DOM API 写入；不得用 `innerHTML` 渲染模型或用户内容。
- **状态漂移**：前端每次以服务端完整响应重绘，不维护独立对话真相。
- **部署回退**：改动不触及持久化格式；若 UI 回归，直接切回旧镜像即可，数据无需回滚。

## 10. 完成定义

以下条件须同时满足：

1. 用户能在单一消息流中看到原始问题、模型历次质询与自己的历次回答。
2. 每次回答后当前 Brief 由模型更新并保持只读。
3. 用户可在任一有效草案上确认并进入原 Research 流程。
4. 五问上限、显式确认、revision/hash、失败恢复与幂等行为无变化。
5. 仅 `src/web.rs`、`src/web/index.html` 及其就地测试发生必要修改；若扩面，须在实现前说明原因。
6. 全部质量门与桌面/移动浏览器验收通过。

## 11. 推荐提交

最小改动宜合为一个提交：

```text
fix: present research intake as a conversation
```

提交内同时包含响应投影、UI 与测试，避免出现“API 已变但页面未消费”的中间版本。文档计划可先独立提交；部署另行执行。
