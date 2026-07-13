# Intake 与 Confirmed Research Brief 实施计划

> 状态：待实施
>
> 依据：[`web-search-architecture.md`](./web-search-architecture.md)
>
> 目标：在现有 Rust 研究主链前增加可恢复、可审计的 Intake；只有用户显式确认的不可变 `ConfirmedResearchBrief` 才能启动研究。

## 1. 实施边界

### 1.1 要实现的目标态

```text
Browser
  │ question / reply / edit / confirm / cancel
  ▼
Intake state machine
  ├── data/intake/<clarification_id>.jsonl
  └── ConfirmedResearchBrief + policy + preallocated run_id
                 │
                 ▼
Existing ResearchSession
  ├── plan(complete brief, archived snapshots)
  ├── select(complete brief, excerpts)
  ├── synthesize(complete brief, selected snapshot bodies)
  ├── data/snapshots.sqlite
  └── data/traces/<run_id>.jsonl
```

Intake 只澄清会改变查询方向、来源选择或答案形态的歧义；不查询事实，不替用户补造限制。清晰问题可零追问，但仍须预览并显式确认。确认前不分配研究资源、不建快照、不写研究 trace。

`ponytail:` 架构文档中的逻辑路径 `trace/<run_id>.jsonl` 继续映射到现有磁盘目录 `data/traces/`；不为目录单复数做无收益迁移。若未来统一数据布局，再以只读兼容旧目录的迁移器处理。

### 1.2 本次不做

- 不建聊天服务、消息 DB、通用工作流引擎、worker 池或分布式锁。
- 不增加第二抓取后端，不改变 Bing/SearXNG、crawl4ai、SSRF 与快照策略。
- 不重写已完成的 3–5 轮 Explore、全候选归档、确定性摘录、目录选源与最终作答。
- 不迁移 `snapshots.sqlite`，不新增 `store.sqlite`。
- 不增加模型或数据目录配置项；Intake 复用现有 strong model 与 `TRACEABLE_SEARCH_DATA_DIR`。
- 不在本计划阶段修改生产代码、部署或真实凭据。

## 2. 当前基线与差距

### 2.1 已有能力

仓库当前已经具备：

- Rust `ResearchSession` 的固定 3–5 轮探索与资源上限；
- 每词 10 条搜索结果、去重后归档、不可变网页快照与内容哈希；
- strong model 的查询规划、目录选源、基于选中原文作答三阶段；
- `snapshot_ref` 归属、哈希及 Claim 引用范围校验；
- `snapshots.sqlite` 与每 run 一份 append-only JSONL trace；
- localhost Axum API、SSE 与单页 WebUI；
- `error_class + stage + message` 的研究错误响应；
- SSRF、防提示注入、轮数、快照数与输入预算边界。

实施前基线：

