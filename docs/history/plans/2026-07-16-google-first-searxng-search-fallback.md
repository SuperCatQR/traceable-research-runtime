# 实施计划：SearXNG 搜索改为 Google 优先、Bing 回退

- 状态：已完成并归档；本地实施与验收完成，192.168.1.71 部署另行执行
- 提出日期：2026-07-16
- 计划细化日期：2026-07-17
- 优先级：高
- 依赖：[Trace 边界强化计划](2026-07-16-trace-boundary-hardening-plan.md)

## 1. 目标结果

每个查询先通过自托管 SearXNG 明确请求 Google；只有该次 Google 请求无法给出一个可信的“成功或
正常空结果”结论时，才通过同一个 SearXNG 边界请求 Bing。Bing 的结果完全替代该次 Google 尝试，
不合并两边结果。

```text
ResearchRunExecutor
  -> SearxngSearchClient.search_web(query)
       -> GET SearXNG /search?...&engines=google
       -> Google 可用：返回 Google 结果（允许为空）
       -> Google 不可用：记录类型化原因
       -> GET SearXNG /search?...&engines=bing
       -> 返回 Bing 结果（允许为空），或返回带两次尝试的失败
  -> 写入 Trace v7 的尝试、回退、结果或失败事件
```

搜索标题和 snippet 仍只用于导航；最终事实必须来自归档快照。应用进程不得直接请求 Google/Bing
搜索页、RSS 或其他绕过 SearXNG 的搜索入口。

## 2. 非目标

- 不把 Google 和 Bing 结果混排、打分或合并。
- 不通过 CAPTCHA 绕过、登录模拟或访问控制规避提高 Google 可用性。
- 不在本次引入跨查询熔断器；每个查询都独立执行 Google 优先策略，避免一次故障改变后续查询语义。
- 不为 Trace v6 增加临时兼容字段；搜索审计直接进入 Trace v7。
- 不迁移或混写旧运行目录和旧持久卷，也不删除它们。
- 不把引擎、回退或底层错误显示在 L1 聊天和 L2 研究概览中。

## 3. 当前基线与差距

当前 `SearxngSearchClient` 只有 `Vec<SearchResult>` 成功结果或字符串错误，无法同时返回结果、实际引擎
和失败尝试。它还存在以下与目标冲突的行为：

1. 部署配置只启用 Bing，应用请求没有使用 `engines` 参数固定单一引擎。
2. SearXNG 失败或没有有效 URL 时，应用直接请求 `https://www.bing.com/search?format=rss`。
3. SearXNG 的正常空结果被当成错误，因此会错误触发 Bing RSS。
4. 搜索使用通用 60 秒 HTTP 超时和四次限流重试；最坏等待时间长，且 Trace 看不到这些尝试。
5. Trace v6 的 `SearchResult` 事件没有引擎、尝试结果或回退原因。
6. `quick-xml` 只服务于 Bing RSS 解析；移除 RSS 后应一并移除该依赖和相关类型。

当前调用链只有一个生产调用者：

```text
ResearchRunExecutor
  -> ResearchExecutionBackend.search_web
  -> LiveResearchBackend.search_web
  -> SearxngSearchClient.search_web
```

因此搜索策略应集中在 `SearxngSearchClient` 内部；把 Google/Bing 判断散落到 Research Run 会扩大调用
接口并让测试、Trace 和部署语义分叉。

## 4. 已锁定的设计决策

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| 策略所在模块 | `SearxngSearchClient` | 回退是搜索适配器的实现细节；调用者只需要完整执行结果，保持接口小而深。 |
| SearXNG 路由 | 单端点，两次请求分别传 `engines=google` / `engines=bing` | 不依赖 SearXNG 默认聚合顺序，能明确验证实际引擎。 |
| 成功空结果 | 成功，结果数为 0，不回退 | “没有结果”不等于“Google 不可用”，符合已确认产品规则。 |
| 回退结果 | Bing 独占 | 保持排名语义单一，避免混合两个引擎的不可比较 rank。 |
| 应用内直连搜索引擎 | 删除 | 所有搜索流量必须经过受控 SearXNG 边界。 |
| 尝试次数 | 每个引擎每个查询至多一次，搜索请求使用独立的 15 秒超时 | 回退本身就是恢复策略；移除不可见的四次重试，使最坏耗时和 Trace 一致。 |
| Trace | 类型化事件和原因枚举，不保存原始响应体 | 可以稳定回放、筛选和展示，同时避免把不可信响应或内部错误原文扩散到浏览器。 |
| 兼容策略 | Trace v7 + 新存储代际 | v6 不混写；旧卷只读保留，符合“不考虑兼容”的既有决定。 |
| 双 SearXNG 端点 | 本轮不实现 | 当前固定版本应支持 `engines` 参数；若部署探针证明不支持，部署失败并另行设计，不能静默改变策略。 |

