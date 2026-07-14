# Research Intake 单问质询改造计划

> 状态：已完成（2026-07-14）
>
> 依据：[`web-search-architecture.md`](./web-search-architecture.md)“Research Intake”章节
>
> 基线：`43dfda6`。现有 Intake 已可创建、修订、确认、取消、失败恢复及 JSONL 回放；本计划只修正其“每次至多一个质询、累计至多五个、任一草案均可确认”的契约。

## 1. 目标与边界

### 1.1 目标行为

```text
create(question)
  -> 模型生成完整 brief_draft
  -> question = null：READY_TO_CONFIRM
  -> question = one：NEEDS_INPUT

reply(revision, answer)
  -> 只回答当前待答 question
  -> 模型结合原问题、当前草案及完整问答史修订草案
  -> questions_asked < 5：可再返回零或一个 question
  -> questions_asked == 5：不得再问，保持 READY_TO_CONFIRM

confirm(revision, content_hash)
  -> NEEDS_INPUT 或 READY_TO_CONFIRM 均可确认
  -> 复用既有幂等确认协议，启动同一 run_id
```

每版草案均完整展示。模型认为问题清晰时可零质询，但不能自动确认。达到五次只停止质询，不自动启动 Research。

### 1.2 不做

- 不改 `ResearchSession`、Explore 轮数、搜索、抓取、快照、选源或作答链。
- 不新增模型、依赖、配置、数据库、聊天服务、worker 或跨进程锁。
- 不改 `ResearchBrief`、`ConfirmedResearchBrief`、content hash 或 trace schema。
- 不迁移或重写既有 Intake JSONL；旧日志必须继续可回放。
- 不让客户端创建或编辑 `brief_draft`。

`ponytail:` 继续使用 `src/intake.rs` 与 append-only JSONL；待需多人并发、会话检索或数据库事务时再迁移。

## 2. 已核实的代码现状

| 层 | 当前实现 | 与目标之差 |
|---|---|---|
| `src/intake.rs` 模型契约 | `IntakeModelOutput.questions: Vec<_>` | 应为 `question: Option<_>` |
| `src/intake.rs` 限额 | 每轮最多 3 问、最多 2 轮、总计 5 问 | 应为每次至多 1 问、累计展示 5 问 |
| `src/intake.rs` 会话 | 暴露全部 `questions`、`answers`、`clarification_rounds` | API 应暴露当前 `question` 与累计 `questions_asked` |
| `src/intake.rs` 回复 | `UserReplied.answers` 可批量回答当前所有待答问题 | HTTP 每次只答当前一个问题 |
| `src/intake.rs` 确认 | `confirmation_event` 仅接受 `READY_TO_CONFIRM` | `NEEDS_INPUT` 亦可手动确认当前草案 |
| `src/backend.rs` | Prompt 要求 `questions` 数组、每轮最多 3 个 | 应要求 `question` 为对象或 `null`，每次最多一个 |
| `src/app.rs` | `reply_intake(..., answers, edited_brief)` | 应收单个 `answer`；客户端不得提交编辑后 Brief |
| `src/web.rs` | reply JSON 为 `{revision, answers, edited_brief}` | 应为 `{revision, answer}`，未知字段仍拒绝 |
| `src/web/index.html` | 同页渲染多个历史问题，可提交多个答案并编辑 Brief | 应只显示当前问题；草案只读；任一有效草案均显示确认按钮 |
| 内嵌测试 | 覆盖多问、两轮、批答和仅 READY 确认 | fixtures 与断言须换成单问五次契约 |

既有正确能力应保留：`revision + content_hash` 防陈旧确认、每会话进程内锁、两次模型 JSON 尝试、`INTAKE_FAILED`、取消、幂等 `run_id`、崩溃恢复、确认前无 Research 副作用。

## 3. 契约决策

### 3.1 模型输出

```json
{
  "brief_draft": { "...": "完整 ResearchBrief" },
  "question": {
    "id": "stable_id",
    "question": "one material clarification",
    "options": []
  },
  "ready_to_confirm": false
}
```

