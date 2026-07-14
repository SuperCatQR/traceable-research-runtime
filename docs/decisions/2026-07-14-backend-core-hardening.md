# 后端核心能力加固决策记录

日期：2026-07-14
状态：执行中
范围：Rust library 后端；不恢复已删除的 HTTP/Web transport，不改前端。

## 总体验收标准

1. Intake 任一已持久化失败态均可由调用方取得 `clarification_id`，并可执行模型 retry 或 cancel。
2. 首次确认冻结完整 `TracePolicy`；重复确认与崩溃恢复不得悄然更换运行策略。
3. 研究执行实际遵守 `rounds`、`input_budget`、`max_snapshots`，且轮次仅允许 3–5。
4. 瞬时抓取失败不永久污染“已归档 URL”；重定向汇聚到同一最终页时不重复计数或进入模型上下文。
5. README、架构文档与 library-only 实现一致。
6. `cargo fmt -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 通过。

## 决策 1：先固化审计基线

### 为什么要做

本轮允许后端重构，且要求每次决策留痕。若不先冻结问题、边界与验收，后续易把架构偏好混入缺陷修复，也无法判断改动是否完成。

### 当前现状背景

仓库已于 commit `06d3a98` 删除 Web transport，当前为 Rust library。静态审计确认两大核心功能均已存在，但有契约断口：Intake 的模型结构错误可进入已持久化失败态却不一定把会话句柄返回调用方；确认事件未冻结完整策略；递归搜索只消费 `rounds`，预算与快照上限仍使用全局常量；抓取失败 URL 会被永久视为已见；文档仍混有 HTTP transport 说明。

### 准备做的范围

只处理可由现有产品目标与测试证明的根因：失败恢复、策略冻结与执行、URL 生命周期、最终页去重、公共契约文档。不在本轮引入跨进程文件锁、全新 transport、任务队列或模型 schema 大改；这些属于独立架构决策。

## 决策 2：使 Intake 失败成为可恢复的正常状态

### 为什么要做

一旦 `intake_failed` 已写入 JSONL，调用方必须拿到 `clarification_id`，否则 retry 与 cancel 虽有实现却不可达。持久化成功后再仅返回 command error，会造成“服务器有状态、调用方无句柄”的协议断裂。后续模型主导 Intake 重构已移除编排器生成 minimal brief 的恢复路径，详见 `2026-07-14-model-led-intake-redesign.md`。

### 当前现状背景

`advance_intake` 在第二次模型输出仍不合法时先追加 `intake_failed`，随后返回 `IntakeCommandError::ModelOutput`。`start_intake` 因 `?` 提前退出，不能返回 `IntakeStatus::IntakeFailed`。另有一层问题：adapter 在 JSON 反序列化处提前失败，可能绕过 Intake 自身的两次纠错解析。

### 准备做的范围

统一 Intake completion 的原始文本解析归属；模型信封/HTTP 错误仍作为外部错误处理，正文 schema 错误交由 `parse_model_attempt`。已成功持久化的 `intake_failed` 作为命令成功结果返回。补首错纠正、次错失败且句柄可恢复的 service 测试。

## 决策 3：在确认时冻结并执行完整 TracePolicy

### 为什么要做

Trace header 声称记录运行策略，但执行只读取轮数，预算与快照数使用全局常量；审计记录遂与事实不一致。确认后若能换 policy，也破坏确认幂等性与恢复确定性。

### 当前现状背景

`Confirmed` 事件仅保存 `run_id` 与 brief。`ResearchSession` 构造只接收 `rounds`；`input_budget`、`max_snapshots` 在编排中取常量。轮次 3–5 的常量已定义，却未在确认边界校验。

### 准备做的范围

将完整 `TracePolicy` 写入确认事件并纳入投影；恢复旧事件时保持兼容。首次确认验证并冻结策略；重复确认不得改变它。`ResearchSession` 使用完整策略控制轮次、输入预算与快照上限。补非法轮次、不同策略重复确认、预算与快照边界测试。

## 决策 4：分离 URL 尝试、成功归档与内容去重

### 为什么要做

递归搜索依靠后续轮次补足证据。将一次瞬时抓取失败永久记为 seen，会阻断后续重试；仅按搜索结果 URL 去重，则多个重定向入口可把同一页面重复计数、重复送入模型。

### 当前现状背景

候选 URL 在 crawl 前进入 `seen_urls`；crawl 失败后后轮不再尝试。crawl 成功后未按 `final_url` 或 `snapshot_ref` 二次去重，SQLite 的幂等插入不能阻止内存与预算重复。

### 准备做的范围

保留每轮候选去重；仅成功归档后进入跨轮完成集合。失败 URL 可在后轮有限重试，以现有轮数自然设上限。成功后同时按规范化 `final_url` 与 `snapshot_ref` 去重，再计数、写 checkpoint、送入模型。补失败后重试及重定向汇聚测试。

## 决策 5：校正 library 公共契约与文档

### 为什么要做

transport 删除后，README 与架构文档仍有 HTTP 状态码、Web API 测试等陈述；crate root 又宣称 flat public surface，却漏导出公共方法签名所需类型。文档与 API 不一致会误导后续实现。

### 当前现状背景

当前无 `main.rs`、`web.rs` 或容器入口。高层入口为 `ResearchService`，外部服务仍经 HTTP adapter 调用。部分公共错误和 prepared-run 类型只能从深模块导入。

### 准备做的范围

删除失效 transport 说明，补最小 library 调用路径与运行依赖；补齐高层 API 必需的 crate-root re-export。低层模块暂不做 semver 收口，避免无关大改。

## 暂缓事项与理由

- 跨进程文件锁：重要，但涉及 OS 兼容、锁恢复与部署模型，须独立设计和压力测试。
- 同一 `run_id` 的进程级执行互斥：与任务调度/结果缓存契约相连，宜在策略与恢复不变量稳定后另立决策。
- claims 唯一事实输出：需要改变模型输出 schema 与用户答案格式，不宜夹带于本轮根因修复。
- 搜索 HTTP 重试策略：需统一 `Retry-After`、状态码与时间测试，另立韧性改进步骤。
