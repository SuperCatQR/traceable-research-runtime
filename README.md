# Traceable Markdown Document Research Runtime

面向标准化 Markdown 文档语料的可恢复、可溯源、可审计研究 Runtime。

本仓库已实现 Rust 2024 library crate：`TraceableMarkdownResearchRuntime` 作为调用方唯一 Interface，内部包含 SQLite 持久化、不可变 Markdown Corpus Snapshot、问题确认状态机、固定研究执行引擎、完整 Trace 回放、来源完整性校验，以及 Fixture / OpenAI-compatible 两种模型 Adapter。

- [Traceable Markdown Document Research Runtime 架构设计](docs/curated-document-research-architecture.md)
- [领域语言](CONTEXT.md)
- [后端优先开发与交付计划](docs/dev/backend-first-implementation-plan.md)
- [前端 Demo 产品、交互与未来接口草稿](docs/dev/frontend-demo-product-draft.md)

仓库目录保留历史名称 `traceable-research-runtime-database-search`；这里的“database search”不表示 SQL、全文数据库查询或通用数据库搜索。项目只处理符合标准的 Markdown 文档集合。

## 后端实现

公开 Interface 覆盖以下完整生命周期：

1. 发布并锁定一个版本化 Markdown Corpus Snapshot。
2. 创建 Document Research Conversation 与 Request。
3. 执行 Research Question Clarification，必要时接受补充消息或重试。
4. 冻结 Brief、Snapshot、模型引用、执行 limits 与回答方式。
5. 执行或恢复固定研究流程，返回公开答案与执行概览。
6. 按请求投影公开答案、执行概览和分页 Detailed Audit。
7. 在任一非终态取消请求；可重试传输故障保留恢复 checkpoint。

SQLite 使用 WAL、`synchronous=FULL`、append-only event trigger 和 command ledger。所有阻塞存储工作在 Tokio blocking pool 中执行，外部模型调用期间不持有数据库事务。调用方只持有 `ResearchPrincipal`、命令输入、结果与投影类型；原始事件、SQLite、Corpus reader、Integrity Validator 和 Execution Engine 保持内部 Locality。

### 本地运行

需要 Rust `1.96`（仓库已提供 `rust-toolchain.toml`）：

```powershell
cargo run --example fixture_research
```

该示例使用完全离线的模型 Fixture，通过公开 Runtime Interface 发布语料、创建请求、确认问题、prepare、execute，并输出带逐字引用的公开答案。它不访问公网；完整代码见 [`examples/fixture_research.rs`](examples/fixture_research.rs)。

### Live Model Adapter

`OpenAiCompatibleMarkdownResearchModelGateway` 接受 endpoint、API key、强/廉价模型名、单次超时、最大尝试次数和 prompt schema version。仅 timeout、连接错误、HTTP 429/5xx 会按配置重试；非法 JSON、封闭 schema、候选归属和引用完整性错误不会伪装成瞬时故障。API key 在 `Debug` 与错误中始终脱敏。

```rust
use std::{sync::Arc, time::Duration};
use traceable_markdown_research_runtime::{
    OpenAiCompatibleMarkdownResearchModelGateway,
    OpenAiCompatibleMarkdownResearchModelGatewayConfig,
    TraceableMarkdownResearchRuntime,
};

let gateway = OpenAiCompatibleMarkdownResearchModelGateway::new(
    OpenAiCompatibleMarkdownResearchModelGatewayConfig {
        endpoint: "https://model.example/v1/".to_owned(),
        api_key: std::env::var("MODEL_API_KEY").unwrap(),
        strong_model: "strong-model".to_owned(),
        cheap_model: "extraction-model".to_owned(),
        request_timeout: Duration::from_secs(60),
        max_attempts: 3,
        prompt_schema_version: 1,
    },
)?;
let runtime = TraceableMarkdownResearchRuntime::open("runtime.sqlite", Arc::new(gateway))?;
# Ok::<(), traceable_markdown_research_runtime::RuntimeError>(())
```

### 质量门禁

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc
```

测试默认完全离线，覆盖 migration、幂等命令、文件数据库重启恢复、跨主体拒绝、并发双 execute、cancel/execute 竞态、模型故障恢复、Trace/Snapshot/hash/offset/citation 篡改、Live Adapter 超时/5xx/响应大小/密钥隔离、封闭模型 schema、审计页大小和完整固定流程。当前 Gate B 共 76 项测试。

## 前端 Demo

[`frontend/`](frontend/) 提供“迹研”本地交互 Demo，覆盖首次使用、正常研究和失败恢复三种可切换场景。界面展示问题确认、锁定 Snapshot、阶段与真实计数、两种回答方式、逐段来源、逐字引用、Coverage Gap、执行概览和分页审计。

Demo 仅导入本地 typed fixtures；代码中没有 `fetch`、XHR、WebSocket、SSE、API path 或 mock server，不需要启动 Rust Runtime。

```powershell
cd frontend
npm install
npm run dev
```

默认地址为 `http://127.0.0.1:5173/`。前端质量命令：

