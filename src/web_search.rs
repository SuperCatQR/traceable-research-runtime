//! Search seam shared by production and deterministic tests.

use std::future::Future;

use crate::{BraveSearchClient, WebSearchExecution};

pub trait WebSearch: Send + Sync {
    fn search_web(&self, query: &str) -> impl Future<Output = WebSearchExecution> + Send;
}

impl WebSearch for BraveSearchClient {
    async fn search_web(&self, query: &str) -> WebSearchExecution {
        BraveSearchClient::search_web(self, query).await
    }
}

#[cfg(test)]
pub(crate) struct FixtureWebSearch {
    execution: WebSearchExecution,
}

#[cfg(test)]
impl FixtureWebSearch {
    pub(crate) fn new(execution: WebSearchExecution) -> Self {
        Self { execution }
    }
}

#[cfg(test)]
impl WebSearch for FixtureWebSearch {
    async fn search_web(&self, _query: &str) -> WebSearchExecution {
        self.execution.clone()
    }
}
