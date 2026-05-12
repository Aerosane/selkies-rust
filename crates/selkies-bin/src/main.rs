// TODO(port): __main__.py — entry point, config, lifecycle (938 lines)

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(true)
        .with_thread_ids(true)
        .init();

    tracing::info!("selkies-rust: not yet implemented — Phase 7+ required");
    Ok(())
}