```powershell
npm test
npm run build
```

## 核心流程

```text
Document Research Conversation
→ Document Research Request
→ Research Question Clarification
→ Frozen Document Research Brief
→ Prepared Markdown Research Execution
→ Model-Knowledge-Only Answer
→ 范围发现与向下探索
→ Verbatim Source Evidence
→ Evidence-Linked Research Claim
→ Evidence-Linked Research Claims Answer
→ Source-Attributed Answer Composition
```

范围发现负责判断问题可能涉及哪些导航方向，向下探索负责在已选方向内找到足够具体的 Markdown 正文。两者构成固定且互相反馈的状态机，不扩展为通用工作流引擎。

模型固定分为两档：

- **强模型**：负责问题确认、全部研究路径、Evidence-Linked Research Claim、Evidence-Linked Research Claims Answer 和最终答案合成。
- **廉价模型**：只在已授权 Markdown Source Segment 内逐字提取 Verbatim Source Evidence。

`MarkdownResearchExecutionEngine` 隐藏分支任务、候选归属、读取预算、正文授权、逐字取证、研究结论、答案合成和恢复复杂度；调用方只通过 `TraceableMarkdownResearchRuntime` 命令 Interface 使用系统。

## Markdown 真源

每篇 Markdown Source Document 只要求三个自然语言分辨率：

```text
markdown_source_document_title        文档标题
markdown_source_document_abstract     文档内容的短描述
canonical_markdown_document_body      canonical Markdown 正文
```

导航如何生成和维护不属于本项目。`TraceableMarkdownResearchRuntime` 负责接收、校验和存储导航，并与 Markdown Source Document Version 共同发布为不可变 `MarkdownCorpusSnapshot`。Markdown Corpus Navigation Node、摘要、模型知识、Evidence-Linked Research Claim 和历史答案都不是事实真源。

## 回答合成方式

同一次 Markdown Research Execution 共享一批 Verbatim Source Evidence 和 Evidence-Linked Research Claim，可以请求一种或同时请求两种 Answer Composition Style：

- `model_knowledge_led`：Evidence-Linked Research Claim : 模型知识 = 2 : 8；以 Model-Knowledge-Only Answer 为基础，由 Evidence-Linked Research Claim 修正和补充。
- `evidence_linked_research_claim_led`：Evidence-Linked Research Claim : 模型知识 = 8 : 2；以 Evidence-Linked Research Claims Answer 为基础，再加入模型知识补充。

两类输入冲突时始终以 Evidence-Linked Research Claim 为准。最终答案的每个 Source-Attributed Answer Segment 必须声明以下来源类型之一：

- `evidence_linked_research_claims`
- `model_knowledge_only`
- `evidence_linked_research_claims_and_model_knowledge`

包含模型知识的回答段明确标记为“模型补充，未由当前 Markdown 文档验证”。

## 保证范围

程序保证：

- 候选和正文读取属于当前 Markdown Research Execution 与锁定 Markdown Corpus Snapshot；
- Verbatim Source Evidence 引文逐字存在于对应 Canonical Markdown Document Body；
- Verbatim Source Evidence、Evidence-Linked Research Claim、Source-Attributed Answer Segment 和 Public Source Citation 的引用关系完整；
- Markdown Research Execution Trace 完整回放后才用于恢复和审计投影。

程序不保证 Verbatim Source Evidence 在语义上支持 Evidence-Linked Research Claim，也不保证模型答案正确。Evidence-Linked Research Claim 只属于产生它的单次 Markdown Research Execution，不得自动成为后续研究的事实来源。

## 当前范围

- 不接入 Markdown 以外的文档来源。
- 不负责生成或治理导航。
- 不实现可自定义研究阶段的工作流平台。
- 当前交付是进程内 Rust library，不提供 HTTP/REST/GraphQL/MCP、CLI、独立 Runtime 进程或部署脚本。
- 不承诺多进程 active-active 执行；同一 Runtime 实例会按 execution ID 串行化模型工作流，多个进程或独立 Runtime 实例仍可能重复外部模型调用，但 SQLite command/checkpoint 和终态提交保持幂等且唯一。
- 不证明模型的语义判断或最终结论正确，只证明可观察的来源、引用、授权、状态和恢复契约。
- Web Search 姊妹项目位于 `C:\WorkSpace\project\traceable-research-runtime-web-search`；本项目参考其生命周期、执行引擎、完整执行记录回放和答案来源标识，不复用 Web 搜索、网页抓取或来源读取 Interface。
