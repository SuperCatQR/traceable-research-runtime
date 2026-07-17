# traceable-search

可审计的 Web 研究运行时与 Demo 工作区。用户只通过自然语言聊天表达意图；模型在每个回合同时生成一条可见的理解回复和一个内部结构化 `ResearchBrief`，再自行决定继续对话或自动开始研究。用户不会看到、编辑或确认 Brief，也没有“开始研究”按钮。研究运行时经 SearXNG/Bing、crawl4ai 和 OpenAI-compatible 模型完成检索、快照锁定与带来源答案；内部以 `snapshot_ref`、内容哈希和追加式审计记录保持可回放性。

## 架构

```text
Browser / Rust caller
        │
        ├── Demo Host：账户、模型配置、会话所有权、Trace 投影
        └── traceable-search
            ├── Research Conversation：长期上下文与已完成 Turn
            ├── Clarification：模型主导的自然对话与内部 Brief
            ├── Research Run：检索、抓取、选源、合成
            ├── HTTP ── SearXNG ── Bing（当前）
            ├── HTTP ── crawl4ai
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
```

## Library 调用

高层入口为 crate root 导出的 `TraceableResearchRuntime`。`ResearchInfrastructureConfig` 提供共享的 SearXNG、crawl4ai 和数据目录配置；`ModelAccessConfig` 则在每个命令中提供某个用户选定模型的端点、密钥和模型 ID。

1. 调用 `create_conversation()` 创建长期研究会话，再以 `start_research_turn(conversation_id, question, model_access)` 开始一轮。模型只看到本会话先前成功轮次的问题与最终答案；不同会话不会混入，且同一会话只能有一个未完成 Turn。
2. 模型返回 `assistant_message + brief_draft + rationale + decision`。`continue_dialogue` 将 Turn 置为 `AwaitingUserMessage`，调用方以 `submit_dialogue_message(...)` 提交下一句普通用户消息；没有专用澄清题、选项或确认动作。
3. `start_research` 将 Turn 置为 `ResearchReady`。Demo Host 先持久化并返回 pending Turn，再在后台调用 `prepare_research_run_with_answer_style(...)` 冻结模型生成的 Brief 和执行策略，并调用 `execute_prepared_research(...)`。`FrozenResearchBrief` 是内部完整性类型名；`frozen_at` 表示模型批准的 Brief 成为可执行输入的时间，不表示用户曾确认。
4. 成功答案写回 Conversation，成为下一轮理解指代的上下文，但不是当前研究的事实证据。模型调用或结构化输出失败进入 `ModelRequestFailed`；已有对话保留，宿主可重试或接受下一条自然消息，亦可取消当前 Turn。

`TracePolicy` 限制 `rounds = 3..=5`、`input_budget = 1..=1_000_000`、`max_snapshots = 1..=300`。当前 crate 不含 HTTP server；Demo Host 负责鉴权、模型配置、所有权检查和浏览器 transport。单个 `TraceableResearchRuntime` 只在进程内串行化 conversation mutation；多进程部署仍需要外部锁或数据库事务。

### Trace 展示边界

- 聊天正文（L1）只展示自然对话、研究状态、最终答案和必要来源。
- 右侧“研究概览”（L2）由服务端投影，展示理解摘要、检索方向、来源数量、主要来源和合成理由。
- 右侧“审计详情”（L3）按阶段、分页返回审阅安全事件。它不包含系统提示词、隐藏推理、API Key、模型原始输入或完整快照正文。

Demo Host 的拥有者受限端点为：

- `POST /api/conversations/{conversation_id}/turns`
- `POST /api/conversations/{conversation_id}/turns/{turn_id}/messages`
- `GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/summary`
- `GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/audit?stage=&cursor=&limit=`

Conversation 事件 schema 已升级到 v2，Clarification 事件 schema 已升级到 v5，Research Trace 已升级到 v6；它们不能在旧 session、v2/v3/v4 `intake` 或 v5 trace 日志上继续写入或回放。部署这一版本时必须使用新的数据目录或持久卷；不要在原卷上原地混用两种 schema。

## 外部服务

本项目不部署或管理外部服务。Rust 宿主须能访问 SearXNG、待抓取的公开网页、crawl4ai 及上游模型端点。请自行准备：