- `question` 允许为 `null`，不再接受 `questions`。
- `ready_to_confirm=true` 时 `question` 必须为 `null`。
- `ready_to_confirm=false` 且尚有额度时，允许一个问题；不得返回数组或额外字段。
- 服务端而非模型执行五问上限。已展示五问后丢弃模型新问题并令状态为 `READY_TO_CONFIRM`。
- `original_question` 仍逐字一致；Brief 仍规范化并重算 hash。

### 3.2 HTTP 表面

```text
POST /api/research/intakes
  {question}

POST /api/research/intakes/{id}/reply
  {revision, answer}

POST /api/research/intakes/{id}/confirm
  {revision, content_hash, rounds?}

POST /api/research/intakes/{id}/cancel
  {revision}
```

Intake 会话响应至少保留：

```json
{
  "clarification_id": "...",
  "revision": 2,
  "status": "NEEDS_INPUT",
  "original_question": "...",
  "brief_draft": {},
  "content_hash": "...",
  "question": {},
  "questions_asked": 2,
  "failure": null,
  "confirmation": null
}
```

决策如下：

1. reply 的 `answer` 为单个非空字符串；服务端自动绑定当前待答问题 ID，客户端不得自报 `question_id`。
2. `question` 只返回当前未答问题；已答历史仍保存在 JSONL/reducer 内，不扩张公共响应。
3. `questions_asked` 指已展示问题总数，而非模型调用轮数或已回答数。
4. `edited_brief` 从 HTTP 请求删除；前端 Brief 改只读。既有“生成最小 Brief”仍由服务端 `minimal_brief_event` 产生，不接受客户端伪造草案。
5. `rounds` 属 Research policy，暂保留现有可选字段；此项不属于 Intake 质询次数。

### 3.3 JSONL 兼容

不升 `INTAKE_EVENT_SCHEMA_VERSION`，不改既有事件 JSON 形状：

- `clarification_asked.questions` 与 `user_replied.answers` 继续使用数组，以便回放旧日志；新 writer 强制数组长度恰为 1。
- reducer 继续接受旧日志中的 1–3 个问题及批量答案；新命令路径永不再生成批量事件。
- 会话内部保留完整 `questions`、`answers` 与 pending ID，另以序列化视图输出单个当前问题和 `questions_asked`。
- 旧日志若同时有多个 pending 问题，按原规则完成回放；新单答 API 对该遗留状态应返回明确 `409 invalid_transition`，不得猜测回答对象。运维可用旧版本完成该会话，或取消后新建。

此法避免破坏已落盘审计记录；若实现发现 serde 无法在不污染内部模型的情况下稳定输出新视图，再引入专用 `PublicIntakeSession`，而非改事件 schema。

## 4. 文件影响范围

### 必改生产文件

| 文件 | 最小改动 |
|---|---|
| `src/intake.rs` | 单问题模型输出；五问计数；单答事件构造；`NEEDS_INPUT` 可确认；兼容旧事件回放；更新单元测试 |
| `src/backend.rs` | 更新 `INTAKE_PROMPT` JSON 示例、单问规则与剩余额度约束；补 Prompt 断言 |
| `src/app.rs` | `reply_intake` 改收单个答案；删除客户端 `edited_brief` 路径；保持失败重试、最小 Brief、确认恢复协议；更新服务测试 |
| `src/web.rs` | reply DTO 改为 `answer: String`；映射新服务命令；更新 HTTP 测试及 400/409 断言 |
| `src/web/index.html` | 单问题控件、单答提交、只读 Brief、问数显示、NEEDS_INPUT 确认；删除批答和客户端 Brief 编辑逻辑 |

### 视编译结果决定

| 文件 | 触发条件 |
|---|---|
| `src/lib.rs` | 仅当公共 re-export 因删除/改名 `ClarificationAnswer` 或新增公共响应视图而需同步 |
| `docs/web-search-architecture.md` | 实现发现契约不可行或原文歧义时先停工回修设计；正常实现不改 |

### 明确不改

`src/orchestration.rs`、`src/trace.rs`、`src/types.rs`、`src/store.rs`、`src/crawl.rs`、`src/search.rs`、部署文件及数据目录。

预计改面：5 个生产文件，另 0–1 个 re-export 文件；无新依赖、无数据重写。

## 5. 原子实施步骤

### P0：锁定基线与 fixtures

**依赖：** 无。