```text
cargo test --locked                                      # 43 passed
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

三项均已通过。后续阶段不得以重写既有研究算法代替 Intake 接入。

### 2.2 核心差距

1. `POST /api/research` 仍可用裸 `question` 直接启动研究。
2. 尚无 `ResearchBrief`、确认哈希、revision 与 Intake 状态机。
3. 尚无 `data/intake/<clarification_id>.jsonl` 及事件回放。
4. `RunHeader` 仍为 v2，只保存原问题，未嵌入冻结 Brief 与 `clarification_id`；实施时须把 `TRACE_SCHEMA_VERSION` 升至 3。
5. `ResearchSession` 和三次 strong 调用仍以字符串 `question` 为主输入。
6. WebUI 没有追问、编辑草案、确认、取消及失败恢复界面。

## 3. 影响范围与文件级影响矩阵

### 3.1 新增

| 文件 | 改动 | 理由 |
|---|---|---|
| `src/intake.rs` | Brief 草案校验、状态转移、模型输出解析、事件定义、append-only 写入与回放、确认幂等协议 | Intake 应有唯一阶段控制点；避免散落于 HTTP 与研究编排 |

### 3.2 修改

| 文件 | 改动 | 保留内容 |
|---|---|---|
| `src/types.rs` | 增加 `ResearchScope`、`ResearchBrief`、`ConfirmedResearchBrief` 与确定性哈希 | 保留快照、查询、Claim 等现有类型及 ID 规则 |
| `src/trace.rs` | writer 升为 v3；`RunHeader` 嵌入 Brief、hash、`clarification_id`；reader 保留 v1/v2 | 保留现有事件、逐行 JSONL 与 `create_new` |
| `src/backend.rs` | 增加 Intake prompt/调用；plan/select/synthesize 请求均携完整 Brief | 保留三个研究 prompt 的职责与网页内容不可信约束 |
| `src/orchestration.rs` | `ResearchSession` 只接受 `ConfirmedResearchBrief`；三阶段传递完整 Brief | 保留轮次、归档、摘录、选源、作答和现有纯校验函数 |
| `src/app.rs` | 编排 start/reply/confirm/cancel；确认后按同一 `run_id` 建 trace 并启动研究 | 保留适配器组装、公开答案映射与数据目录配置 |
| `src/web.rs` | 增加四个 Intake 写端点及 400/404/409 映射；删除最终的裸问题启动入口 | 保留 run 状态查询与 SSE |
| `src/lib.rs` | 导出 Intake 模块及必要领域类型 | 保留平坦 crate 公共面 |
| `src/web/index.html` | 改为“输入—澄清/编辑—确认—研究—结果”界面 | 保留单文件、无前端构建链与现有研究进度展示 |
| `README.md` | 更新数据流、API、持久化目录、操作步骤与恢复语义 | 保留构建、外部依赖和部署说明 |
| `.env.example` | 注明数据目录新增 `intake/`，且不需要新凭据 | 保留现有变量和值模板 |

### 3.3 原则不改

| 文件 | 原因 |
|---|---|
| `Cargo.toml`、`Cargo.lock` | `serde`、`serde_json`、`sha2`、`hex`、`chrono`、Tokio 与 stdlib 已覆盖全部需求 |
| `src/adapters.rs` | 外部 HTTP、SSRF 与抓取协议不变 |
| `src/snapshot.rs` | 快照 schema、内容寻址与读取接口不变 |
| `src/error.rs` | Intake 自身冲突/输入错误可定义于 `intake.rs` 并由 Web 层映射；研究错误分类不变 |
| `src/main.rs` | 路由仍由 `router(ResearchService)` 组装；按命令惰性回放 Intake，无启动扫描要求 |
| `Containerfile` | 二进制、端口、卷与运行用户不变 |
| `docs/web-search-architecture.md` | 它是本计划的冻结契约，不在实施中顺手改设计 |

若实现发现必须越出此矩阵，应先记录具体编译或契约证据，再更新本计划；不得预先增加依赖或抽象。

## 4. 冻结的数据契约

### 4.1 Brief

`src/types.rs` 增加以下等价 Rust 结构；字段序列化名与架构 JSON 一致：

```text
ResearchScope {
  time_range: Option<String>,
  geography: Option<String>,
  include: Vec<String>,
  exclude: Vec<String>,
}

ResearchBrief {
  schema_version: u32,              // 首版固定为 1
  original_question: String,        // 初始输入，任何修订不得改写
  research_question: String,
  desired_output: Option<String>,
  scope: ResearchScope,
  source_constraints: Vec<String>,
  accepted_assumptions: Vec<String>,
}

