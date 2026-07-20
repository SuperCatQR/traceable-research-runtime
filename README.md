# traceable-search

可审计的 Web 研究运行时与 Demo 工作区。用户只通过自然语言聊天表达意图；模型在每个回合同时生成一条可见的理解回复和一个内部结构化 `ResearchBrief`，再自行决定继续对话或自动开始研究。用户不会看到、编辑或确认 Brief，也没有“开始研究”按钮。研究运行时经 Brave Search API、进程内安全网页提取和 OpenAI-compatible 模型完成检索、快照锁定与带来源答案；内部以 `snapshot_ref`、内容哈希和追加式审计记录保持可回放性。

## 架构

```text
Browser / Rust caller
        │
        ├── Demo Host：账户、模型配置、会话所有权、Trace 投影
        └── traceable-search
            ├── Research Conversation：长期上下文与已完成 Turn
            ├── Clarification：模型主导的自然对话与内部 Brief
            ├── Research Run：检索、抓取、选源、合成
            ├── HTTPS ── Brave Search API
            ├── HTTP ── public Web → embedded HTML-to-Markdown
            ├── HTTP ── upstream model
            └── data/
                ├── sessions/<conversation_id>.jsonl
                ├── intake/<clarification_id>.jsonl
                ├── traces/<run_id>.jsonl
                └── snapshots.sqlite
```

详见 [`docs/web-search-architecture.md`](docs/web-search-architecture.md)。

## 前置条件

- Rust toolchain
- Linux 构建依赖：`pkg-config` 与 OpenSSL 开发包（Ubuntu 24.04：`sudo apt install pkg-config libssl-dev`）
- `curl`（服务连通性验证）
- Python 3（仅供下述部署命令生成随机密钥及校验 JSON；`traceable-search` 本身不依赖 Python）

构建并验证：

```bash
cargo build --release
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo fmt --manifest-path demo-host/Cargo.toml --all -- --check
cargo clippy --manifest-path demo-host/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path demo-host/Cargo.toml
npm --prefix web run check
npm --prefix web test
npm --prefix web run build
```

## Library 调用

高层入口为 crate root 导出的 `TraceableResearchRuntime`。`ResearchInfrastructureConfig` 提供共享的 Brave Search API 凭据和数据目录配置；`ModelAccessConfig` 则在每个命令中提供某个用户选定模型的端点、密钥和模型 ID。

1. 调用 `create_conversation()` 创建长期研究会话，再以 `start_research_turn(conversation_id, question, model_access)` 开始一轮。模型只看到本会话先前成功轮次的问题与最终答案；不同会话不会混入，且同一会话只能有一个未完成 Turn。
2. 模型返回 `assistant_message + brief_draft + rationale + decision`。`continue_dialogue` 将 Turn 置为 `AwaitingUserMessage`，调用方以 `submit_dialogue_message(...)` 提交下一句普通用户消息；没有专用澄清题、选项或确认动作。
3. `start_research` 将 Turn 置为 `ResearchReady`。Demo Host 先持久化并返回 pending Turn，再在后台调用 `prepare_research_run_with_answer_style(...)` 冻结模型生成的 Brief 和执行策略，并调用 `execute_prepared_research(...)`。`FrozenResearchBrief` 是内部完整性类型名；`frozen_at` 表示模型批准的 Brief 成为可执行输入的时间，不表示用户曾确认。
4. 成功答案写回 Conversation，成为下一轮理解指代的上下文，但不是当前研究的事实证据。模型调用或结构化输出失败进入 `ModelRequestFailed`；已有对话保留，宿主可重试或接受下一条自然消息，亦可取消当前 Turn。

`TracePolicy` 限制 `rounds = 3..=5`、`input_budget = 1..=1_000_000`、`max_snapshots = 1..=300`。当前 crate 不含 HTTP server；Demo Host 负责鉴权、模型配置、所有权检查和浏览器 transport。单个 `TraceableResearchRuntime` 只在进程内串行化 conversation mutation；多进程部署仍需要外部锁或数据库事务。

### Trace 展示边界

- 聊天正文（L1）只展示自然对话、研究状态、最终答案和必要来源。
- 右侧“研究概览”（L2）由服务端投影，展示理解摘要、检索方向、来源数量、主要来源和合成理由。
- 右侧“审计详情”（L3）按阶段、分页返回带 v7 序号/时间的审阅安全事件，包括 Brave 搜索尝试和探索停止原因。它不包含系统提示词、隐藏推理、API Key、模型原始输入或完整快照正文。

除注册、登录和健康检查外，Demo Host 的业务端点均要求有效的同源 Cookie Session。资源端点按账户所有权过滤，
不存在和属于其他账户的资源统一返回 `404 not_found`，避免资源枚举。完整路由、DTO 与错误契约见
[`docs/workspace-http-api.md`](docs/workspace-http-api.md)。

Conversation 事件 schema 是 v2，Clarification 事件 schema 是 v5，Research Trace 已升级到 v7。v7 每行都有连续 `sequence` 和 `occurred_at`，并且只能通过完整 replay 校验读取；不能在旧 trace 日志上继续写入或回放。部署这一版本时必须使用新的数据目录或持久卷；不要在原卷上原地混用两种 schema。

## 外部服务

Rust 宿主须能访问 Brave Search API、待抓取的公开网页及上游模型端点。请自行准备：

- Brave Search API：服务端 API key；
- 上游模型：OpenAI-compatible `/v1/chat/completions` API、API key 与模型名。

Brave Search API 官方文档：<https://api.search.brave.com/app/documentation/>。API key 只注入 Rust 宿主，不能写入前端、镜像或 Trace。

