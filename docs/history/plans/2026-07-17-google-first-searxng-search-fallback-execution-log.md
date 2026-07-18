# Trace v7 与 Google-first SearXNG 实施日志

本日志对应：

- [Trace 边界强化计划](2026-07-16-trace-boundary-hardening-plan.md)
- [Google-first / Bing fallback 计划](2026-07-16-google-first-searxng-search-fallback.md)

## 2026-07-17 09:44 +08:00 / 范围确认

### 改动文件

尚未修改生产代码。确认后续范围覆盖：

- 核心 Trace：`src/research_trace.rs`、`src/research_run.rs`、`src/runtime.rs`、`src/lib.rs`；
- 搜索：`src/research_domain.rs`、`src/external_adapters.rs`、`src/live_research_backend.rs`、
  `Cargo.toml`、两份 `Cargo.lock`；
- Host/前端：`demo-host/src/workspace_api.rs`、`demo/src/research-workspace-client.ts`、
  `demo/src/main.ts`、`demo/src/styles.css`、HTTP verifier；
- 部署与文档：WSL/server 脚本、README、架构文档、ADR 和 SearXNG 策略探针。

不修改 Clarification 的模型主导自然对话语义，不增加用户确认操作，不迁移或删除旧存储。

### 执行命令

```text
cargo test --all-targets
cargo test --manifest-path demo-host/Cargo.toml --all-targets
npm.cmd run check
```

### 测试结果

- 核心：93 项通过；1 项 live test 按设计忽略。
- Demo Host：12 项通过。
- 前端：未执行；清理构建产物后当前没有 `node_modules`，将在前端切片前运行 `npm ci` 后补验。

### 观察到的事实

1. Trace v6 的每行直接序列化 `TraceEvent`，没有 sequence/timestamp envelope；resume 只投影最后完成轮次，
   未保留全部事件的 sequence 游标。
2. Runtime 的完成恢复和 Host 的 L2/L3 加载各自直接读取 JSONL，绕开同一个完整 replay 接口。
3. Conversation 主响应当前返回模型端点、模型 ID、knowledge draft、comparison 和 claim rationale，超出 L1。
4. 搜索适配器把正常空结果当错误，并绕过 SearXNG 直连 Bing RSS。
5. SearXNG 正常空响应没有 selected-engine 回显；非空结果才有 `engine` / `engines` 元数据。
6. 根目录和 `demo-host` 各有一份 lock file，删除 `quick-xml` 时必须同步更新。

### 决策及理由

测试接缝固定为：

1. `TraceWriter` + 唯一 replay 接口：验证持久化顺序、时间、恢复和 schema 拒绝；
2. `SearxngSearchClient.search_web`：通过本地 HTTP fixture 验证公开行为，不测试私有 helper；
3. Host Chat/L2/L3 HTTP 投影：通过 exact-key 白名单和拥有权行为验证，不让浏览器过滤原始数据。

空搜索响应的判定修订为“单一目标请求参数 + 两个必需数组 + 目标引擎未 unresponsive”。理由是 SearXNG
无法在空结果中返回逐条引擎元数据；部署探针继续用非空结果实证路由。

### 与计划的偏差

- 文件范围补充 `demo-host/Cargo.lock`。
- 澄清空结果的可证明条件；不改变“正常空结果不回退”的产品决定。

### 下一步

以一个垂直 TDD 切片实现 Trace v7 envelope：sequence 从 1 连续、持久时间不倒退、resume 从全部已写事件
继续 sequence，并拒绝 v6 trace 目录。

## 2026-07-17 14:22 +08:00 / 阶段 0：Trace v7 与展示边界

### 改动文件

- `src/research_trace.rs`、`src/runtime.rs`、`src/lib.rs`
- `demo-host/src/workspace_api.rs`
- `web/src/research-workspace-client.ts`、`web/src/main.ts`、`web/src/styles.css`

### 执行命令与结果

```text
cargo test research_trace::tests -- --nocapture
# 14 passed

cargo test --no-run
# core 编译通过

cargo test --no-run  # demo-host
# Demo Host 编译通过

cargo test workspace_api::tests -- --nocapture  # demo-host
# 7 passed

npm.cmd run check  # web
# TypeScript 检查通过
```

### 观察到的事实

1. v7 每行现为 `TraceEventEnvelope`，包含从 1 连续的 `sequence` 和不倒退的 `occurred_at`。
2. Runtime 终态恢复和 Host 研究 Trace 加载均改用 `replay_trace`；仓库内已无
   `replay_run_header`、`ReplayedRunHeader` 或研究事件裸 JSONL 读取路径。
3. v1/v2 在线兼容类型和 test-only legacy writer 已移除；v7 头的 brief 与 clarification ID 为必需字段。
4. L2 继续不返回 engine/attempt/fallback；L3 返回安全投影后的序号、时间和搜索决策。
5. 前端目录已由另一项已保留的迁移从 `demo/` 改为 `web/`，因此本计划按当前目录实施。