ConfirmedResearchBrief {
  brief: ResearchBrief,
  clarification_id: String,
  content_hash: String,
  confirmed_at: DateTime<Utc>,
}
```

哈希只覆盖规范化后的 `ResearchBrief`，不覆盖 `clarification_id`、`confirmed_at`、revision 或 `run_id`。结构体字段固定顺序，以 `serde_json::to_vec` 生成紧凑 UTF-8 JSON，再复用 SHA-256/hex；禁止用 `HashMap` 承载 Brief 字段。增加 known-answer test，防止重构后哈希漂移。

信任边界校验置于 Intake：输入 trim 后不得为空；`original_question` 不得变化；字符串与数组数量有界；空可选项保持 `None`/空数组，不触发追问；用户编辑后的结构再次执行同一校验。首版上限集中为常量：问题 10,000 字符、其余单字符串 4,000 字符、每个数组 32 项、数组单项 2,000 字符、单次回答 4,000 字符。

### 4.2 Intake 状态

状态仅为：

```text
DRAFT
NEEDS_INPUT
READY_TO_CONFIRM
INTAKE_FAILED
CONFIRMED
CANCELLED
```

硬约束：

- `MAX_QUESTIONS_PER_ROUND = 3`；`MAX_CLARIFICATION_ROUNDS = 2`；`MAX_TOTAL_QUESTIONS = 5`，三者共同构成轮次上限。
- 清晰问题由 `DRAFT` 直达 `READY_TO_CONFIRM`，但不得直接研究。
- `NEEDS_INPUT`、`READY_TO_CONFIRM`、`INTAKE_FAILED` 均可取消。
- 达上限后只允许按当前理解确认、继续手工编辑或取消，不再调用模型追问。
- 每次 Brief 修订递增 revision；确认必须匹配当前 `revision + content_hash`。
- `CONFIRMED` 与 `CANCELLED` 是互斥、不可逆终态；`INTAKE_FAILED` 可重试，不是终态。
- 模型调用失败或连续两次结构化输出不合法时进入 `INTAKE_FAILED`；不得静默用原问题启动研究。
- “按原问题生成最小 Brief”是显式恢复命令，生成后仍进入预览/确认。

### 4.3 Intake 事件

每会话一份 `data/intake/<clarification_id>.jsonl`，只追加下列事件：

```text
intake_started
clarification_asked
user_replied
brief_revised
confirmed
cancelled
intake_failed
```

`intake_started` 保存原问题与已校验 policy；`brief_revised` 保存 revision、完整 Brief 与 hash；`confirmed` 保存 `run_id`、完整 `ConfirmedResearchBrief` 与 policy。每行含事件 schema version 与时间戳。回放以事件重新构造状态，不另写可变 session 文件。

事件写入须校验 `clarification_id` 为单一文件名组件，使用 `create_new` 创建首文件、append 写后 `flush + sync_data`。日志不记录 API key、Authorization header 或上游完整错误响应；失败事件只保留有界错误摘要。

### 4.4 HTTP

最终仅有四个 Intake 写端点：

```text
POST /api/research/intakes
  {question, policy?}

POST /api/research/intakes/{clarification_id}/reply
  {revision, answers, edited_brief?}

POST /api/research/intakes/{clarification_id}/confirm
  {revision, content_hash}

POST /api/research/intakes/{clarification_id}/cancel
  {revision}