网页正文由 Rust 进程直接执行 DNS/SSRF 校验、重定向控制、大小限制、HTML allowlist 清洗和 HTML-to-Markdown 转换，不需要 Python、浏览器或额外抓取服务。

推荐使用 `deepseek-v4-pro`，已验证普通调用可用。当前项目不发送思考模式参数；思考行为由上游模型决定，项目仅解析最终 `content`。

> [!CAUTION]
> 勿将任何 token 或 API key 提交至仓库、写入镜像或公开日志。

## 配置 traceable-search

仓库维护无凭据模板 `.env.example`；真实 `.env` 已由 `.gitignore` 排除。复制模板并填写依赖：

```bash
cp .env.example .env
```

本 library 读取进程环境，不会自动解析 `.env`。宿主启动前加载：

```bash
set -a
source .env
set +a
mkdir -p "$TRACEABLE_SEARCH_DATA_DIR"
test -w "$TRACEABLE_SEARCH_DATA_DIR"
```

变量说明：

| 变量 | 必需 | 含义 |
|---|---:|---|
| `BRAVE_SEARCH_API_KEY` | 是 | Brave Search API 服务端密钥 |
| `STRONG_MODEL_BASE_URL` | 是 | 上游模型的 OpenAI-compatible API 基础 URL |
| `STRONG_MODEL_API_KEY` | 是 | 上游模型签发的 API key |
| `STRONG_MODEL_ID` | 是 | 上游模型名；建议 `deepseek-v4-pro` |
| `TRACEABLE_SEARCH_DATA_DIR` | 否 | `sessions/`、`intake/`、`traces/` 与快照目录；默认 `data`；运行用户须有写权限 |
| `DEMO_CREDENTIAL_ENCRYPTION_KEY` | Demo Host | base64 编码的 32-byte Model Profile 加密主密钥 |
| `VITE_API_PROXY_TARGET` | Vite 开发 | `/api` 同源代理目标；默认 `http://127.0.0.1:8080` |

> [!IMPORTANT]
> Brave Search API 使用固定官方端点；程序只从服务端环境读取 API key。


先验证 Brave Search API 与 OpenAI-compatible API 可达。宿主完成一次成功研究后，本项目应生成：

```text
data/snapshots.sqlite
data/sessions/<conversation_id>.jsonl
data/intake/<clarification_id>.jsonl
data/traces/<run_id>.jsonl
```

`data/sessions/` 保存长期会话和已完成 Turn；`data/intake/` 保存原问题、普通用户消息、模型理解、自动研究准备、取消或失败的 append-only 历史；`data/traces/` 在 Run 准备后保存检索与答案审计。删除或截断任一 JSONL 都会破坏审计与重启恢复，勿以此处理失败会话。

常见故障：

| 现象 | 检查 |
|---|---|
| Brave Search API 返回错误 | 检查 `BRAVE_SEARCH_API_KEY`、HTTP 状态、额度与服务端日志 |
| 模型返回 401 | `STRONG_MODEL_API_KEY` 是否有效 |
| 模型端点 404 | `STRONG_MODEL_BASE_URL` 是否指向 OpenAI-compatible `/v1/` API |
| 模型不可用 | `STRONG_MODEL_ID` 是否与上游暴露名称完全一致；建议 `deepseek-v4-pro` |
| 无法配置思考模式 | 当前版本未发送思考参数；请在上游模型侧配置 |

## WSL2 Demo

推荐使用仓库根目录的 `compose.yaml` 启动 App。复制 `.env.example` 后，填写 Brave API key 和仓库外持久保存的加密主密钥：

```bash
python3 -c 'import base64, os; print(base64.b64encode(os.urandom(32)).decode())'
docker compose up -d --build
```

默认从 Windows 浏览器打开 <http://127.0.0.1:8080>。若覆盖 `DEMO_HOST_PORT`，还须同步覆盖 `DEMO_TRUSTED_ORIGINS`。停止时运行 `docker compose down`；不要使用 `down -v` 删除研究数据卷。

仓库不维护环境专用的启动、停止或服务器替换脚本。若只需构建 App 镜像，可从仓库根目录运行：

```bash
podman build --tag traceable-search-demo --file demo-host/Containerfile .
```

Backend continuation status: Catalog v7 now preserves durable operation IDs,
Runtime conversation/clarification logs are replayable, and each protected
write commits its Catalog resource and response snapshot in one fenced SQLite
transaction. Fault-injected process restart, Compose restart, and live
Brave/model acceptance remain environment gates; a provider without request
idempotency can still receive a repeated billable call after a local crash.

运行镜像时由部署环境显式提供 `.env.example` 中的变量、可写数据卷和端口映射。`DEMO_CREDENTIAL_ENCRYPTION_KEY` 必须是仓库外持久保存的 base64 编码 32-byte 密钥；不得删除或替换，否则既有 Model Profile 的加密 API Key 无法解密。Research Trace v7 必须使用新的空数据目录或 volume，不得继续写入旧 schema 数据。

Host 启动后首次使用依次注册账户、添加 OpenAI-compatible
Model Profile、创建研究对话。用户只在聊天框中发送问题或补充；模型会自然回复理解，并在信息足够时自动开始研究。对话、对话式校准状态和已完成答案会保留在 Podman volume 中，重启 Host 后仍可恢复。

当前已实现持久状态加载和非终态自动研究候选恢复，但本轮尚未完成 Compose、真实模型/Brave 的进程重启验收。
进程若在 Runtime/文件系统或 Catalog 副作用提交后、幂等 completion 持久化前退出，四类受保护写操作还不保证
crash-safe exactly-once；详见 [`docs/workspace-http-api.md`](docs/workspace-http-api.md#open-delivery-gate)。