## 5. 目标模块接口

### 5.1 领域类型

在 `src/research_domain.rs` 增加审计所需的稳定类型，名称直接表达业务含义：

```rust
enum SearchEngine {
    Google,
    Bing,
}

enum SearchEngineAttemptOutcome {
    Completed { valid_result_count: u32 },
    Unavailable { reason: SearchEngineUnavailability },
    ContractRejected { reason: SearchBoundaryContractFailure },
}

struct SearchEngineAttempt {
    engine: SearchEngine,
    outcome: SearchEngineAttemptOutcome,
}

enum WebSearchCompletion {
    Completed {
        selected_engine: SearchEngine,
        results: Vec<SearchResult>,
    },
    Failed {
        reason: WebSearchFailureReason,
    },
}

struct WebSearchExecution {
    attempts: Vec<SearchEngineAttempt>,
    completion: WebSearchCompletion,
}
```

最终代码可按 Rust 所有权需要调整字段可见性，但不得退回“只有字符串错误”的接口。预期的调用接口是：

```rust
fn search_web(&mut self, query: &str)
    -> impl Future<Output = WebSearchExecution>;
```

构造客户端时的无效基础 URL、HTTP client 构造失败仍使用现有 `Result`；一次已开始的搜索则必须返回
`WebSearchExecution`，这样即使两台引擎都失败，Research Run 也能先写完整尝试，再写 `RunFailed`。

`SearchResult` 增加 `search_engine` 字段，但 `search_result_id` 继续由 `query + URL` 生成。这样同一 URL
仍能跨引擎/轮次去重，而每条导航结果仍保留来源引擎。

### 5.2 SearXNG 响应契约

`SearxngEnvelope` 需要解析并验证：

- `results`；
- 每条结果的 `engine` / `engines` 元数据；
- `unresponsive_engines`；
- 最多 10 条通过校验的 HTTP(S) URL。

请求必须显式且只带一个 `engines=<expected_engine>`。非空响应的每条 raw result 都必须通过
`engine` / `engines` 元数据证明来自目标引擎；任何其他引擎都属于边界契约失败，终止当前查询，
不用 Bing 掩盖配置错误。引擎校验必须发生在 URL 过滤和最多 10 条截取之前。

SearXNG 的正常空响应没有 envelope 级 selected-engine 回显。空响应按以下组合证明本次请求有效：客户端
确实只发送一个目标 `engines` 参数、响应同时包含 `results` 与 `unresponsive_engines` 两个数组，且目标
引擎未出现在 `unresponsive_engines`。运行时据此接受正常空结果；部署探针仍要求每个引擎至少返回一条
有效结果，以实证实例遵守单引擎路由。

## 6. 精确回退矩阵

| Google 请求结果 | Google 尝试记录 | 是否请求 Bing | 当前查询结果 |
| --- | --- | ---: | --- |
| 200 JSON，至少 1 条有效 Google HTTP(S) 结果 | `completed(count > 0)` | 否 | Google 结果 |
| 200 JSON，无有效结果，且未报告 Google unresponsive | `completed(count = 0)` | 否 | 正常空结果 |
| 200 JSON，同时有有效 Google 结果和 unresponsive 元数据 | `completed(count > 0)` | 否 | Google 结果；有效结果证明本次请求可用 |
| 200 JSON，无有效结果，并报告 Google unresponsive | `unavailable(engine_unresponsive)` | 是 | 取决于 Bing |
| 连接失败、DNS 错误或 15 秒超时 | `unavailable(transport/timeout)` | 是 | 取决于 Bing |
| HTTP 408/429 | `unavailable(timeout/rate_limited)` | 是 | 取决于 Bing |
| HTTP 5xx | `unavailable(server_error)` | 是 | 取决于 Bing |
| HTTP 400/401/403/404 等配置或访问错误 | `contract_rejected(http_status)` | 否 | `search` 阶段失败 |
| JSON 无法解析、缺少必要引擎元数据 | `contract_rejected(invalid_response)` | 否 | `search` 阶段失败 |
| 返回了非 Google 引擎结果 | `contract_rejected(engine_selection_violation)` | 否 | `search` 阶段失败 |

