//! Production composition of the three external adapters.

use serde_json::{Value, json};

use crate::{
    BingClient, CrawlClient, Excerpt, Result, SearchResult, Snapshot, StrongClient,
    orchestration::ResearchBackend,
};

pub const PLAN_PROMPT: &str = r#"Return JSON only: {"queries":[{"query":"at most 12 words","gap":"why it is needed"}, ...]}. Produce exactly 3 unique, non-empty queries that address missing evidence. Do not repeat previous_queries."#;

pub const SELECT_PROMPT: &str = r#"Return JSON only: {"selected":[{"snapshot_ref":"snapshot:web/...","reason":"why this source is useful"}, ...]}. Select only snapshot_ref values present in excerpts. Prefer direct, diverse, authoritative evidence."#;

pub const SYNTHESIZE_PROMPT: &str = r#"Return JSON only: {"answer":"grounded answer","claims":[{"text":"verifiable claim","snapshot_refs":["snapshot:web/..."]}, ...]}. Every claim must cite at least one snapshot_ref present in snapshots. Do not cite absent sources."#;

pub struct LiveBackend {
    search: BingClient,
    crawl: CrawlClient,
    strong: StrongClient,
}

impl LiveBackend {
    pub fn new(search: BingClient, crawl: CrawlClient, strong: StrongClient) -> Self {
        Self {
            search,
            crawl,
            strong,
        }
    }

    async fn model_json(&self, system: &str, user: Value) -> Result<String> {
        let value: Value = self.strong.complete_json(system, &user.to_string()).await?;
        Ok(value.to_string())
    }
}

impl ResearchBackend for LiveBackend {
    async fn plan(
        &mut self,
        question: &str,
        archived: &[Snapshot],
        previous_queries: &[String],
    ) -> Result<String> {
        self.model_json(
            PLAN_PROMPT,
            json!({
                "question": question,
                "archived": archived,
                "previous_queries": previous_queries,
            }),
        )
        .await
    }

    async fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        self.search.search(query).await
    }

    async fn crawl(&mut self, url: &str) -> Result<Snapshot> {
        self.crawl.crawl(url).await
    }

    async fn select(&mut self, question: &str, excerpts: &[Excerpt]) -> Result<String> {
        self.model_json(
            SELECT_PROMPT,
            json!({"question": question, "excerpts": excerpts}),
        )
        .await
    }

    async fn synthesize(&mut self, question: &str, snapshots: &[Snapshot]) -> Result<String> {
        self.model_json(
            SYNTHESIZE_PROMPT,
            json!({"question": question, "snapshots": snapshots}),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_prompt_names_exactly_the_required_selection_fields() {
        assert!(SELECT_PROMPT.contains("snapshot_ref"));
        assert!(SELECT_PROMPT.contains("reason"));
        assert!(!SELECT_PROMPT.contains("relevance"));
    }
}
