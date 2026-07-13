# traceable-search 容器交付开发步骤

> 状态：已实施并验证
>
> 日期：2026-07-12
>
> 目标：将 localhost WebUI 主程序打包为 `linux/amd64` OCI 镜像；SearXNG、crawl4ai 与 OpenAI-compatible model 仍由用户独立部署。

## 1. 交付边界

```text
Browser
  │ http://127.0.0.1:8787
  ▼
Podman host network
  │ WEB_BIND=127.0.0.1:8787
  ▼
traceable-search container
  ├── WebUI / HTTP API / SSE
  ├── ResearchSession
  ├── /data/snapshots.sqlite
  └── /data/traces/*.jsonl
       │
       ├── SearXNG（外部）
       ├── crawl4ai 0.9.1（外部）
       └── OpenAI-compatible model（外部）
```

本次只打包 WebUI 主程序，不把三个外部服务塞入同一镜像，不新增 compose 编排。

约束：

- 目标平台仅 `linux/amd64`；
- 使用 Podman 构建与运行；
- 镜像默认监听 `0.0.0.0:8787`；当前 WSL host-network 部署覆写为 `127.0.0.1:8787`；
- runtime 使用非 root 用户；
- `/data` 必须挂载持久卷；
- `.env`、token、API key、SQLite、trace 不得进入镜像；
- 当前 WSL 部署使用 Podman host network，经 `127.0.0.1` 访问宿主上的 SearXNG 与 crawl4ai；
- WebUI 单任务、状态不跨进程重启之限制保持不变。

## 2. P0：固定容器化前基线

1. 运行：

   ```bash
   cargo fmt --check
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   cargo build --release --locked
   git diff --check
   ```

2. 本地启动并确认：

   ```text
   GET  /                            → 200
   POST /api/research 空问题         → 400
   GET  /api/research/unknown        → 404
   ```

3. 确认 WebUI 默认仅监听：

   ```text
   127.0.0.1:8787
   ```

完成条件：源码运行基线全绿；容器化不得改变研究、API、SQLite 或 trace 语义。

## 3. P1：监听地址配置化

修改 `src/main.rs`，从环境读取 `WEB_BIND`：

```rust
let bind = std::env::var("WEB_BIND")
    .unwrap_or_else(|_| "127.0.0.1:8787".into());
let listener = tokio::net::TcpListener::bind(&bind).await?;
```

行为：

- 源码运行默认：`127.0.0.1:8787`；
- 容器运行覆写：`0.0.0.0:8787`；
- 非法地址直接启动失败，不静默回退；
- 启动日志输出实际 bind 地址至 stderr。

更新 `.env.example`：

```env
WEB_BIND=127.0.0.1:8787
```

测试：

- 未设置时使用 localhost 默认值；
- 设置时正确覆盖；
- 不改变其他环境变量契约。

完成条件：Windows 本地行为不变，容器可通过端口映射访问。

## 4. P2：新增 Containerfile

仓库根目录新增 `Containerfile`，采用 multi-stage build。

### 4.1 Build stage

```Dockerfile
FROM docker.io/library/rust:1.96-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked
```

要求：

- Rust 版本与当前验证环境一致；
- 使用 `--locked`；
- 不复制 `.env`、`data`、docs 或无关文件；
- 首版不为 Cargo layer cache 增加空项目脚手架，保持 Containerfile 直白。

### 4.2 Runtime stage

```Dockerfile
FROM docker.io/library/debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home app \
 && mkdir -p /data \
 && chown app:app /data
COPY --from=build /app/target/release/traceable-search /usr/local/bin/traceable-search
USER app
ENV WEB_BIND=0.0.0.0:8787
ENV TRACEABLE_SEARCH_DATA_DIR=/data
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/traceable-search"]
```

要求：