Bing 使用同样的成功、空结果和契约校验，但不再有第三层回退：

- Bing 成功或正常为空：完成当前查询；
- Bing unavailable / contract rejected：保留 Google 和 Bing 两条尝试，以 `ResearchStage::Search` 失败；
- Google unavailable、Bing 正常为空不等于“两者不可用”，因此返回 Bing 的正常空结果。

输入为空属于调用者违反 Research Run 查询不变量：不发外部请求、不回退，并明确失败。

## 7. Trace v7 事件设计

本计划不单独扩展 v6。Trace v7 的事件 envelope 先提供 `sequence` 和 `occurred_at`，搜索再按以下顺序
写入审计事件：

```text
SearchQuery
SearchAttemptCompleted(engine=google, outcome=..., result_count=...)
[SearchFallbackActivated(from=google, to=bing, reason=...)]
[SearchAttemptCompleted(engine=bing, outcome=..., result_count=...)]
[SearchResult(engine=<selected>, ...)] * N
[RunFailed(stage=search, ...)]
```

约束：

1. `SearchFallbackActivated` 只能出现在 Google `Unavailable` 后，reason 必须与前一尝试一致。
2. `ContractRejected` 后不得出现 fallback。
3. `SearchResult.engine` 必须等于 `WebSearchCompletion.selected_engine`。
4. 尝试和 fallback 原因使用稳定枚举；L3 投影生成简短可读文案，不展示 reqwest 原始错误或响应体。
5. L1 Chat DTO 和 L2 Summary DTO 不增加 engine、attempt、fallback 字段。
6. 全部查询正常为空时，不制造搜索失败或空白成功证据；Research Run 继续处理其他查询，并由 Trace v7
   的 `no_new_urls` / 无证据终止规则解释整个探索结果。

## 8. 分阶段实施流程

### 阶段 0：先固定 Trace v7 契约

涉及：`src/research_trace.rs`、`src/research_domain.rs`、`demo-host/src/workspace_api.rs`、前端 DTO。

1. 先完成 Trace v7 envelope、聊天/L2/L3 DTO 白名单和新存储代际测试。
2. 加入搜索事件的类型和回放校验，但暂不改变生产搜索路径。
3. 验证 v7 writer 拒绝 v6 数据目录；旧目录和卷保持原样。

完成门槛：v7 事件能稳定回放，L1/L2 泄露测试通过，才进入搜索适配器改动。

### 阶段 1：先写搜索策略的接口测试

涉及：`src/research_domain.rs`、`src/external_adapters.rs` 的本地 Axum fixture。

1. 为目标领域类型补序列化、等值和 `search_result_id` 不变量测试。
2. fixture 必须记录请求顺序和 query 参数，明确断言 `google` 在 `bing` 前且每次只指定一个引擎。
3. 按第 10 节矩阵先写失败测试，确认旧 Bing RSS 行为不能满足新契约。

完成门槛：测试能够区分空结果、unavailable 和 contract rejected。

### 阶段 2：实现 SearXNG 内的顺序回退

涉及：`src/external_adapters.rs`、`Cargo.toml`、`Cargo.lock`。

1. 将通用 HTTP client 与搜索 client 的超时分开，搜索请求固定为 15 秒。
2. 提取内部 `search_searxng_engine(query, engine)`，只负责一次显式单引擎请求和响应验证。
3. `search_web` 负责 Google -> 可选 Bing 的策略组合，生成完整 `WebSearchExecution`。
4. 删除 `bing_rss_endpoint`、`search_bing_rss`、XML 数据结构、`parse_bing_rss_results`、
   `body_reports_rate_limit` 和四次搜索重试。
5. 删除 `quick-xml` 依赖，并确认仓库不再出现 `bing.com/search` 直连字符串。

完成门槛：适配器测试全绿，单查询最多产生两次 SearXNG HTTP 请求。

### 阶段 3：接入 Research Run 和 Trace

涉及：`src/live_research_backend.rs`、`src/research_run.rs`、`src/research_trace.rs`、`src/lib.rs`。

1. 将 `ResearchExecutionBackend.search_web` 返回值改为 `WebSearchExecution`。
2. Research Run 先顺序写 attempt/fallback，再消费结果；失败时最后写 `RunFailed(Search)`。
3. 正常空结果继续同一轮其他查询，不触发 Bing，也不立即终止整个 Run。
4. 保持最多 10 条、规范化 URL 跨轮去重、快照归档和标题/snippet 仅导航等现有不变量。
5. 更新 fixture backend，使核心测试从模块接口观察行为，不断言适配器内部函数。

