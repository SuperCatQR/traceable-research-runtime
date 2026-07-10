# Web Search MCP Server — PoC 阶段 0

架构 B（Web Search）的最小可跑实现。挂到现成 agent 客户端（Claude Desktop、Cline 等），手测"公网搜索 + 网页抓取"提供的素材能否支撑高质量、可溯源的专业回答。

> 只验素材质量与答案提升，不含逐字取证/审计回放（阶段 1 再补，见 `../../docs/validation-poc.md`）。

## 工具

对应架构 B 的 Source 契约 `list_candidates / open / read`：

| 工具 | 作用 | 关键返回 |
| --- | --- | --- |
| `search_candidates(query, k)` | 搜索，返回有界候选网页（仅导航，非证据） | `candidate_id` / title / url / snippet |
| `open_source(candidate_id)` | 抓取正文、固化快照、算哈希 | `source_ref` / `content_hash` / char_len |
| `read_source(source_ref, max_chars)` | 读取已固化快照正文 | text / `content_hash` |

约束：`open_source` 只接受本会话 `search_candidates` 产生的 `candidate_id`；阻断内网/环回地址与非 HTTP(S)；快照落 `snapshots/`（已 gitignore）。网页内容视为不可信数据。

## 安装

```
pip install -r requirements.txt
```

## 本机运行

```
python server.py
```

stdio 传输，通常由客户端拉起，无需手动常驻。

## 挂到 Claude Desktop

编辑 `claude_desktop_config.json`（Windows：`%APPDATA%\Claude\claude_desktop_config.json`）：

```json
{
  "mcpServers": {
    "web-search-source": {
      "command": "C:\\Users\\ChosenEcho\\Desktop\\GenericAgent-Desktop-Windows-Portable\\runtime\\python\\python.exe",
      "args": ["C:\\WorkSpace\\project\\research\\poc\\search_mcp\\server.py"]
    }
  }
}
```

重启客户端即可见三个工具。Cline 等同理，填同样的 command 与 args。

## 建议测试提示词

约束模型走固定链路，便于对比：

> 回答前，先用 search_candidates 检索，从候选中选权威来源用 open_source 打开，再用 read_source 读取正文，最后只依据读到的原文作答并标注 source_ref。

对照组：同一批问题让模型直接凭内置知识回答，比较事实准确度、是否敢给出处、有无幻觉。
