# traceable-search

可审计的 Web 研究服务：先经 Intake 反复澄清并由用户确认完整 Brief，再经 Bing 搜索、crawl4ai 抓取并锁定网页快照，最后由 OpenAI-compatible 强模型生成带来源 URL 与标题的答案。服务在 `http://127.0.0.1:8787/` 提供 WebUI；确认前不启动研究，内部以 `snapshot_ref` 与内容哈希完成校验和审计。

## 架构

```text
Browser ──HTTP/SSE── traceable-search WebUI
                       ├── Intake：创建、澄清、确认或取消
                       ├── HTTP ── SearXNG ── Bing
                       ├── HTTP ── crawl4ai
                       ├── HTTP ── upstream model
                       └── data/
                           ├── snapshots.sqlite
                           ├── intake/<clarification_id>.jsonl
                           └── traces/<run_id>.jsonl
```

详见 [`docs/web-search-architecture.md`](docs/web-search-architecture.md)。

## 前置条件

- Podman 可运行的 Linux 环境（Windows 建议使用 WSL2；本文以 Ubuntu 24.04 为例）
- Rust toolchain
- Linux 构建依赖：`pkg-config` 与 OpenSSL 开发包（Ubuntu 24.04：`sudo apt install pkg-config libssl-dev`）
- `curl`（服务连通性验证）
- Python 3（仅供下述部署命令生成随机密钥及校验 JSON；`traceable-search` 本身不依赖 Python）

构建并测试：

```bash
cargo build --release
cargo test
```

## 外部服务

本项目不部署或管理外部服务。宿主须能访问 Bing、待抓取的公开网页及上游模型端点。请自行准备：

- SearXNG：自托管 JSON Search API；
- crawl4ai `0.9.1`：可访问的 `/crawl` API 及 bearer token（若启用认证）；
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

未带 token 返回 `401` 说明认证已启用；最终仍应使用带 token 的 `POST /crawl` 完成真实抓取验证。

推荐使用 `deepseek-v4-pro`，已验证普通调用可用。当前项目不发送思考模式参数；思考行为由上游模型决定，项目仅解析最终 `content`。

> [!CAUTION]
> 勿将任何 token 或 API key 提交至仓库、写入镜像或公开日志。

## 配置 traceable-search

仓库维护无凭据模板 `.env.example`；真实 `.env` 已由 `.gitignore` 排除。复制模板并填写依赖：

```bash
cp .env.example .env
```

本程序读取进程环境，不会自动解析 `.env`。启动前加载：

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
| `WEB_BIND` | 否 | WebUI 监听地址；默认 `127.0.0.1:8787`；容器内用 `0.0.0.0:8787` |
| `SEARCH_BASE_URL` | 是 | SearXNG 基础 URL；保留尾部 `/` |
| `CRAWL4AI_BASE_URL` | 是 | crawl4ai 基础 URL；保留尾部 `/` |
| `CRAWL4AI_TOKEN` | 否 | crawl4ai bearer token；服务启用认证时填写 |
| `STRONG_MODEL_BASE_URL` | 是 | 上游模型的 OpenAI-compatible API 基础 URL |
| `STRONG_MODEL_API_KEY` | 是 | 上游模型签发的 API key |
| `STRONG_MODEL_ID` | 是 | 上游模型名；建议 `deepseek-v4-pro` |
| `TRACEABLE_SEARCH_DATA_DIR` | 否 | 快照、`intake/` 与 `traces/` 目录；默认 `data`；运行用户须有写权限 |

> [!IMPORTANT]
> 基础 URL 应保留尾部 `/`。程序分别拼接 `search`、`crawl` 与 `chat/completions`。

## 启动 WebUI

本程序读取进程环境，不自动解析 `.env`。加载配置后启动：

```bash
set -a
source .env
set +a
cargo run --release
```

浏览器访问：

```text
http://127.0.0.1:8787/
```

默认仅监听 localhost；一次仅运行一个研究任务。WebUI 依次调用四个 Intake 命令端点：

```text
POST /api/research/intakes
POST /api/research/intakes/{clarification_id}/reply
POST /api/research/intakes/{clarification_id}/confirm
POST /api/research/intakes/{clarification_id}/cancel
```