- runtime 不含 Rust toolchain；
- 安装 HTTPS 所需 CA 与 OpenSSL runtime；
- UID 固定为 `10001`；
- 默认数据目录为 `/data`；
- 镜像中不设置任何 token 或 API key；
- stdout/stderr 只用于服务日志，不写密钥与网页正文。

完成条件：镜像仅含运行时必要文件，进程 UID 非 0。

## 5. P3：限制构建上下文

新增 `.dockerignore`：

```text
.git
.gitignore
target
data
.env
.codegraph
e2e-output
reasonix.toml
1.md
```

检查：

- `.env` 不进入 build context；
- `data/snapshots.sqlite` 与 trace 不进入镜像；
- Windows `target/release/*.exe` 不进入 Linux build；
- 未跟踪本地文件不进入镜像。

完成条件：构建日志显示 context 体积合理；镜像内无敏感或本地运行数据。

## 6. P4：构建 linux/amd64 镜像

执行：

```bash
podman build \
  --platform linux/amd64 \
  --tag localhost/traceable-search:0.1.0 \
  --file Containerfile \
  .
```

检查元数据：

```bash
podman image inspect localhost/traceable-search:0.1.0
```

必须确认：

- architecture：`amd64`；
- OS：`linux`；
- entrypoint：`/usr/local/bin/traceable-search`；
- `WEB_BIND=0.0.0.0:8787`；
- `TRACEABLE_SEARCH_DATA_DIR=/data`；
- 无 `CRAWL4AI_TOKEN`；
- 无 `STRONG_MODEL_API_KEY`。

检查镜像内容：

```bash
podman run --rm --entrypoint /bin/sh localhost/traceable-search:0.1.0 -c '
  id -u
  test ! -e /app/.env
  test -x /usr/local/bin/traceable-search
  test -w /data
'
```

预期 UID：

```text
10001
```

完成条件：镜像构建成功，安全元数据与文件检查通过。

## 7. P5：准备运行配置

宿主新建：

```text
~/.config/traceable-search/web.env
```

内容：

```env
WEB_BIND=127.0.0.1:8787
SEARCH_BASE_URL=http://127.0.0.1:8888/
CRAWL4AI_BASE_URL=http://127.0.0.1:11235/
CRAWL4AI_TOKEN=<crawl4ai token>
STRONG_MODEL_BASE_URL=https://api.deepseek.com
STRONG_MODEL_API_KEY=<model API key>
STRONG_MODEL_ID=deepseek-v4-pro
TRACEABLE_SEARCH_DATA_DIR=/data
```

权限：

```bash
chmod 600 ~/.config/traceable-search/web.env
```

说明：

- 当前 WSL Podman bridge 无法访问仅绑定 WSL loopback 的 SearXNG/crawl4ai，故主容器使用 host network；
- host network 下 `127.0.0.1` 指向 WSL 网络命名空间，可访问上述宿主服务；
- 若外部服务改为可从 bridge 访问，再改用隔离网络与容器 DNS 名；
- env file 不提交仓库。

完成条件：env file 权限为 `0600`，地址可从容器访问。

## 8. P6：启动容器

准备持久目录：

```bash
mkdir -p data
```

启动：

```bash
podman run -d \
  --name traceable-search \
  --platform linux/amd64 \
  --network host \
  --env-file ~/.config/traceable-search/web.env \
  --volume "$PWD/data:/data:Z" \
  localhost/traceable-search:0.1.0
```

检查：

```bash
podman ps --filter name=traceable-search
podman logs --tail 50 traceable-search
curl -fsS http://127.0.0.1:8787/
```

安全要求：

- 必须使用：

  ```text
  127.0.0.1:8787:8787
  ```

- 禁止使用：

  ```text
  0.0.0.0:8787:8787
  8787:8787
  ```

后两者可能将无认证 WebUI 暴露至局域网或公网。

完成条件：容器健康运行，宿主 localhost 可访问，其他主机不可访问。

## 9. P7：HTTP smoke test

### 9.1 首页

```bash
curl -i http://127.0.0.1:8787/
```