### 决策及理由

- `replay_trace` 是唯一公开研究 Trace 读取接口。理由：只有完整读取才能同时验证 schema、事件顺序、
  时间顺序、轮次、理由和终止状态，单独读取头会给调用方留下绕过校验的入口。
- Clarification 事件不伪造 v7 sequence；L3 对其返回空 sequence/time，研究事件返回真实 envelope 元数据。
  理由：Clarification 有独立 schema，不能把两个日志的序号拼成一个虚假的全局顺序。

### 与计划的偏差

- 前端文件路径由 `demo/` 调整为 `web/`；产品边界与 DTO 语义不变。

### 下一步

完成领域文档、ADR、SearXNG 双引擎配置、强制策略探针与新存储代际部署脚本。

## 2026-07-17 14:22 +08:00 / 阶段 1-3：Google-first 与 Research Run 接入

### 改动文件

- `src/research_domain.rs`、`src/external_adapters.rs`、`src/live_research_backend.rs`
- `src/research_run.rs`、`src/research_trace.rs`
- `Cargo.toml`、`Cargo.lock`、`demo-host/Cargo.lock`

### 执行命令与结果

```text
cargo test external_adapters::tests -- --nocapture
# 16 项 adapter 场景通过

cargo test research_run::tests -- --nocapture
# 21 passed
```

### 观察到的事实

1. 每个查询先显式请求 `engines=google`；只有 Google 被分类为 unavailable 才请求 `engines=bing`。
2. Google 正常空结果不回退；4xx 契约错误、错误 JSON 或错误引擎结果也不回退并明确失败。
3. Bing RSS 直连、四次隐式重试和 `quick-xml` 已删除；搜索使用独立 15 秒 HTTP client。
4. Research Run 按 attempt、fallback、attempt、result 顺序写 Trace，并记录结果所属引擎。
5. `completed_rounds`、`input_budget`、`snapshot_limit`、`no_new_urls` 四种停止原因均恰好写入一个
   `ExplorationStopped` 事件。

### 决策及理由

- 引擎校验发生在 URL 过滤和数量截断之前，避免错误引擎结果借由无效 URL 被静默丢弃。
- `search_result_id` 继续只由 query + URL 生成，保留跨引擎、跨轮次 URL 去重语义。
- 停止原因属于探索生命周期，不属于单个查询失败；单独事件能让恢复和审计使用同一事实。

### 与计划的偏差

无。

### 下一步

完成部署配置和文档后运行全量测试、HTTP verifier 和真实 SearXNG 强制引擎探针。

## 2026-07-17 14:48 +08:00 / 本地全量验收与远端部署门禁

### 改动文件

- `scripts/verify-searxng-search-policy.py`
- `scripts/wsl-demo-up.sh`、`scripts/server-demo-up.sh`
- `scripts/verify-demo-workspace.mjs`
- `README.md`、`CONTEXT.md`、`docs/web-search-architecture.md`
- `docs/adr/0008-route-google-first-bing-fallback-through-searxng.md`
- `docs/adr/0009-require-complete-v7-trace-replay-before-projection.md`

### 执行命令与结果

```text
cargo test --all-targets
# 101 passed；live E2E 1 项因需要真实外部服务按设计 ignored

cargo test --all-targets  # demo-host
# 15 passed

cargo clippy --all-targets -- -D warnings
# core 通过

cargo clippy --all-targets -- -D warnings  # demo-host
# 通过

cargo fmt --all -- --check
# core / demo-host 均通过

npm.cmd run check
npm.cmd run build
# TypeScript 与 Vite production build 通过

node scripts/verify-demo-workspace.mjs
# HTTP workspace verifier 通过

python scripts/verify-searxng-search-policy.py --help
node --check scripts/verify-demo-workspace.mjs
# 探针入口与 verifier 语法通过
```

### 观察到的事实

1. HTTP verifier 的首个 Google fixture 返回 unresponsive，随后 Bing 成功；L3 可见回退，L1 无
   `run_id`，L2 JSON 不含 engine/fallback，L3 研究事件包含 sequence/time。
2. WSL 当前没有可用 Linux 发行版，不能在本机执行目标 shell 或真实 loopback 探针。
3. 远端只读 SSH 首次授权请求未执行：授权审查服务返回 503。没有向服务器发送命令，也没有替换容器。
4. 浏览器安全策略阻止访问本地预览 URL；没有改端口或换浏览器绕过。Web production build 和 HTTP
   verifier 仍已通过，但本轮没有新增浏览器截图证据。

### 决策及理由

- 两个部署脚本都在 image build 和旧 Host 删除之前运行强制 Google/Bing 探针；失败时 shell 的
  `set -e` 保留当前部署。
- Trace schema 是 v7，部署存储代际是 v6；名称刻意不同，旧 v5 目录、卷与密钥均不删除、不复用。
- 授权服务失败不等于服务器或 Google 探针失败，因此计划状态记录为“待远端探针及部署”，不宣称上线。

### 与计划的偏差

