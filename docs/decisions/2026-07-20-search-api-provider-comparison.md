# Search API Provider Comparison

Date: 2026-07-20

## Recommendation

Use **Brave Search API** as the primary provider and keep **Tavily** as an optional
fallback. Both are direct provider APIs, so the application no longer depends on
scraping Google/Bing through SearXNG and does not inherit their CAPTCHA state.

Brave is the better default for this runtime because its web search response is
already close to the existing `SearchResult` shape: title, URL, and description.
The API also accepts language and country parameters, which matters for the
current Chinese/English query flow.

## Provider Comparison

| Provider | Official API shape | Fit for this project | Main tradeoff |
| --- | --- | --- | --- |
| Brave Search API | `GET /res/v1/web/search`, `X-Subscription-Token`; web results expose title, URL, and description | Best primary provider; independent web index, language/country controls, simple result mapping | Must check current quota and plan limits before production rollout |
| Tavily | `POST /search`, bearer API key; results expose title, URL, content, score, and optional raw content | Best AI-research fallback; richer evidence snippets and domain filters | More opinionated search/research semantics; response/content quotas need monitoring |
| Exa | Search API with neural/keyword modes and optional contents/highlights | Strong second provider for research-oriented retrieval and highlights | Semantic ranking differs from normal web search; requires tuning query and result limits |
| Serper | Google Search JSON API with API key and organic result URLs/snippets | Small adapter and familiar Google-style result shape | Still depends on a Google-results proxy and its availability/ranking behavior |
| Microsoft Bing Web Search API | First-party Azure Bing Search API | Not a new choice for this deployment | Microsoft documentation records the Bing Search APIs retirement, so it should not be selected for a new integration |

Official sources:

- Brave API documentation: <https://api.search.brave.com/app/documentation/>
- Brave API pricing: <https://brave.com/search/api/>
- Tavily API reference: <https://docs.tavily.com/>
- Tavily pricing: <https://tavily.com/pricing>
- Exa API documentation: <https://exa.ai/docs/reference/search>
- Exa pricing: <https://exa.ai/pricing>
- Serper API site: <https://serper.dev/>
- Microsoft Bing Search API retirement notice and documentation: <https://learn.microsoft.com/en-us/bing/search-apis/bing-web-search/overview>

Prices, quotas, and free credits change frequently. They should be read from the
linked provider pages when creating the account rather than frozen in application
documentation.

## Integration Plan

The current runtime already has a generic `WebSearch` seam, but the concrete
client and trace model are named around Google/Bing. The safe migration is:

1. Add a `BraveSearchClient` using a server-side `BRAVE_SEARCH_API_KEY`.
2. Map Brave `web.results[]` to the existing title/URL/snippet result contract.
3. Record `brave` as the search provider in Trace rather than pretending it is
   Google or Bing.
4. Keep `validate_public_web_url` and the existing Snapshot/SSRF path unchanged;
   a provider result is still untrusted input.
5. Add bounded provider retry/backoff for transport timeout, 429, and 5xx, then
   use Tavily only as a provider-level fallback if configured.
6. Add provider configuration through environment variables and never expose the
   provider key to React or browser storage.

Suggested deployment variables:

```text
SEARCH_PROVIDER=brave
BRAVE_SEARCH_API_KEY=<server-side secret>
TAVILY_API_KEY=<optional server-side fallback secret>
```

The old SearXNG adapter and container are removed; the production path is now
independent of search-engine scraping and CAPTCHA suspension.