1. 记录 `git status --short`、`git rev-parse HEAD`。
2. 运行现有 Intake、App、Web 定向测试，保存真实测试数，不沿用旧文档数字。
3. 先增加失败测试：模型数组输出被拒、每次单问、第五问封顶、NEEDS_INPUT 可确认、单答 API、客户端不能提交 `edited_brief`。

**完成判据：** 新测试以预期原因失败；旧测试仍能区分回放兼容与新写入契约。

**回滚：** 只删新增测试。

### P1：收紧纯状态机

**依赖：** P0。

1. 将 `IntakeModelOutput.questions` 改为 `question: Option<ClarificationQuestion>`。
2. 删除“每轮 3 问/最多 2 轮”控制；只保留 `MAX_TOTAL_QUESTIONS = 5`。
3. 以已追加的 `clarification_asked` 问题总数计算 `questions_asked`；每次新事件至多含一个问题。
4. 单答仅匹配唯一 pending ID；空答案、无 pending、多个遗留 pending、错误状态均机械拒绝。
5. 第五问已展示后，后续回复仍调用模型修订 Brief，但不再接受新问题，并转 `READY_TO_CONFIRM`。
6. 放宽确认前态为 `NEEDS_INPUT | READY_TO_CONFIRM`；仍校验 revision、hash 与 Brief 存在。
7. 保持旧 `ClarificationAsked.questions`、`UserReplied.answers` reducer 可回放。

**完成判据：** `cargo test --locked intake::tests` 全绿；包含 0、1、5、6 问边界，旧多问 JSONL fixture 可恢复。

**风险门：** 若必须更改已落盘事件字段方能实现，停止 P1，不得静默升 schema。

**回滚：** 回退 `src/intake.rs`；无数据副作用。

### P2：更新 Prompt 与服务编排

**依赖：** P1。

1. `INTAKE_PROMPT` 改为 `question: object|null`，明示一次至多一问。
2. `advance_intake` 输入继续传原问题、当前草案、完整问答史，并显式传剩余额度；模型不得自行决定突破五问。
3. `reply_intake` 仅接一个 answer，由服务绑定当前问题并追加单元素 `UserReplied.answers`。
4. 删除 HTTP 来源的 `edited_brief`；保留服务端生成最小 Brief 的纯函数及 `INTAKE_FAILED` 重试路径。
5. 确认与 `prepare_confirmed_run` 不重写，只验证新增的 NEEDS_INPUT 路径仍复用原幂等协议。

**完成判据：** Prompt shape 测试、App create/reply/confirm/cancel/recovery 测试全绿；确认前仍无 trace 和 snapshot。

**回滚：** 同时回退 `src/backend.rs`、`src/app.rs`，恢复与 P1 之前一致的契约。

### P3：切换 HTTP API

**依赖：** P2。

1. `ReplyIntakeRequest` 改为 `{revision, answer}`，保留 `deny_unknown_fields`。
2. 空白 answer 返回 `400 invalid_request`；陈旧 revision 返回 `409 stale_brief`；状态冲突返回 `409 invalid_transition`。
3. 响应改为当前 `question` 与 `questions_asked`；不得泄露内部 pending ID 列表之外的信息。
4. 验证 NEEDS_INPUT 确认返回 `202 + run_id`，重复确认仍返回同一 run_id 或既有幂等语义。

**完成判据：** Web handler 测试覆盖创建、逐答、提前确认、第五问、取消、失败、未知字段与 stale 请求。

**回滚：** API 与 App 必须成对回退，禁止只回退一层造成半切换。

### P4：切换 WebUI

**依赖：** P3。

1. 每次只渲染 `session.question`；选项外保留可访问的自定义文本输入。
2. 提交一个 answer 后禁用按钮直至响应，避免重复 reply。
3. `questions_asked / 5` 取代 `clarification_rounds`。
4. Brief 全字段只读展示；移除组装并回传 `edited_brief` 的 JS。
5. `NEEDS_INPUT` 与 `READY_TO_CONFIRM` 均显示确认；确认仍发送当前 revision/hash。
6. `INTAKE_FAILED` 保留重试、服务端最小 Brief、取消入口；不把失败静默降级为研究。
7. 保持键盘操作、label 关联、focus、错误提示与窄屏布局。

