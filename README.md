# traceable-search

可审计的 Web 研究 MCP 服务：经 Bing 搜索、crawl4ai 抓取并锁定网页快照，再由 OpenAI-compatible 强模型生成带 `snapshot_ref` 的答案。服务通过 MCP `stdio` 暴露 `research_web` 工具。

## 架构

```text
MCP client ──stdio── traceable-search
                       ├── Python ddgs ── Bing
                       ├── HTTP ── crawl4ai
                       ├── HTTP ── upstream model
                       └── data/
                           ├── snapshots.sqlite
                           └── traces/*.jsonl
```

详见 [`docs/web-search-architecture.md`](docs/web-search-architecture.md)。

## 前置条件

- Podman 可运行的 Linux 环境（Windows 建议使用 WSL2；本文以 Ubuntu 24.04 为例）
- Rust toolchain
- Python 3.12 与 `pip`

安装搜索依赖并构建：

```bash
python -m venv .venv
source .venv/bin/activate
pip install -r requirements-search.txt
cargo build --release
cargo test
```

> [!IMPORTANT]
> 运行 `traceable-search` 时，`PATH` 中的 `python` 必须能导入 `ddgs`。

## 外部服务

本项目不部署或管理外部服务。请自行准备：

- crawl4ai `0.9.1`：可访问的 `/crawl` API 及 bearer token（若启用认证）；
- 上游模型：OpenAI-compatible `/v1/chat/completions` API、API key 与模型名。

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
```

变量说明：

| 变量 | 必需 | 含义 |
|---|---:|---|
| `CRAWL4AI_BASE_URL` | 是 | crawl4ai 基础 URL；保留尾部 `/` |
| `CRAWL4AI_TOKEN` | 否 | crawl4ai bearer token；服务启用认证时填写 |
| `STRONG_MODEL_BASE_URL` | 是 | 上游模型的 OpenAI-compatible API 基础 URL |
| `STRONG_MODEL_API_KEY` | 是 | 上游模型签发的 API key |
| `STRONG_MODEL_ID` | 是 | 上游模型名；建议 `deepseek-v4-pro` |
| `TRACEABLE_SEARCH_DATA_DIR` | 否 | 快照与 trace 目录；默认 `data` |

> [!IMPORTANT]
> 基础 URL 应保留尾部 `/`。程序分别拼接 `crawl` 与 `chat/completions`。

## 接入 MCP client

本项目是 `stdio` MCP server，通常由 MCP client 直接启动。通用配置示例：

```json
{
  "mcpServers": {
    "traceable-search": {
      "command": "/absolute/path/target/release/traceable-search",
      "args": [],
      "env": {
        "PATH": "/absolute/path/.venv/bin:/usr/local/bin:/usr/bin",
        "CRAWL4AI_BASE_URL": "http://127.0.0.1:11235/",
        "CRAWL4AI_TOKEN": "<crawl4ai token>",
        "STRONG_MODEL_BASE_URL": "http://127.0.0.1:3000/v1/",
        "STRONG_MODEL_API_KEY": "<上游模型 API key>",
        "STRONG_MODEL_ID": "<上游模型名>",
        "TRACEABLE_SEARCH_DATA_DIR": "/absolute/path/data"
      }
    }
  }
}
```

若 MCP client 运行于 Windows、binary 运行于 WSL，可令客户端通过 WSL 启动；具体配置依客户端对命令与参数的格式而异：

```text
command: wsl.exe
args: ["--distribution", "Ubuntu-24.04", "--exec", "/absolute/wsl/path/target/release/traceable-search"]
```

## 验证

先依外部服务文档验证 crawl4ai 与 OpenAI-compatible API 可达。一次成功研究后，本项目应生成：

```text
data/snapshots.sqlite
data/traces/<run_id>.jsonl
```

常见故障：

| 现象 | 检查 |
|---|---|
| `failed to start ddgs` | `PATH` 中是否存在 `python` |
| `No module named 'ddgs'` | 是否在该 Python 环境执行 `pip install -r requirements-search.txt` |
| crawl4ai 返回认证失败 | `CRAWL4AI_TOKEN` 是否为该服务签发的有效 token |
| 模型返回 401 | `STRONG_MODEL_API_KEY` 是否有效 |
| 模型端点 404 | `STRONG_MODEL_BASE_URL` 是否指向 OpenAI-compatible `/v1/` API |
| 模型不可用 | `STRONG_MODEL_ID` 是否与上游暴露名称完全一致；建议 `deepseek-v4-pro` |
| 无法配置思考模式 | 当前版本未发送思考参数；请在上游模型侧配置 |