```

`policy.rounds` 缺省为 3，并复用现有 3–5 校验；输入预算与快照上限由服务端现有常量固定，不接受客户端扩大。

状态码统一：非法/空输入为 400；未知会话或 run 为 404；旧 revision/hash、已终止会话或并发修改为 409；模型或持久化失败沿现有 JSON 错误形态返回 `error_class + stage + message`。错误响应不得返回 prompt、网页正文或凭据。

确认成功返回既有或新建的 `{run_id}`。现有 `GET /api/research/{run_id}` 与 SSE 端点不变。

## 5. 分阶段实施

每阶段先完成本阶段测试，再进入下一阶段；任一阶段失败只回退该阶段文件，不带病切换 WebUI。

### 阶段 0：锁定基线与 fixture

**文件：** 不改生产文件；准备模块内测试数据。

1. 重新运行三项基线命令并记录测试数。
2. 从架构文档固化一份明确问题、一份歧义问题、一个最小 Brief、一个 v2 trace header fixture。
3. 记录当前公开 API、SSE 事件与 `data/traces/` 路径，作为兼容比较基准。

**依赖：** 当前仓库可构建基线与已冻结的架构文档；无前置实施阶段。

**完成判据：** 基线全绿；fixture 不含网络、时间随机值或真实凭据。

### 阶段 1：领域类型与确定性哈希

**文件：** `src/types.rs`、`src/lib.rs`。

1. 增加 Scope、Brief 与 Confirmed Brief；字段只用结构体、`Vec`、`Option`。
2. 增加 Brief 规范化、边界校验及 SHA-256 hash；禁止确认后可变 setter。
3. 导出研究编排真正需要的类型，不导出存储内部结构。
4. 增加 round-trip、空约束、原问题不可改、超限拒绝及 known-answer hash 测试。

**依赖：** 仅现有 `serde`、`serde_json`、`sha2`、`hex`、`chrono`。

**完成判据：** 同一 Brief 在重复序列化与 round-trip 后 hash 相同；任何字段变化均改变 hash；非法输入在模型或磁盘调用前被拒绝。

### 阶段 2：Intake 纯状态机与 append-only 日志

**文件：** 新增 `src/intake.rs`；修改 `src/lib.rs`。

1. 定义 `IntakeStatus`、问题/回答 DTO、事件枚举与由事件归约出的 `IntakeSession`。
2. 把状态转移写成无 I/O 的纯函数；模型只能提出草案和问题，不能直接改状态、分配 run 或触网。
3. 实现单文件 writer/replay；逐事件验证 schema、revision、hash、终态互斥和文件名安全。
4. 实现模型输出固定 schema 校验；坏 JSON 只纠正重试一次，第二次失败追加 `intake_failed`。
5. 实现最小 Brief 恢复：`research_question = original_question`，其余约束为空，仍生成新 revision/hash 并等待确认。
6. 对同一会话的命令用现有 Tokio 锁串行化；不增加跨进程锁。

**七条必测 fixture：**

1. 明确问题零追问但进入 `READY_TO_CONFIRM`；
2. 歧义问题进入 `NEEDS_INPUT`；
3. 达 2 轮或 5 问上限后停止追问；
4. `NEEDS_INPUT`、`READY_TO_CONFIRM`、`INTAKE_FAILED` 均可取消；
5. 旧 revision 或 hash 确认得到 stale 冲突；
6. 模型连续两次坏 JSON 后进入可恢复 `INTAKE_FAILED`；
7. `confirmed` 已落盘而 trace 尚未创建时，恢复仍使用同一 `run_id`。

另测截断末行、未知事件版本、终态后写命令与路径穿越。截断末行不得被悄悄当作完整事件；返回可定位的持久化错误。

**依赖：** 阶段 1 的领域类型、规范化与确定性哈希；复用现有 Tokio、Serde 与文件设施。

**完成判据：** 七条 fixture 与负测均通过；重放结果和在线状态一致；尚未修改生产入口。

### 阶段 3：确认握手与 trace v3

**文件：** `src/trace.rs`、`src/app.rs`。

1. 预分配 `run_id`，先同步追加 `confirmed`，再以 `create_new` 建研究 trace。
2. v3 `run_header` 嵌入 schema version、`run_id`、`clarification_id`、完整 `ConfirmedResearchBrief` 与 policy。
3. 保留研究事件枚举；`query` 同行记录 `gap`，`snapshot_selection` 每项固定为 `snapshot_ref + reason` 且不含 `relevance`，`run_failed` 固定含 `error_class + stage + message`，其中 `error_class` 仅为 `external | internal`，`stage` 仅为 `setup | planning | search | archive | selection | synthesis | trace`。
4. 重复确认先回放 Intake：若已 confirmed，返回首次 `run_id`；若对应 trace 缺失，补建同一文件；不得生成第二个 ID。
5. writer 只写 v3；reader 通过 version 分派，保留 v1/v2 只读回放。旧 header 中只有 question 时不得伪造为已确认 Brief。
6. 每个 run 仍以 `answer` 或 `run_failed` 唯一终止；确认前错误只进 Intake 日志。

**依赖：** 阶段 1–2；仅 stdlib `OpenOptions::create_new` 与现有 trace 设施。

**完成判据：** 故障注入覆盖“confirmed 后、create trace 前”崩溃窗口；恢复得到相同 `run_id` 且仅一份 trace；v1/v2 fixture 可读，v3 可完整回放 Brief。

### 阶段 4：冻结 Brief 贯穿 ResearchSession

**文件：** `src/backend.rs`、`src/orchestration.rs`、`src/app.rs`。

1. `ResearchSession` 构造参数从 `question: String` 改为 `ConfirmedResearchBrief`；裸问题构造不再公开。
2. 查询规划每轮传完整 Brief、round、previous queries 与已有快照原文；第 1 轮原文为空。
3. 目录选源传完整 Brief 与全部标题/确定性摘录；最终作答传完整 Brief 与已校验选中原文。
4. `backend.rs` 增加 Intake 固定 prompt；所有 prompt 把用户输入、澄清回答和网页内容声明为不可信数据。
5. 保留现有 `plan_queries`、`select_sources`、`synthesize_answer` 纯解析/校验边界；不重写搜索、抓取、快照或 Claim 算法。
6. 使用 fake backend 捕获三阶段请求，断言每次收到与 run header hash 一致的完整 Brief，而非仅 `research_question`。

**依赖：** trace v3 已可提供冻结 Brief。

**完成判据：** 编译期已无生产研究路径接受裸 `&str question`；三阶段传播测试通过；现有 Explore/Synthesize 回归测试全绿。

### 阶段 5：服务与四个 Intake API

**文件：** `src/app.rs`、`src/web.rs`、`src/lib.rs`。

1. 在 `ResearchService` 增加 start/reply/confirm/cancel 命令；每次命令按 `clarification_id` 惰性回放磁盘事件，故重启后仍可继续。
2. start 校验输入和 policy，创建 Intake 文件，再调用 strong 生成首版草案。
3. reply 校验当前 revision，记录回答或用户编辑，生成下一 revision；达到上限时禁止继续模型追问。
4. confirm 执行阶段 3 协议，随后才启动后台 `ResearchSession`；cancel 只追加终态事件，不创建 run。
5. 接入四个 POST 路由及 JSON DTO；保持 GET run 状态与 SSE 路由。
6. 同一进程仍只运行现有受控研究任务；已有任务运行时按既有策略拒绝或排队，不引入 worker 池。
7. 新路径测试通过后删除公开 `POST /api/research` 裸问题入口；若保留内部 helper，须为 `pub(crate)` 且只接受 Confirmed Brief。

**接口测试：** 覆盖 400 空问题、404 未知 ID、409 stale hash、重复 confirm 同 ID、cancel 后 confirm 冲突、失败响应字段、确认前无 trace/快照副作用。

**依赖：** 阶段 2–4 的 Intake 状态机、确认握手与冻结 Brief 研究入口。

**完成判据：** 只有 confirm 能产生 `run_id`；四个写端点与状态码符合 §4.4；源码中不存在从 HTTP 裸问题直启研究的路径。

### 阶段 6：WebUI 切换与文档同步

**文件：** `src/web/index.html`、`README.md`、`.env.example`。

1. WebUI 首屏只创建 Intake，不再调用旧 `POST /api/research`。
2. `NEEDS_INPUT` 显示问题、互斥选项、“其他/不限制”与可编辑 Brief；所有控件有 `<label>`、键盘可达和清晰焦点。
3. `READY_TO_CONFIRM` 展示完整 Brief、空约束、assumptions、revision/hash；确认按钮只发送当前版本。
4. `INTAKE_FAILED` 显示重试、生成最小 Brief、取消；不得自动开始研究。
5. 确认成功后复用现有 SSE、进度计数、结果与来源渲染。请求期间禁用重复提交；409 后展示版本过期并保留用户输入。
6. README 更新架构图、四端点顺序、`data/intake/` 与 `data/traces/`、恢复/取消语义；`.env.example` 仅更新数据目录注释，不增加变量。

**依赖：** 阶段 5 的四个 Intake API 已稳定，现有 SSE 与结果渲染保持可复用。

**完成判据：** 键盘可完成创建、回答、编辑、确认、取消；清晰问题也必须停在确认页；WebUI 源码不再引用旧写端点。

### 阶段 7：[VERIFY] 全量验证与迁移门

**依赖：** 阶段 0–6 全部完成；旧入口尚未删除，便于失败时回滚。

**自动验证：**

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
git diff --check
```

