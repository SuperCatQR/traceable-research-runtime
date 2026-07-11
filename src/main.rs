//! Binary entry point. MCP server wiring (stdio transport) lands in P5;
//! for now this is the P0 scaffold that proves the async runtime + dep tree build.

#[tokio::main]
async fn main() {
    println!(
        "traceable-search {} — P0 scaffold. MCP server wiring lands in P5.",
        env!("CARGO_PKG_VERSION")
    );
}
