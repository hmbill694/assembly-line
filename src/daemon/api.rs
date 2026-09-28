//! What travels over the daemon's socket, and the routes that answer it.

use super::Daemon;
use crate::runner::Runner;
use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub version: String,
}

/// Why the daemon would not do what it was asked, with every reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub summary: String,
    pub reasons: Vec<String>,
}

pub fn router<R: Runner + Send + Sync + 'static>(daemon: Arc<Daemon<R>>) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(daemon)
}

async fn health() -> Json<Health> {
    Json(Health {
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
