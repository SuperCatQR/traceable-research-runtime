# Web Search 数据流示例

> 状态：配套示例（对应 [web-search-architecture.md](./web-search-architecture.md)）
>
> 日期：2026-07-11
>
> 目的：用一个具体问题走一遍完整数据流，标出每步的真实字段名与数据形态，帮助理解 §5 的多轮探索与作答。示例问题贯穿全篇：**「2024 年诺贝尔物理学奖颁给了谁？获奖理由是什么？」**

本示例的数据形态用于说明流程，字段值为示意，非真实抓取结果。术语、组件与约束以主文档为准。

## 1. 阶段 0：入口与建档

```
U → run.py:  question = "2024年诺贝尔物理学奖颁给了谁？获奖理由是什么？"
run.py → research_session.py:  ResearchSession(question, policy).run()
```

`research_session.py` 启动先往 `trace/<run_id>.jsonl` 写首行 **run header**。它是原始用户问题的唯一权威副本，不可变，之后每轮生成查询和最终作答都回这里取：

```json
{"type":"run_header","run_id":"r-20260711-a1b2","question":"2024年诺贝尔物理学奖颁给了谁？获奖理由是什么？","started_at":"2026-07-11T10:00:00Z","policy":{"rounds":3,"input_budget":800000,"max_snapshots":300}}
```

## 2. 阶段一：Explore（固定 3 轮）

### 2.1 第 1 轮

**步 1 生成查询。** `research_session.py` 用固定提示词模板装配一次调用，模型不能改模板，只有槽位随轮变化。第 1 轮 `archived_sources` 为空：

```text
[system]
你是查询规划器。只依据用户问题和已归档原文提出后续搜索词，用于公网检索。
硬约束：
- 恰好输出 3 个查询词，每个不超过 12 个词。
- 每个查询针对一个尚未被已归档原文覆盖的证据缺口；不得重复 previous_queries。
- 只依据给定材料，不臆造事实、专有名词或时间。
- 只返回符合下方 schema 的 JSON，不输出任何解释性文字。

[user]
question: 2024年诺贝尔物理学奖颁给了谁？获奖理由是什么？   # 取自 run header
round: 1
previous_queries: []          # 首轮没有历史
archived_sources:             # 首轮为空
```

strong 按 schema 返回，每条查询都带 `gap`（要补的证据缺口）：

```json
{"queries":[
  {"query":"2024 诺贝尔物理学奖 得主","gap":"尚不知得主姓名"},
  {"query":"2024 Nobel Prize Physics winner","gap":"缺英文权威源交叉印证"},
  {"query":"2024 诺贝尔物理奖 获奖理由","gap":"尚不知官方获奖理由"}
]}
```

程序按主文档 §6 校验 1 强制校验这段 JSON：schema 合法、恰好 3 条、单条长度有界、与 `previous_queries` 去重。不合格即拒绝并要求重出。合格后 `gap` 一并写入 trace 的 query 行（即 §7 风险 1 的 query rationale，可回放追责）：

```json
{"type":"query","round":1,"query":"2024 诺贝尔物理学奖 得主","gap":"尚不知得主姓名"}
```

**步 2 搜索第一页。** 每词打 Bing 取前 10 条，只留导航字段，跨词、跨轮按规范化 URL 去重。假设三词去重后剩 24 条候选：

```json
{"search_result_id":"sr-01","title":"2024年诺贝尔物理学奖授予Hopfield和Hinton","url":"https://example-news.com/nobel2024","snippet":"…机器学习奠基…","rank":1}
```

**步 3 全量抓取。** 24 条全部进 `archive_page`，无模型预选。每条的处理：校验 URL 公网可达 → 装配默认 config（JS 渲染 + `scan_full_page` 上限，不对抗反爬）交 crawl4ai → 得到 `{success, status_code, final_url, metadata, raw_markdown, fit_markdown}` → 对 `final_url` 再校验防重定向越界 → 本地判成败与质量。分两条路：

- 有效则 `snapshot.writer.save()` 写 `snapshot.sqlite`，正文写入即不可变：

```json
{"snapshot_ref":"snapshot:web/9f3a1c…","content_hash":"sha256:ab12…","final_url":"https://example-news.com/nobel2024","char_len":8421}
```

