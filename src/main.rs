use traceable_search::{AppConfig, ResearchService, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;
    std::fs::create_dir_all(&config.data_dir)?;
    let bind = std::env::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("traceable-search listening on http://{bind}");
    axum::serve(listener, router(ResearchService::new(config))).await?;
    Ok(())
}
