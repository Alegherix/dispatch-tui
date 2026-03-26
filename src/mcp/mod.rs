pub mod handlers;

use std::sync::Arc;

use axum::{Router, routing::post};

use crate::db;

pub struct McpState {
    pub db: Arc<dyn db::TaskStore>,
}

pub fn router(db: Arc<dyn db::TaskStore>) -> Router {
    let state = Arc::new(McpState { db });
    Router::new()
        .route("/mcp", post(handlers::handle_mcp))
        .with_state(state)
}

pub async fn serve(db: Arc<dyn db::TaskStore>, port: u16) -> anyhow::Result<()> {
    let app = router(db);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