完成门槛：成功、空结果、fallback 和双失败都能完整回放且事件顺序确定。

### 阶段 4：只在 L3 投影搜索审计

涉及：`demo-host/src/workspace_api.rs`、`web/src/research-workspace-client.ts`、
`web/src/main.ts`、`web/src/styles.css`。

1. L3 将 engine、outcome、fallback reason 和结果数映射为简短审计条目。
2. 右侧审计详情按 v7 sequence/time 展示；不显示原始响应和内部错误栈。
3. L1/L2 响应白名单保持不变，前端关闭侧栏时仍不请求 Trace 接口。

完成门槛：所有者可复盘每次引擎选择；普通聊天界面没有新增技术噪音。

### 阶段 5：部署配置和强制探针

涉及：`README.md`、`scripts/wsl-demo-up.sh`、`scripts/server-demo-up.sh`，以及新增的可复用 SearXNG
策略验证脚本。

1. SearXNG `keep_only` 同时保留 `google` 与 `bing`，二者名称固定为应用请求使用的值。
2. 不使用 SearXNG 默认聚合结果表达优先级；优先级只由应用的两次显式请求实现。
3. 新增部署前探针，分别请求 `engines=google` 和 `engines=bing`，校验 JSON、引擎元数据、
   `unresponsive_engines` 和至少一个用于部署验收的有效 URL。
4. WSL 从 `127.0.0.1:8888` 执行探针；服务器脚本在 `searxng` 容器内通过 loopback 执行同一探针，
   并确认 SearXNG 与 Demo Host 位于预期 Podman network。
5. 探针必须在构建和替换现有 Demo Host 前运行；失败时保持旧容器继续服务。
6. 可记录运营方允许的出站代理配置，但不得在仓库保存代理凭据，也不得把规避访问控制写成目标。

完成门槛：Google/Bing 任一强制探针失败都阻止新版本替换现有部署。

### 阶段 6：文档、决策记录和新存储部署

涉及：`README.md`、`docs/web-search-architecture.md`、新增 ADR 和执行日志。

1. 新增 ADR，记录“回退位于 SearXNG adapter、单端点显式单引擎请求、空结果不回退”的决定及
   被拒绝方案。
2. 架构图改为 `SearXNG -> Google | Bing fallback`，删除当前 Bing/RSS 描述。
3. 创建 `docs/history/plans/2026-07-17-google-first-searxng-search-fallback-execution-log.md`，逐阶段记录
   测试命令、结果、决策偏差、部署探针和最终 commit。
4. Trace v7 部署将 WSL/server runtime 目录和 Podman volume 从存储代际 `v5` 升为 `v6`；旧 `v5`
   目录、卷和密钥不删除、不复用。
5. 新部署完成后执行 HTTP workspace verifier 和一次真实研究 smoke test，再交付人工测试地址。

## 9. 文件级影响清单

| 文件 | 计划改动 |
| --- | --- |
| `src/research_domain.rs` | 搜索引擎、尝试、完成结果类型；`SearchResult.search_engine` |
| `src/external_adapters.rs` | 显式单引擎请求、回退矩阵、响应验证；删除 Bing RSS/重试 |
| `src/live_research_backend.rs` | 透传 `WebSearchExecution` |
| `src/research_run.rs` | 消费完整搜索执行、写 v7 搜索事件、处理空结果/失败 |
| `src/research_trace.rs` | v7 attempt/fallback/result 事件和回放不变量 |
| `src/lib.rs` | 导出新的公共领域类型 |
| `Cargo.toml` / `Cargo.lock` / `demo-host/Cargo.lock` | 删除只用于 Bing RSS 的 `quick-xml` |
| `demo-host/src/workspace_api.rs` | L3 搜索投影；L1/L2 不泄露断言 |
| `web/src/research-workspace-client.ts` | L3 搜索审计 DTO |
| `web/src/main.ts` / `styles.css` | 右侧栏的紧凑审计展示 |
| `scripts/wsl-demo-up.sh` | Google/Bing 探针、新 v6 存储代际 |
| `scripts/server-demo-up.sh` | 容器内探针、network 检查、新 v6 存储代际 |
| `README.md` | 双引擎 settings、验证、故障排查和部署说明 |
| `docs/web-search-architecture.md` | 当前实现契约和数据流更新 |
| `docs/adr/0008-route-google-first-bing-fallback-through-searxng.md` | 搜索回退架构决策 |

