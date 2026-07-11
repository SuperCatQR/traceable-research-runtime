//! Thin MCP surface: one complete research run per tool call.

use std::{
    env,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::Utc;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResult, ListPromptsResult,
        ListResourcesResult, PaginatedRequestParams, Prompt, PromptArgument, PromptMessage,
        ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, Role,
        ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    BingClient, CrawlClient, LiveBackend, RunHeader, SnapshotReader, SnapshotWriter, StrongClient,
    TracePolicy, TraceWriter,
    orchestration::{EXPLORE_ROUNDS, MAX_SNAPSHOTS, MAX_STRONG_INPUT_TOKENS, ResearchSession},
};

static RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PROMPT_NAME: &str = "research_question";
const ARCHITECTURE_URI: &str = "traceable-search://architecture";
const ARCHITECTURE_TEXT: &str = "One research_web tool performs one bounded research run: Bing discovery, SSRF-guarded Crawl4AI retrieval, SQLite snapshots, JSONL audit trace, and citation-grounded synthesis.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResearchRequest {
    /// The question to investigate on the public web.
    pub question: String,
}

#[derive(Debug, Clone)]
struct ServerConfig {
    crawl_base_url: String,
    crawl_token: String,
    model_base_url: String,
    model_api_key: String,
    model: String,
    data_dir: PathBuf,
}

impl ServerConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
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

#[derive(Debug, Clone)]
pub struct SearchServer {
    config: ServerConfig,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl SearchServer {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            config: ServerConfig::from_env()?,
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        name = "research_web",
        description = "Research a question on the public web and return an answer whose claims cite immutable snapshot references."
    )]
    async fn research_web(
        &self,
        Parameters(request): Parameters<ResearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let question = request.question.trim();
        if question.is_empty() {
            return Err(McpError::invalid_params("question must not be empty", None));
        }
        let answer = self
            .run_research(question)
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let content = ContentBlock::json(answer)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }
}

impl SearchServer {
    async fn run_research(&self, question: &str) -> anyhow::Result<crate::Answer> {
        let run_id = format!(
            "{}-{}-{}",
            Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
            std::process::id(),
            RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let store_path = self.config.data_dir.join("snapshots.sqlite");
        let trace_dir = self.config.data_dir.join("traces");
        let backend = LiveBackend::new(
            BingClient::new()?,
            CrawlClient::new(&self.config.crawl_base_url, self.config.crawl_token.clone())?,
            StrongClient::new(
                &self.config.model_base_url,
                self.config.model_api_key.clone(),
                self.config.model.clone(),
            )?,
        );
        let snapshots = SnapshotWriter::open(&store_path)?;
        let trace = TraceWriter::create(
            trace_dir,
            RunHeader {
                run_id,
                question: question.to_owned(),
                started_at: Utc::now(),
                policy: TracePolicy {
                    rounds: EXPLORE_ROUNDS,
                    input_budget: MAX_STRONG_INPUT_TOKENS as u32,
                    max_snapshots: MAX_SNAPSHOTS as u32,
                },
            },
        )?;
        let mut session = ResearchSession::new(question, backend, snapshots, trace);
        session.explore().await?;
        let reader = SnapshotReader::open(store_path)?;
        Ok(session.synthesize_answer(reader).await?)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for SearchServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();
        info
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            PROMPT_NAME,
            Some("Turn a question into a traceable public-web research request."),
            Some(vec![PromptArgument::new("question").with_required(true)]),
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != PROMPT_NAME {
            return Err(McpError::invalid_params("unknown prompt", None));
        }
        let question = request
            .arguments
            .and_then(|arguments| arguments.get("question").cloned())
            .and_then(|value| value.as_str().map(str::to_owned))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| McpError::invalid_params("question is required", None))?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Use research_web to investigate this question and return the cited answer: {question}"
            ),
        )]))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(ARCHITECTURE_URI, "architecture")
                .with_description("Runtime research pipeline and audit boundary.")
                .with_mime_type("text/plain"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if request.uri != ARCHITECTURE_URI {
            return Err(McpError::invalid_params("unknown resource", None));
        }
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            ARCHITECTURE_TEXT,
            ARCHITECTURE_URI,
        )]))
    }
}

#[cfg(test)]
mod tests {
    use super::ResearchRequest;

    #[test]
    fn request_schema_requires_question() {
        let schema = schemars::schema_for!(ResearchRequest);
        let value = serde_json::to_value(schema).unwrap();
        assert_eq!(value["required"], serde_json::json!(["question"]));
    }
}
