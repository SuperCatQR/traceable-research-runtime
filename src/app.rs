use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::Utc;
use serde::Serialize;

use crate::{
    Claim, CrawlClient, ErrorClass, LiveBackend, PipelineStage, RunHeader, SearchError,
    SearxngClient, SnapshotRef, SnapshotWriter, StrongClient, TracePolicy, TraceWriter,
    orchestration::{
        AnswerSource, MAX_SNAPSHOTS, MAX_STRONG_INPUT_TOKENS, ResearchResult, ResearchSession,
    },
};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct AppConfig {
    search_base_url: String,
    crawl_base_url: String,
    crawl_token: String,
    model_base_url: String,
    model_api_key: String,
    model: String,
    pub data_dir: PathBuf,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            search_base_url: required_env("SEARCH_BASE_URL")?,
            crawl_base_url: required_env("CRAWL4AI_BASE_URL")?,
            crawl_token: env::var("CRAWL4AI_TOKEN").unwrap_or_default(),
            model_base_url: required_env("STRONG_MODEL_BASE_URL")?,
            model_api_key: required_env("STRONG_MODEL_API_KEY")?,
            model: required_env("STRONG_MODEL_ID")?,
            data_dir: env::var_os("TRACEABLE_SEARCH_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data")),
        })
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    env::var(name).map_err(|_| anyhow::anyhow!("required environment variable {name} is not set"))
}

#[derive(Clone)]
pub struct ResearchService {
    config: AppConfig,
}

impl ResearchService {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub fn new_run_id(&self) -> String {
        format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
            std::process::id(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    }

    pub fn trace_path(&self, run_id: &str) -> PathBuf {
        self.config
            .data_dir
            .join("traces")
            .join(format!("{run_id}.jsonl"))
    }

    pub async fn run(
        &self,
        question: &str,
        rounds: u32,
        run_id: String,
    ) -> Result<PublicAnswer, SearchError> {
        let store_path = self.config.data_dir.join("snapshots.sqlite");
        let backend = LiveBackend::new(
            SearxngClient::new(&self.config.search_base_url).map_err(setup_error)?,
            CrawlClient::new(&self.config.crawl_base_url, self.config.crawl_token.clone())
                .map_err(setup_error)?,
            StrongClient::new(
                &self.config.model_base_url,
                self.config.model_api_key.clone(),
                self.config.model.clone(),
            )
            .map_err(setup_error)?,
        );
        let snapshots = SnapshotWriter::open(&store_path).map_err(setup_error)?;
        let trace = TraceWriter::create(
            self.config.data_dir.join("traces"),
            RunHeader {
                run_id,
                question: question.to_owned(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
            },
        )
        .map_err(setup_error)?;
        let mut session = ResearchSession::new(question, rounds, backend, snapshots, trace);
        public_answer(session.run(store_path).await?)
    }
}

fn setup_error(error: impl std::fmt::Display) -> SearchError {
    SearchError::Setup {
        message: error.to_string(),
    }
}

#[derive(Debug, Serialize)]
pub struct PublicAnswer {
    pub answer: String,
    pub claims: Vec<PublicClaim>,
}
#[derive(Debug, Serialize)]
pub struct PublicClaim {
    pub text: String,
    pub sources: Vec<PublicSource>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicSource {
    pub url: String,
    pub title: String,
}

fn public_answer(result: ResearchResult) -> Result<PublicAnswer, SearchError> {
    let sources: HashMap<SnapshotRef, PublicSource> = result
        .sources
        .into_iter()
        .map(|source: AnswerSource| {
            (
                source.snapshot_ref,
                PublicSource {
                    url: source.url,
                    title: source.title,
                },
            )
        })
        .collect();
    let claims = result
        .answer
        .claims
        .into_iter()
        .map(
            |Claim {
                 text,
                 snapshot_refs,
             }| {
                let sources = snapshot_refs
                    .into_iter()
                    .map(|reference| {
                        sources.get(&reference).cloned().ok_or_else(|| {
                            SearchError::InvalidSnapshot(format!(
                                "cited snapshot missing source metadata: {}",
                                reference.as_str()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(PublicClaim { text, sources })
            },
        )
        .collect::<Result<Vec<_>, SearchError>>()?;
    Ok(PublicAnswer {
        answer: result.answer.answer,
        claims,
    })
}

#[derive(Debug, Serialize)]
pub struct PublicError {
    pub error_class: ErrorClass,
    pub stage: PipelineStage,
    pub message: String,
}

impl From<&SearchError> for PublicError {
    fn from(error: &SearchError) -> Self {
        Self {
            error_class: error.error_class(),
            stage: error.stage().unwrap_or(PipelineStage::Setup),
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Answer, SnapshotRef};

    #[test]
    fn public_answer_hides_snapshot_refs() {
        let reference = SnapshotRef::from_id("abc123");
        let value = serde_json::to_value(
            public_answer(ResearchResult {
                answer: Answer {
                    answer: "Grounded".into(),
                    claims: vec![Claim {
                        text: "Fact".into(),
                        snapshot_refs: vec![reference.clone()],
                    }],
                },
                sources: vec![AnswerSource {
                    snapshot_ref: reference,
                    url: "https://example.com/final".into(),
                    title: "Example".into(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["claims"][0]["sources"][0]["title"], "Example");
        assert!(!value.to_string().contains("snapshot_ref"));
    }
}