## 10. 测试矩阵

### 核心适配器

- Google 成功：只调用一次 Google；不调用 Bing。
- Google 正常空结果：返回空列表；不调用 Bing。
- Google 只有无效 URL：按正常空结果处理；不调用 Bing。
- Google 有有效结果且同时报告 unresponsive：接受有效结果；不调用 Bing。
- Google `unresponsive_engines`：调用 Bing，记录 engine failure reason。
- Google 连接失败 / 超时 / 429 / 5xx：调用 Bing，原因分类稳定。
- Google 400/401/403/404：终止，不调用 Bing。
- Google JSON 无效或返回其他引擎：契约失败，不调用 Bing。
- Google unavailable + Bing 成功：只返回 Bing 结果，不含 Google 结果。
- Google unavailable + Bing 正常为空：以 Bing 完成，结果为空。
- Google unavailable + Bing unavailable：返回包含两次尝试的失败。
- 结果始终最多 10 条，仅保留 HTTP(S)，rank 从 1 连续编号。
- 请求顺序严格为 Google 后 Bing，每个请求只带一个 `engines` 值。

### Research Run 与 Trace v7

- attempt -> fallback -> attempt -> result 的 sequence 连续且可重放。
- `ContractRejected` 后写失败，不写 fallback。
- 双引擎不可用时最终 `RunFailed.stage == search`。
- 正常空结果不产生 search failure，并继续处理同轮其他查询。
- `SearchResult.search_engine` 与完成引擎一致。
- 旧 URL 去重、快照限制和三查询/轮不变量继续通过。

### Host 与前端

- L1 Conversation/Turn 响应不含 engine、attempt、fallback、raw error。
- L2 Summary 不含 engine、attempt、fallback。
- L3 所有者能看到引擎、结果数和类型化回退原因。
- 未登录为 401；其他用户访问仍为 404；分页和 stage 筛选保持稳定。
- 窄屏右侧抽屉无文本裁切或重叠。

### 部署和真实环境

- 强制 Google 请求只返回 Google 元数据，且未报告 Google unresponsive。
- 强制 Bing 请求只返回 Bing 元数据，且未报告 Bing unresponsive。
- 应用容器网络可访问 SearXNG；应用日志不出现直接 Google/Bing URL。
- 新 v6 卷写入 Trace v7；旧 v5 卷仍存在且未被挂载到新容器。
- 完成一次真实研究后，L3 可以还原实际引擎选择和回退原因。

## 11. 执行留痕规则

实施时每个阶段都在执行日志追加以下信息，不在最后一次性补写：

```text
时间 / 阶段
改动文件
执行命令
测试结果
观察到的事实
决策及理由
与本计划的偏差（没有则写“无”）
下一步
```

任何改变第 4 节锁定决策或第 6 节回退矩阵的情况，都必须先更新本文和 ADR，再继续编码。单纯实现细节
可以记录在执行日志，不需要制造新的产品决策。

## 12. 风险与部署阻断条件

历史服务器探针曾观察到 Google 出站不可用，而 Bing 可用。因此本功能代码完成不代表可以直接部署到
`192.168.1.71`。若 Google 强制探针仍失败，应停止替换当前 Demo，记录实际错误类别，并由运营环境解决
合法网络可达性；不能把“永远走 Bing”伪装为 Google 优先已经上线。

以下任一项阻止部署：

- SearXNG 不接受或不遵守 `engines` 参数；
- Google 或 Bing 强制探针不可用；
- L1/L2 泄露搜索审计细节；
- v7 代码尝试读取或追加 v6 Trace；
- 新容器需要挂载旧 v5 数据卷才能启动；
- 仓库仍存在应用内 `bing.com/search` 直连路径。

## 13. 最终验收

- 正常查询的第一次搜索请求只能是 SearXNG Google。
- 只有第 6 节列出的 Google unavailable 状态触发 SearXNG Bing。
- Google 正常零结果不触发回退；Bing 结果不与 Google 合并。
- 应用进程不直接请求 Google/Bing 搜索页面或 RSS。
- 每次尝试、回退和失败都有稳定、可回放、审阅安全的 L3 记录。
- 两个引擎都不可用时，Research Run 明确以 `search` 阶段失败，不制造空白成功结果。
- L1/L2 保持精简；用户无需点击确认或执行额外操作。
- 新存储上线且旧存储保留后，才交付人工测试地址。
