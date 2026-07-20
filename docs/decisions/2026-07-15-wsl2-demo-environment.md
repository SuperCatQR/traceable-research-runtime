# WSL2 完整 Demo 环境

日期：2026-07-15
状态：已实施；2026-07-20 更新为 Brave Search API 与进程内 Snapshot 提取

## 原因

用户需要在本地 WSL2 中运行真实自然对话校准、搜索、抓取、快照与答案链路，并在浏览器中管理多个可恢复研究对话、账户与模型配置。

## 现状

- WSL2：Ubuntu 24.04，Podman 可用。
- Brave Search API 作为外部搜索依赖；正文抓取与 Markdown 提取已内收至 Rust 进程。
- WSL 内 DNS 返回真实公网 IP，可满足 Rust SSRF guard；Windows 侧曾出现 `198.18.0.0/15` 合成地址。
- 上游模型由用户保存的 OpenAI-compatible Model Profile 提供；本机不部署大型模型。`.env`
  只为本地宿主和兼容性构造提供基础变量，不把用户 Profile 密钥带入浏览器。
- 前端 production build 已生成，但须由同源宿主提供，避免 CORS 与凭据进入浏览器。

## 决定

新增独立 `demo-host/` crate：

- 以 path dependency 调用根 crate `traceable-search`，不修改其源码或依赖。
- 使用 Axum 暴露经过登录与所有权校验的工作区 API 和 `/api/health`；Turn 通过普通消息推进，
  模型决定后自动执行研究。
- 同源托管 `web/dist`，浏览器只访问宿主的 `8080` 端口。
- 模型产生 `start_research` 后，Host 立即返回 pending Turn，并在后台任务中依次准备和执行；浏览器
  只轮询 `ready | running` 状态，不发送确认或手动执行命令。Host 重启后由恢复协调器继续未终态 Run。
- API 仅绑定 localhost；浏览器使用 HttpOnly、SameSite=Strict 的登录 Cookie，SQLite catalogue 负责账户、登录会话、模型配置与研究对话所有权。
- 服务配置从容器 env file 注入；用户模型 key 以 AES-256-GCM 加密后写入 catalogue，主密钥首次部署时生成到权限为 `0600` 的 WSL 配置目录。模型 key、crawl token 与主密钥不写入镜像、前端或日志。
- 研究数据写入独立 Podman volume，保留 JSONL trace、SQLite snapshots 与 demo catalogue。主密钥必须跨重启保持不变，否则既有模型 key 无法解密。Conversation schema v2、Clarification schema v5 不读取旧日志，Trace schema v7 只读取带 envelope 的 v7 事件；升级到本契约时必须使用 v6 存储代际的新数据卷和运行目录，旧 v5 存储保留。
- WSL 本地部署显式允许私网模型端点，以支持同机网关；其他部署仍保持默认拒绝策略。

## 进程布局

```text
Windows browser -> http://127.0.0.1:8080
                      |
                      +-- demo-host (Axum + authenticated static workspace)
                            |-- SQLite catalogue (accounts, login sessions, profiles, conversations)
                            |-- traceable-search library (JSONL trace and snapshots)
                            |-- Brave Search API (HTTPS)
                            |-- embedded Snapshot extraction
                            `-- user-selected OpenAI-compatible model endpoint
```

## 验收

- WSL2 中 demo-host 通过服务端 API key 访问 Brave Search API。
- `GET /api/health` 返回 `ok`。
- 浏览器可注册账户、保存加密模型配置、新建并恢复多个研究对话。
- 重新启动 demo-host 后，登录 Cookie、对话列表、已完成答案和待继续的自然对话仍可恢复。
- 聊天正文只显示自然对话、状态、答案和来源；右侧栏按需显示研究概览与分页审计详情。
- 两个账户不能读取或修改彼此的对话或模型配置。
