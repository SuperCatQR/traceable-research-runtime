# 待办：SearXNG 搜索改为 Google 优先、Bing 回退

- 状态：已定规则，待按 Trace v7 计划实施
- 提出日期：2026-07-16
- 优先级：高

## 目标行为

搜索应优先使用自托管 SearXNG 中的 Google 引擎；仅当 Google 在当前请求中连接失败或不可用时，
才回退到 Bing。Google 返回零条有效结果不自动触发 Bing 回退。外部搜索流量应保持通过受控的
SearXNG 边界，而不是由应用进程直接调用搜索引擎。

```text
ResearchRunExecutor
  -> SearXNG: Google
  -> Google 连接失败 / 不可用
  -> SearXNG: Bing
  -> 返回最多 10 条有效 HTTP(S) 导航结果
```

搜索标题和 snippet 仍然只用于导航，不能作为最终事实证据。

## 当前实现与目标的差异

当前部署说明将 SearXNG 固定为 Bing；`SearxngSearchClient` 优先请求该实例，失败后会直接
请求 `https://www.bing.com/search?format=rss` 作为进程内兜底。这不满足“Google 优先、
SearXNG 内 Bing 回退”的目标，并引入绕过受控搜索边界的直接 Bing 出网路径。

## 预期影响范围

1. SearXNG 部署配置与运行文档
   - 更新 `README.md`、部署脚本及 SearXNG settings：启用 Google 和 Bing，并明确 Google
     的优先级、Bing 的回退条件、所需代理或限流配置以及合规风险。
   - 验证 Google 引擎在部署环境中的可用性；不得默认以规避 CAPTCHA、登录限制或访问控制
     为目标。

2. `src/external_adapters.rs`
   - 重新定义 `SearxngSearchClient` 的请求契约，使应用层能明确请求 Google、识别
     Google 不可用 / 无有效结果，再请求 Bing。
   - 移除或替换直接 Bing RSS fallback，确保其不会绕过 SearXNG；保留可审计的失败原因。
   - 明确 HTTP 429、空结果、无效 URL、网络失败和引擎响应状态各自是否触发回退或终止。

3. `src/research_run.rs` 与 Trace
   - 为每个查询记录实际使用的搜索引擎、Google 回退原因及最终结果，或通过扩展 schema
     表达等价信息。
   - 保持“每词最多 10 条、跨轮 URL 去重、标题/snippet 仅导航”的既有不变量。

4. 测试与线上验证
    - 覆盖 Google 成功、Google 空结果、Google 限流、Google 网络失败、Bing 成功、两者均
      失败、Trace 回放，以及新旧 Trace 卷拒绝混写但保留旧卷的部署边界。
   - 更新 HTTP 工作区验证和服务器部署检查，确认实际搜索路径符合优先级。

## 已确认的决策

1. 回退条件仅为 Google 的连接或可用性失败；零条有效结果按 Google 的正常结果处理。
2. Bing 是独占回退结果，不与 Google 结果合并。
3. 应用通过 SearXNG 受控边界请求单一引擎；部署前必须验证该实例能否按请求固定引擎。若不能，
   采用两个仍由 SearXNG 托管的受控端点，不在应用进程直接请求 Google/Bing 页面或 RSS。
4. Google 引擎的可用性、速率限制和服务条款是服务器部署验收条件，不以规避 CAPTCHA、登录限制
   或访问控制为目标。
5. 实际搜索引擎、尝试结果和回退原因进入所有者可见的 L3 审计详情；L1/L2 不展示这些过程信息。
   详细实现见 [Trace 边界强化计划](../plans/2026-07-16-trace-boundary-hardening-plan.md)。

## 验收标准（待讨论后细化）

- 正常查询的首选请求仅使用 SearXNG 的 Google 引擎。
- Google 连接失败或不可用时，系统通过 SearXNG 的 Bing 引擎回退；Google 的零结果不触发回退。
- 应用进程不直接请求 Google 或 Bing 搜索页面 / RSS。
- 每次回退均有可回放、可审计的原因和实际引擎记录。
- 两个引擎均不可用时，Research Run 明确以 `search` 阶段失败结束，不制造空白成功结果。