创建后先回答互斥问题或编辑 Brief；即使问题已清晰，也必须预览并确认。只有携当前 `revision` 与 `content_hash` 的 confirm 才会分配 `run_id` 并启动研究；旧版本返回 409，WebUI 保留尚未提交的输入。`INTAKE_FAILED` 可经 reply 重试或生成最小 Brief；cancel 进入不可恢复终态且不创建 run。各命令按 `clarification_id` 从 `data/intake/` 惰性回放，故进程重启后可用原 ID 重试 reply、confirm 或 cancel。

确认时可选择 3–5 轮查询，默认 3 轮；达到 1,000,000 token 输入预算或 300 份快照时仍会提前收敛。页面经 SSE 展示 query、搜索、归档、选源与作答进度。

## Podman 容器

仅 WebUI 主程序进入镜像；SearXNG、crawl4ai 与模型仍须独立部署。构建固定目标：

```bash
podman build --platform linux/amd64 \
  --tag localhost/traceable-search:0.1.0 \
  --file Containerfile .
```

准备权限受限的运行配置：

```bash
install -d -m 700 ~/.config/traceable-search
umask 077
cat > ~/.config/traceable-search/web.env <<'EOF'
WEB_BIND=127.0.0.1:8787
SEARCH_BASE_URL=http://127.0.0.1:8888/
CRAWL4AI_BASE_URL=http://127.0.0.1:11235/
CRAWL4AI_TOKEN=<token>
STRONG_MODEL_BASE_URL=https://api.deepseek.com/
STRONG_MODEL_API_KEY=<api-key>
STRONG_MODEL_ID=deepseek-v4-pro
TRACEABLE_SEARCH_DATA_DIR=/data
EOF
chmod 600 ~/.config/traceable-search/web.env
mkdir -p data
```

此 WSL 部署使用 Podman host network，因 bridge 容器通常不能访问仅绑定 WSL loopback 的外部服务。启动：

```bash
podman run -d \
  --name traceable-search \
  --platform linux/amd64 \
  --network host \
  --env-file ~/.config/traceable-search/web.env \
  --volume "$PWD/data:/data:Z" \
  localhost/traceable-search:0.1.0
```

> [!CAUTION]
> WebUI 无认证。host network 下必须保持 `WEB_BIND=127.0.0.1:8787`；勿改为 `0.0.0.0:8787`。密钥不得写入镜像或提交仓库。

检查、停止：

```bash
podman logs --tail 50 traceable-search
curl -fsS http://127.0.0.1:8787/
podman stop traceable-search
```

升级时构建新 tag，删除旧容器，再以同一 env file 与 `/data` bind mount 启动；快照和 trace 因此保留。本机开发无需镜像，可继续直接运行 release binary。

## 返回格式

WebUI/API 返回来源 URL 与标题，不暴露内部 `snapshot_ref`：

```json
{
  "answer": "grounded answer",
  "claims": [
    {
      "text": "verifiable claim",
      "sources": [
        {"url": "https://example.com/page", "title": "Example"}
      ]
    }
  ]
}
```

## 验证

先验证 SearXNG、crawl4ai 与 OpenAI-compatible API 可达。一次成功研究后，本项目应生成：

```text
data/snapshots.sqlite
data/intake/<clarification_id>.jsonl
data/traces/<run_id>.jsonl
```

`data/intake/` 记录创建、澄清、确认或取消的 append-only 历史；`data/traces/` 仅在确认后产生。删除或截断任一 JSONL 都会破坏审计与重启恢复，勿以此处理失败会话。

常见故障：

| 现象 | 检查 |
|---|---|
| SearXNG 返回空结果 | 检查 Bing engine、`unresponsive_engines` 与 `SEARCH_BASE_URL` |
| crawl4ai 返回认证失败 | `CRAWL4AI_TOKEN` 是否为该服务签发的有效 token |
| 模型返回 401 | `STRONG_MODEL_API_KEY` 是否有效 |
| 模型端点 404 | `STRONG_MODEL_BASE_URL` 是否指向 OpenAI-compatible `/v1/` API |
| 模型不可用 | `STRONG_MODEL_ID` 是否与上游暴露名称完全一致；建议 `deepseek-v4-pro` |
| 无法配置思考模式 | 当前版本未发送思考参数；请在上游模型侧配置 |
