use rmcp::{ServiceExt, transport::stdio};
use traceable_search::SearchServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    SearchServer::from_env()?
        .serve(stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