**离线验收：**

1. 重放旧 v1/v2 trace，确认读取兼容且不改原文件。
2. 跑七条 Intake fixture、哈希 known-answer、API 状态码及三阶段 Brief 传播测试。
3. 断言确认前 `snapshots.sqlite` 无本会话写入且 `data/traces/<run_id>.jsonl` 不存在。
4. 在 confirmed 事件后注入失败，再次 confirm；断言同一 `run_id`、一个 trace、一次研究启动。
5. 检查所有终态：Intake 仅 confirmed/cancelled；run 仅 answer/run_failed。

**本地 E2E：**

1. 明确问题：创建后直接预览，确认，再观察 SSE 至 answer/run_failed。
2. 歧义问题：至少一次 reply，编辑 Brief，旧 hash 确认应 409，新 hash 成功。
3. 失败恢复：模拟连续坏 JSON，选择最小 Brief，预览确认。
4. 取消：三个可取消状态各执行一次，确认无 run。
5. 重启：创建待确认会话，重启进程，以原 `clarification_id` reply/confirm 并成功回放。

**迁移门：** 仅当自动验证与五组 E2E 全通过，才删除旧公开写入口并部署。测试失败时不得通过保留隐藏的裸问题直启路径绕过。

## 6. 完成定义

实现完成须同时满足：