- SearXNG：自托管 JSON Search API；
- crawl4ai `0.9.1`：可访问的 `/crawl` API 及 bearer token（若启用认证）；Rust 侧先安全抓取并清洗网页，再提交 `raw:<html>`，故 crawl4ai 只负责离线 HTML 转 Markdown；
- 上游模型：OpenAI-compatible `/v1/chat/completions` API、API key 与模型名。

### SearXNG 部署示例

以下为 WSL rootless Podman 示例。宿主机需有 `python3`，用于生成 `SECRET_KEY` 及后续校验 JSON；SearXNG 容器已自带其运行环境：

```bash
install -d -m 700 ~/.config/searxng
SECRET_KEY="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
umask 077
cat > ~/.config/searxng/settings.yml <<EOF
use_default_settings:
  engines:
    keep_only:
      - bing

general:
  instance_name: "traceable-search"

server:
  secret_key: "$SECRET_KEY"
  bind_address: "0.0.0.0"
  port: 8080
  limiter: false
  image_proxy: false

search:
  safe_search: 0
  autocomplete: ""
  default_lang: "auto"
  formats:
    - html
    - json

engines:
  - name: bing
    engine: bing
    shortcut: bi
    disabled: false
    base_url: https://cn.bing.com
EOF
unset SECRET_KEY

podman run -d \
  --name searxng \
  --restart=unless-stopped \
  --publish 127.0.0.1:8888:8080 \
  --volume ~/.config/searxng:/etc/searxng:Z \
  docker.io/searxng/searxng@sha256:bf2700fa1e7b63c9ef577004513efef509f9c23bfa2cd6e56be08211508df95a
```

验证 JSON Search API：

```bash
curl -fsS 'http://127.0.0.1:8888/search?q=Rust&format=json&categories=general' \
  | python3 -c '
import json, sys
data = json.load(sys.stdin)
engines = sorted({e for item in data.get("results", []) for e in item.get("engines", [])})
print("results:", len(data.get("results", [])))
print("engines:", engines)
assert data.get("results") and engines == ["bing"]
'
```

> [!NOTE]
> 此配置仅保留 Bing engine，并使用 `https://cn.bing.com`。当前中英文 smoke test 均返回 10 条结果，且 `engines` 唯一为 `bing`。SearXNG 此处抓取 Bing 搜索页，并非调用官方 Bing Search API；仍可能受 CAPTCHA、限流或页面结构变化影响。

> [!CAUTION]
> 此配置关闭 limiter，仅适用于 localhost。若对外开放，须配置反向代理、HTTPS、访问控制及 Valkey/Redis limiter。

### crawl4ai 部署示例

本项目不维护 crawl4ai 镜像；以下为 WSL rootless Podman 示例。先生成 token 与权限受限的 env file：

```bash
install -d -m 700 ~/.config/traceable-search
umask 077
TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
printf 'CRAWL4AI_API_TOKEN=%s\n' "$TOKEN" > ~/.config/traceable-search/crawl4ai.env
unset TOKEN
```

官方 `0.9.1` tag 在部分环境会因 Playwright browser 位于 root 目录、而服务以 `appuser` 运行而启动失败。构建一个只修正 browser 路径的派生镜像：

```bash
cat > /tmp/Containerfile.crawl4ai <<'EOF'
FROM docker.io/unclecode/crawl4ai:0.9.1
USER root
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN mkdir -p /opt/ms-playwright \
 && cp -a /root/.cache/ms-playwright/. /opt/ms-playwright/ \
 && chmod -R a+rX /opt/ms-playwright
USER appuser
EOF

podman build \
  --tag localhost/crawl4ai-with-browser:0.9.1 \
  --file /tmp/Containerfile.crawl4ai \
  /tmp
```

启动派生镜像：

```bash
podman run -d \
  --name traceable-search-crawl4ai \
  --restart=unless-stopped \
  --shm-size=1g \
  --publish 127.0.0.1:11235:11235 \
  --env-file ~/.config/traceable-search/crawl4ai.env \
  localhost/crawl4ai-with-browser:0.9.1
```

将同一 token 填入项目 `.env` 的 `CRAWL4AI_TOKEN`，而服务端变量名须为 `CRAWL4AI_API_TOKEN`。