- 本机 WSL 与浏览器截图验收受环境策略阻断；后端、Web 构建和 HTTP 集成验收已完成。

### 下一步

获得新的远端执行授权后，在 `192.168.1.71` 先运行 `bash -n` 和容器内双引擎探针。只有两项均通过
才同步源码、构建新镜像、使用 v6 新卷替换 Host，并执行 health、存储代际和真实 L3 smoke test。

## 2026-07-17 15:06 +08:00 / 完成审计补强

### 改动文件

- `scripts/verify-searxng-search-policy.py`
- `scripts/test_verify_searxng_search_policy.py`
- `src/external_adapters.rs`、`src/research_run.rs`、`src/research_trace.rs`

### 执行命令与结果

```text
python scripts/test_verify_searxng_search_policy.py
# 4 passed

cargo test external_adapters::tests -- --nocapture
# 22 passed

cargo test research_run::tests -- --nocapture
# 24 passed

cargo test research_trace::tests -- --nocapture
# 16 passed

cargo test --all-targets
# 110 passed；live E2E 1 项按设计 ignored

cargo test --all-targets  # demo-host
# 15 passed

cargo clippy --all-targets -- -D warnings
# core / demo-host 均通过

cargo build  # demo-host
node scripts/verify-demo-workspace.mjs
# HTTP workspace verifier 再次通过
```

### 观察到的事实

1. 初版部署探针只拒绝目标引擎自身出现在 `unresponsive_engines`；若强制 Google 响应声称 Bing
   unresponsive，仍会错误放行。补强后任何 unresponsive 项、畸形 pair、错误引擎元数据或无主机 URL
   都会阻止部署。
2. Adapter 测试矩阵现直接覆盖无效 URL 正常空结果、有效结果与同引擎 unresponsive 并存、
   408/429/5xx 回退、普通 4xx/坏 JSON/错误引擎不回退、Bing 正常空结果、transport 双失败和最多
   10 条连续排名。
3. Research Run 直接验证 contract rejected 不写 fallback、双 unavailable 后 `RunFailed(Search)`，以及
   三个正常空查询完成整轮后以 `no_new_urls` 停止。
4. v7 replay 现在要求每个完成轮次恰好三个全局不重复查询，`previous_queries` 与声明顺序完全一致，
   并验证结果数量、排名、引擎和 `search_result_id`。
5. 用户再次要求继续后，只读 SSH 授权仍因同一审批服务 503 未执行；服务器状态未改变。

### 决策及理由

- 部署探针比运行时空结果规则更严格：部署必须用非空结果证明两台引擎的强制路由，并要求没有任何
  unresponsive 元数据。运行时仍允许目标引擎未 unresponsive 的正常空结果。
- 已完成轮次必须能从 Trace 自身证明三查询不变量；不能只信任写入时调用路径。

### 与计划的偏差

无产品语义偏差；仅增加计划矩阵原本要求但证据不足的测试与回放校验。

### 下一步

等待 SSH 审批服务恢复并取得服务器 SSH 用户名，然后执行远端门禁与部署。

## 2026-07-17 20:13 +08:00 / L3 缓存隔离与计划归档

### 改动文件

- `web/src/research-trace-audit-cache.ts`
- `web/src/main.ts`
- `web/test/research-trace-audit-cache.test.mjs`
- `web/package.json`
- `docs/web-search-architecture.md`

### 执行命令与结果

```text
npm.cmd test
# 修复前连续两次稳定失败：Turn A 的 search 审计页会被 Turn A 的 archive 筛选复用
# 修复后 1 passed

npm.cmd run check
# TypeScript 检查通过

npm.cmd run build
# Vite production build 通过

cargo test --all-targets
# Core 110 passed；真实依赖 live E2E 1 项按设计 ignored

cargo test --all-targets  # demo-host
# 15 passed

cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
# Core / demo-host 均通过

python scripts/test_verify_searxng_search_policy.py
# 4 passed

node scripts/verify-demo-workspace.mjs
# HTTP workspace verifier 通过
```

### 观察到的事实

1. 原缓存只以 Turn ID 为 key，而当前 stage 是独立的全局状态；已有缓存因此不能证明与当前筛选一致。
2. 缓存现按 `Turn ID -> stage -> page` 两层隔离；读取、分页追加、渲染和失效都使用同一个复合作用域。
3. 回归测试覆盖两个 Turn、两个不同 stage，明确拒绝跨 stage 复用页面。

### 决策及理由

保留不同 Turn 和 stage 的有效缓存，而不是每次切换筛选都清空全部审计页。复合缓存作用域直接表达 HTTP
请求的身份，既消除错误复用，也保留安全的按需加载结果。

### 与计划的偏差

无。该修复补齐 L3 阶段筛选的浏览器缓存边界。

### 下一步

按用户要求将两份本地实施计划移入 `docs/history/plans/`。远端探针与部署不作为这两份已归档本地计划的
完成条件；若继续上线，应单独建立部署任务并保留既有强制探针门禁。