- 所有研究均可追溯到一个 confirmed Intake；
- `original_question` 逐字保留，完整 Brief 在确认后不可变；
- plan/select/synthesize 三阶段均重放同一完整 Brief；
- revision/hash 能机械拒绝旧草案；
- 重复确认及指定崩溃窗口只产生同一 `run_id`；
- Intake 与 Research 各有独立 append-only 日志，确认前无研究副作用；
- clear、ambiguous、limit、cancel、stale、bad JSON、crash recovery 七条 fixture 全过；
- v1/v2 trace 可读，v3 header 可单文件回放研究输入；
- 400/404/409 与 `error_class + stage + message` 一致；
- WebUI 可访问、可键盘操作且不绕过确认；
- 无新依赖、无新配置、无凭据入库；
- fmt、Clippy、全测与 `git diff --check` 全绿。

## 7. 风险与回滚

| 风险 | 防护 | 回滚 |
|---|---|---|
| Brief 序列化变化导致 hash 漂移 | 固定结构字段顺序、无 map、known-answer test | 停止新确认；保留旧日志，按 schema version 读取 |
| confirmed 与 trace 跨文件崩溃 | 先同步 confirmed，再 `create_new` trace；同 ID 补建 | 重试 confirm/recovery；绝不删除已落盘事件 |
| 重复请求启动两个研究 | 会话锁、终态校验、预分配 ID、trace 独占创建 | 保留首次 run，第二次返回 409 或原 ID |
| Intake 模型坏 JSON 或臆造约束 | 固定 schema、一次纠错重试、程序边界、显式确认 | 进入 `INTAKE_FAILED`，由用户重试/最小 Brief/取消 |
| 旧 trace 因 v3 失读 | version reader 与 v1/v2 fixture | writer 回退前保留 v3 文件；不做破坏性转换 |
| UI 与 API 半切换 | 先并存新 API，E2E 后才删旧 POST | 回退 UI；旧入口仅在迁移阶段短暂保留 |
| 日志泄露或无限增长 | 不记凭据/正文型错误；字段、轮次、问题数均有界 | 停止 Intake 写入并归档文件；快照策略不受影响 |
| 改动误伤成熟 Research 主链 | 只改输入类型与 prompt payload，保留纯函数和现有测试 | 按阶段回退 backend/orchestration 接线，不回滚数据 |

回滚原则：代码可按阶段回退，append-only Intake、v3 trace 与快照一律保留，不通过删日志“恢复”。旧二进制不会消费新 Intake；回滚期间暂停创建新会话，待兼容版本恢复后继续。

## 8. 推荐提交顺序

1. `types: add immutable research brief and stable hash`
2. `intake: add bounded state machine and jsonl replay`
3. `trace: persist confirmed brief in v3 run headers`
4. `research: require confirmed brief across model stages`
5. `web: add intake commands and confirmation gate`
6. `ui: switch research flow to intake confirmation`
7. `docs: document intake operation and recovery`

每个提交均应可编译、测试通过且不含真实凭据；禁止把全部迁移压成一个不可回退提交。