> [!NOTE]
> 此派生镜像不下载或升级 browser，只把官方镜像已有文件复制到 `appuser` 可读的稳定路径。若构建时 `/root/.cache/ms-playwright` 为空，说明上游镜像已变化；请依该版本官方文档安装匹配的 Chromium，勿混用不同 Playwright 版本。

> [!CAUTION]
> 仅绑定 `127.0.0.1` 或可信私网；勿公开暴露 `/crawl`。生产环境还应限制容器访问私网、loopback、link-local 与云 metadata 地址，以防 SSRF。token 文件须保持 `0600`，且不得提交至仓库。

检查服务：

```bash
podman logs --tail 50 traceable-search-crawl4ai
curl -i http://127.0.0.1:11235/
```

未带 token 返回 `401` 说明认证已启用；最终应使用带 token 的 `POST /crawl` 提交一段 `raw:<html>`，验证离线 HTML 转 Markdown。真实网页由 Rust 侧在 SSRF 与重定向校验后抓取，crawl4ai 无须访问目标站点。

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
| `SEARCH_BASE_URL` | 是 | SearXNG 基础 URL；保留尾部 `/` |
| `CRAWL4AI_BASE_URL` | 是 | crawl4ai 基础 URL；保留尾部 `/` |
| `CRAWL4AI_TOKEN` | 否 | crawl4ai bearer token；服务启用认证时填写 |
| `STRONG_MODEL_BASE_URL` | 是 | 上游模型的 OpenAI-compatible API 基础 URL |
| `STRONG_MODEL_API_KEY` | 是 | 上游模型签发的 API key |
| `STRONG_MODEL_ID` | 是 | 上游模型名；建议 `deepseek-v4-pro` |
| `TRACEABLE_SEARCH_DATA_DIR` | 否 | `sessions/`、`intake/`、`traces/` 与快照目录；默认 `data`；运行用户须有写权限 |

> [!IMPORTANT]
> 基础 URL 应保留尾部 `/`。程序分别拼接 `search`、`crawl` 与 `chat/completions`。


先验证 SearXNG、crawl4ai 与 OpenAI-compatible API 可达。宿主完成一次成功研究后，本项目应生成：

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
| SearXNG 返回空结果 | 检查 Bing engine、`unresponsive_engines` 与 `SEARCH_BASE_URL` |
| crawl4ai 返回认证失败 | `CRAWL4AI_TOKEN` 是否为该服务签发的有效 token |
| 模型返回 401 | `STRONG_MODEL_API_KEY` 是否有效 |
| 模型端点 404 | `STRONG_MODEL_BASE_URL` 是否指向 OpenAI-compatible `/v1/` API |
| 模型不可用 | `STRONG_MODEL_ID` 是否与上游暴露名称完全一致；建议 `deepseek-v4-pro` |
| 无法配置思考模式 | 当前版本未发送思考参数；请在上游模型侧配置 |

## WSL2 Demo

在 WSL2 已准备 SearXNG、crawl4ai 和项目 `.env` 后，运行：

```bash
bash scripts/wsl-demo-up.sh
```

脚本会构建同源前端与 Demo Host，确保依赖容器运行，并在
`~/.config/traceable-search-demo-v5/` 生成权限为 `0600` 的持久主密钥与运行时 env file。
主密钥不得删除或替换，否则既有 Model Profile 的加密 API Key 无法解密。

从 Windows 浏览器打开 <http://127.0.0.1:8080>。首次使用依次注册账户、添加 OpenAI-compatible
Model Profile、创建研究对话。用户只在聊天框中发送问题或补充；模型会自然回复理解，并在信息足够时自动开始研究。对话、对话式校准状态和已完成答案会保留在 Podman volume 中，重启 Host 后仍可恢复。

停止 Demo Host：

```bash
bash scripts/wsl-demo-down.sh
```

面向服务器的容器启动脚本是 `scripts/server-demo-up.sh`；默认入口为
`http://192.168.1.71:8090`。它默认使用
`~/.config/traceable-search-server-demo-v5` 和
`traceable-search-server-demo-data-v5`，以避免复用 Clarification schema v4 / Research Trace schema v5 数据；旧目录和卷会被保留。
仅在明确指定一个新的空目录/卷时才覆盖 `TRACEABLE_SERVER_RUNTIME_DIR` 或 `DEMO_DATA_VOLUME`。