- 命中边界（正文在 OSS 的 `.docx`、登录墙、付费墙、Cloudflare challenge 等）时，crawl4ai 即使返回 `success=true` 也只是空壳，本地质量校验不过，于是记 `archive_skip`，不重试、不伪装成功：

```json
{"type":"archive_skip","search_result_id":"sr-07","reason":"body_not_in_dom"}
```

第 1 轮假设 24 条里 19 条归档成功、5 条 `archive_skip`。

### 2.2 第 2 轮：提示词槽位变化，用户问题作锚

**步 1** 用同一个提示词模板，但槽位填满。`question` 仍从 run header 取，一字不变；`previous_queries` 填第 1 轮那 3 词；`archived_sources` 填 19 份已归档原文的标题与摘录：

```text
[user]
question: 2024年诺贝尔物理学奖颁给了谁？获奖理由是什么？   # 还是 run header，锚不动
round: 2
previous_queries: ["2024 诺贝尔物理学奖 得主","2024 Nobel Prize Physics winner","2024 诺贝尔物理奖 获奖理由"]
archived_sources:
  - title: 2024年诺贝尔物理学奖授予Hopfield和Hinton
    excerpt: 标题+首段
  - ...（共 19 条）
```

strong 对照原始问题读这 19 份原文，发现「Hopfield 网络」「玻尔兹曼机」等第 1 轮未覆盖的新主体，产出更深的 3 词；硬约束逼它避开 `previous_queries`：

```json
{"queries":[
  {"query":"Hopfield network 物理原理","gap":"原文提到Hopfield网络但未解释物理机制"},
  {"query":"Boltzmann machine Hinton 贡献","gap":"缺Hinton具体贡献细节"},
  {"query":"诺贝尔物理奖 机器学习 争议","gap":"未见对'物理奖颁给AI'的评价"}
]}
```

`question` 每轮重放不变，正是防止 strong 沿错误方向漂走（§7 风险 1）。步 2、步 3 同第 1 轮，新增快照，跨轮 URL 去重自动跳过已抓过的。

### 2.3 第 3 轮

同上，用同一模板。跑满默认 3 轮，或中途撞到 800k 输入预算、300 份快照、本轮零新 URL 之一则提前收敛。假设累计 42 份成功快照。

## 3. 阶段二：Synthesize（三轮后一次）

1. **程序摘录**：对 42 份快照逐一 `reader.get(snapshot_ref)`，程序确定性截取「标题+首段+URL」，不经模型、不改正文：

```json
{"snapshot_ref":"snapshot:web/9f3a1c…","title":"…Hopfield和Hinton","excerpt":"标题+首段+URL"}
```

2. **选源**（strong）：一次读入全部 42 份 `title + excerpt + snapshot_ref`，返回相关的 `snapshot_ref` 与逐项理由：

```json
{"selected":[{"snapshot_ref":"snapshot:web/9f3a1c…","relevance":"high","reason":"官方公告，含得主与理由"}]}
```

3. **校验归属**：程序确认每个 `snapshot_ref` 属本 run，且原文预算够。
4. **读原文**：`reader.get` 读出选中的不可变原文与 `content_hash`。
5. **作答**（strong）：输入为原始问题（再次从 run header 取）加选中原文，返回答案及每条 Claim 携带的 `snapshot_ref`：

```json
{"answer":"2024年诺贝尔物理学奖授予 John Hopfield 与 Geoffrey Hinton，表彰其在人工神经网络机器学习方面的奠基性发现…",
 "claims":[{"text":"得主为 Hopfield 与 Hinton","snapshot_refs":["snapshot:web/9f3a1c…"]}]}
```

6. **校验有源**：程序只接受引用了已送入本次调用、且哈希匹配的 `snapshot_ref`，随后写 trace 的 `selection`/`claim`/`answer` 行，返回 `ResearchResult` 给 `run.py`，再给用户。若搜索无果、全部 `archive_skip`、无可用原文或 strong 判断原文不足，则据实拒答。

## 4. 提示词块在流程里的作用

- **同一模板每轮复用**：`system` 硬约束不变，只换 `question`、`round`、`previous_queries`、`archived_sources` 四个槽位。
- **question 恒取 run header**：每轮生成查询和最终作答都回同一处取原始问题，是不漂移的锚。
- **强约束输出**：逼 strong 只出 3 条、每条带 `gap`、避免重复、只回 JSON，让程序能用 §6 校验 1 卡关，`gap` 进审计可回放。
