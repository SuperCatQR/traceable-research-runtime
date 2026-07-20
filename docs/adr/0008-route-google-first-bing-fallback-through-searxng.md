---
status: superseded
date: 2026-07-17
superseded-by: 0010-embed-snapshot-extraction-and-keep-search-at-a-seam
---

# Route Google-first Bing fallback through SearXNG

Each web query is executed by `SearxngSearchClient` as an explicit single-engine request. The adapter first sends `engines=google`. A successful Google response, including a contract-proven empty result, completes the query. Only a typed Google unavailable outcome sends a second request with `engines=bing`; Bing results replace rather than merge with Google results. Contract rejection stops the query because fallback must not hide a configuration, response-schema, or engine-selection violation.

The adapter owns this policy because it can validate the external response and return one complete `WebSearchExecution` to the Research Run. Moving the decision into the Run would spread SearXNG response semantics across orchestration, Trace, tests, and deployment. Configuring SearXNG aggregation order was rejected because one aggregated response cannot prove the attempt order or why fallback occurred. Direct Google pages, direct Bing pages, and Bing RSS were rejected because they bypass the controlled SearXNG boundary.

Every engine is attempted at most once per query with a 15-second search timeout. `results` and `unresponsive_engines` are required response fields. Non-empty results must prove the requested engine before URL filtering or truncation. Empty results are accepted only when the request selected exactly one engine and that engine is not reported unresponsive. Trace v7 records each typed attempt, the optional Google-to-Bing fallback, the selected result engine, and the Research Run stop reason without recording raw response bodies.

Deployment is stricter than runtime empty-result handling: a reusable pre-deployment probe must obtain at least one valid result from forced Google and forced Bing requests. Either probe failure blocks image replacement and preserves the running Demo. This proves that the deployed SearXNG instance honors explicit routing; it does not introduce a third fallback or bypass network access controls.
