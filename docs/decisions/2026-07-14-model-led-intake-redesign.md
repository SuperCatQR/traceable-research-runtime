# 模型主导 Intake 重设计决策

日期：2026-07-14
状态：实施中

## 为什么要做

现有 Intake 名为模型澄清，实则编排器掌握关键决策：强制首轮提问、依据问题数量改写模型决定、达到上限时自行切换状态、允许服务端生成 minimal brief，并要求用户最终确认。此结构把“研究问题是否充分”这一语义判断散落在状态机与服务编排中，模型仅负责填充字段，违背模型主导质询的产品定义。

问题质量判断属于模型能力；编排器无法用布尔值、轮数或字符串规则可靠替代。结构性约束应限制协议、资源与状态合法性，而不裁决问题是否充分。

## 当前背景

- `src/intake.rs` 同时定义持久化事件、状态投影、模型输出协议及问题质量规则。
- `src/app.rs` 同时负责锁与 I/O、模型重试、质询终止、minimal brief 恢复、人工确认及 run 创建。
- `src/backend.rs` 的普通 prompt 强制首轮提问，且无最后一轮专用 prompt。
- `ReadyToConfirm`、`Confirmed` 与 `confirm_intake` 使用户拥有模型澄清流程的最终决策权。
- append-only JSONL、revision/hash 并发保护、trace create-once 与崩溃恢复机制已有价值，应保留。

## 职责边界

### 模型负责

- 反思原问题及全部历史回答尚缺何种信息。
- 决定本轮 `ask` 或 `complete`。
- 生成唯一一个下一问题，或生成最终规范化 brief。
- 在最后允许轮次以专用 prompt 基于已有信息强制完成 brief，不再提问。

### 编排器负责

- 选择普通 prompt 或最后一轮 prompt。
- 调用模型，至多进行一次格式纠错重试。
- 校验 JSON/schema、字段长度、ID、hash 与事件顺序。
- 记录模型决定和用户回答，投影确定性状态。
- 设置最大质询轮数以防死循环。
- 模型完成后创建一次稳定的 run 准备记录，确保幂等及崩溃恢复。

编排器不得判断问题质量，不得强制首轮提问，不得把 `ask` 改为 `complete`，不得自行生成 minimal brief。

### 状态机负责

- 仅验证事件在当前状态是否合法并投影状态。
- 保证 revision、待答问题、轮次、hash、终态不可变等不变量。
- 不推断用户问题是否充分，不生成业务内容。

### Adapter 负责

- HTTP、认证、OpenAI-compatible envelope 与原始 `content` 提取。
- 不解析 Intake 业务 decision，不实施澄清策略。

## 目标协议

模型输出使用显式 decision：

- `decision = "ask"`：必须有一个非空 `question`，并带当前 revised brief。
- `decision = "complete"`：`question` 必须为空，brief 必须完整有效。
- 最后一轮 prompt 仅允许 `complete`。

普通 prompt 不规定首轮必问；模型可认为原问题已充分并直接完成。

## 目标状态

运行态简化为：

- `Draft`：模型调用中或等待重试。
- `NeedsInput`：等待用户回答模型提出的唯一问题。
- `Complete`：模型已完成 brief，run 已可准备或已准备。
- `Failed`：模型调用或协议纠错失败，可重试。
- `Cancelled`：调用方取消。

旧 `ReadyToConfirm`、`Confirmed` 事件继续可重放，作为兼容输入；新流程不再要求用户确认。

## 自动准备边界

澄清 API 不接收搜索 policy，因此模型完成时先持久化 complete brief；调用方以 `prepare_run(clarification_id, policy)` 冻结执行 policy 并取得 `PreparedRun`。此调用是执行配置，不是用户对问题内容的确认。

为避免旧调用方立即破坏：

- 保留旧确认事件的 replay。
- 评估将 `confirm_intake` 作为 deprecated 的幂等 prepare wrapper；不得令其重新引入内容确认语义。
- 删除新流程对 `use_minimal_brief` 的依赖；旧 API 若保留，只可明确 deprecated，不进入正常路径。

## 改动范围

- `src/backend.rs`：普通 Intake prompt 与独立 final prompt。
- `src/intake.rs`：decision schema、事件转换、reducer/状态投影、移除语义裁决。
- `src/app.rs`：轮数选择、模型调用、失败恢复、自动完成及 prepare API。
- `src/lib.rs`：新类型/API 导出与旧接口兼容标记。
- `src/adapters.rs`：仅在模型端口签名需要时调整，不加入业务规则。
- `README.md`、`docs/web-search-architecture.md`：流程、状态与职责更新。
- 测试：首轮直接完成、连续模型质询、最后一轮专用 prompt、格式纠错、失败重试、旧日志重放、prepare 幂等。

递归搜索执行逻辑不在本次重构范围，除 Intake 完成态与 run 准备连接处外不改。

## 验收标准

1. 普通 prompt 明确由模型决定 `ask`/`complete`，且无首轮强制提问。
2. 最大质询轮前使用独立 final prompt；其 schema 禁止继续提问。
3. 编排器仅选择 prompt、重试、校验、持久化与推进状态，不生成 brief、不改写模型 decision。
4. 用户只回答模型问题；无需确认 brief 内容。
5. 模型可在首轮直接完成，亦可连续提问直至上限。
6. 旧 Intake JSONL 可重放；run prepare 仍幂等且能修复确认/创建 trace 间的崩溃窗口。
7. `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 与 `git diff --check` 通过。

## 暂不处理

- HTTP/UI transport；当前 crate 仍为 library-only。
- 跨进程 Intake 文件锁与同 run 并发执行锁。
- 递归搜索策略及抓取算法的进一步调整。