预期：

```text
HTTP 200
页面含 Traceable Research
```

### 9.2 空问题

```bash
curl -i -X POST http://127.0.0.1:8787/api/research \
  -H 'Content-Type: application/json' \
  -d '{"question":"  "}'
```

预期：

```text
HTTP 400
```

### 9.3 未知任务

```bash
curl -i http://127.0.0.1:8787/api/research/unknown
```

预期：

```text
HTTP 404
```

### 9.4 单任务约束

首个任务运行时再次提交，预期：

```text
HTTP 409
```

完成条件：四项 HTTP 行为与源码运行一致。

## 10. P8：真实研究 E2E

提交：

```bash
curl -fsS -X POST http://127.0.0.1:8787/api/research \
  -H 'Content-Type: application/json' \
  -d '{"question":"Rust 2024 edition 有哪些主要变化？"}'
```

取得 `run_id` 后监听：

```bash
curl -N http://127.0.0.1:8787/api/research/<run_id>/events
```

查询结果：

```bash
curl -fsS http://127.0.0.1:8787/api/research/<run_id>
```

确认：

- SSE 含 `run_header/query/search_result/archive/.../answer` 或 `run_failed`；
- 最终状态为 `completed`；
- 返回 `answer + claims + sources[url,title]`；
- 响应不含 `snapshot_ref`；
- `data/snapshots.sqlite` 已生成或更新；
- `data/traces/<run_id>.jsonl` 已生成；
- 来源 URL/title 与最终引用对应。

完成条件：容器内完成一次真实研究。

## 11. P9：持久化与重建验证

1. 记录当前 SQLite 与 trace 文件；
2. 删除容器：

   ```bash
   podman rm -f traceable-search
   ```

3. 使用同一 `/data` volume 重建容器；
4. 确认旧文件仍存在且可读；
5. 再运行一次研究，确认可继续写入。

完成条件：容器生命周期不影响审计与快照数据。

## 12. P10：文档与交付

更新 `README.md`：

- `WEB_BIND`；
- Containerfile 构建命令；
- env file；
- WSL Podman host network；
- `WEB_BIND=127.0.0.1:8787`；
- `/data` 持久化；
- 查看日志、停止、升级与重建命令；
- 外部服务仍由用户独立部署。

最终检查：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
git diff --check

podman build --platform linux/amd64 \
  -t localhost/traceable-search:0.1.0 \
  -f Containerfile .
```

交付物：

```text
Containerfile
.dockerignore
.env.example
README.md
docs/develop.md
localhost/traceable-search:0.1.0
```

## 13. 预期文件变更

新增：

```text
Containerfile
.dockerignore
```

修改：

```text
src/main.rs
.env.example
README.md
docs/develop.md
```

不改研究语义：

```text
src/app.rs
src/web.rs
src/orchestration.rs
src/adapters.rs
src/backend.rs
src/snapshot.rs
src/trace.rs
src/types.rs
src/error.rs
```

## 14. 风险与升级边界

- 首次 Linux build 会下载并编译全部 Rust 依赖，耗时较长；后续由 Podman layer cache 缓解。
- `native-tls` 在 runtime 依赖 `libssl3` 与 CA；镜像基础版本变化时须重新验证。
- bind mount 的 UID/SELinux 权限可能阻止 `/data` 写入；rootless Podman 使用 `:Z`，必要时调整宿主目录 owner。
- host network 降低网络隔离；因 WebUI 无认证，必须保持 `WEB_BIND=127.0.0.1:8787`，不可监听 `0.0.0.0`。
- 无认证 WebUI 只因宿主绑定 localhost 才安全；改变 publish 地址前必须先加认证、CSRF、TLS 与限流。

<!-- ponytail: 首版仅 linux/amd64、单容器、单进程；需要 registry 分发、多架构、供应链证明时，再增加 buildx/podman manifest、SBOM、镜像签名与漏洞扫描。 -->