**完成判据：** 浏览器手测清晰问题、两问后提前确认、五问封顶、取消、失败恢复五条流程；网络面板中 reply 仅含 revision/answer。

**回滚：** UI 与 API 使用同一提交或同一部署批次；失败则整体回退镜像。

### P5：全量回归与部署门

**依赖：** P4。

依次执行：

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo build --release --locked
git diff --check
git status --short
```

另以临时 `TRACEABLE_SEARCH_DATA_DIR` 做进程级黑盒检查：

1. create 后仅有 Intake JSONL，无 trace；
2. 每次 reply 只追加一组单问/单答事件；
3. 第五问后为 `READY_TO_CONFIRM` 且无第六问；
4. 第一问出现时直接 confirm 可启动；
5. stale revision/hash 被拒；
6. 重启后会话回放一致；
7. confirm 后仅一个 `run_id`、一个 trace header；
8. 旧多问 fixture 仍可读取且不会被新 API 错配回答。

**完成判据：** 命令全为 0；黑盒八项全过；Git 差异仅含计划内文件且无凭据。

**回滚：** 发布前不改生产数据；发布后保留 Intake JSONL、trace 与 snapshots，回退上一镜像并暂停创建新 Intake，不删除审计记录。

## 6. 测试矩阵

| 场景 | 关键断言 |
|---|---|
| 清晰问题 | `question=null`、`READY_TO_CONFIRM`、仍需人工确认 |
| 首轮质询 | 仅一个 `question`、`questions_asked=1` |
| 连续质询 | 每答一次最多新增一问，Brief revision 单调递增 |
| 第五问 | `questions_asked=5`；答后无第六问且 READY |
| 模型越界 | 数组/额外字段/第六问不能进入新日志 |
| 提前确认 | NEEDS_INPUT 的当前 revision/hash 可确认 |
| 陈旧确认 | 旧 revision 或 hash 为 409，不启动 Research |
| 单答边界 | 空答、无 pending、多 pending 遗留状态均明确拒绝 |
| 坏 JSON | 一次纠错；第二次失败写 `intake_failed` |
| 失败恢复 | 重试或服务端最小 Brief 后仍须预览确认 |
| 取消 | 任一非终态可取消，不启动 Research |
| 重启回放 | 当前问题、问数、Brief、revision/hash 一致 |
| 旧日志 | 旧批问批答事件可读；不改写原文件 |
| 幂等确认 | 崩溃窗口与重复确认不产生第二 run_id |
| UI | 单问题、只读 Brief、键盘可用、重复提交受抑制 |

## 7. 风险、依赖与回滚

| 风险 | 防护 | 回滚 |
|---|---|---|
| 新 API 无法回答旧日志的多个 pending 问题 | 回放兼容但命令拒绝歧义；取消重建或旧版本处理 | 回退旧镜像，日志不动 |
| `questions_asked` 被误算为回复数 | 从持久化 `clarification_asked.questions.len()` 求和 | 回退派生视图，不改事件 |
| 第五答后模型仍返回问题 | 服务端硬截断；边界测试 | 回退状态机提交 |
| NEEDS_INPUT 提前确认绕过一致性 | 仍强制当前 revision/hash 与完整 Brief | 禁用 UI 提前确认并回退对应提交 |
| UI/API 半切换 | 同批部署、HTTP contract 测试 | 整体回退镜像 |
| 删除客户端编辑影响失败恢复 | 保留服务端 `minimal_brief_event` 与显式重试 | 临时恢复旧 UI/API，不接受未校验草案 |
| 日志 schema 被误改 | P1 风险门、旧 fixture、禁止原地重写 | 停工；恢复代码，保留日志 |

外部依赖仅为现有 strong model；模型输出不可信，所有数量、状态、revision/hash 约束均由 Rust 执行。

## 8. 推荐提交顺序

1. `test: lock single-question intake contract`
2. `intake: ask one question up to five times`
3. `app: accept one clarification answer`
4. `web: expose single-question intake api`
5. `ui: render one intake question at a time`
6. `docs: record intake migration and verification`

每个提交须可编译、定向测试通过、无真实数据与凭据。部署只在 P5 全绿后进行。
