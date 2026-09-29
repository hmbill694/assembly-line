//! What travels over the daemon's socket, and the routes that answer it.

use super::{Serving, submit};
use crate::runner::Runner;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
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

/// A job for the daemon: a new one, or with `job`, another round of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub remote_url: String,
    /// The ref a new job starts from. A revise names none: it starts from
    /// its job's own base.
    pub base_ref: Option<String>,
    pub job: Option<u64>,
    pub prompt: String,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    pub job: u64,
    pub branch: String,
    /// Settings worth flagging that did not stop the job.
    pub warnings: Vec<String>,
}

impl std::fmt::Display for Queued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job {} queued on {}", self.job, self.branch)
    }
}

pub fn router<R: Runner + Send + Sync + 'static>(serving: Arc<Serving<R>>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/jobs", post(submit_job::<R>))
        .with_state(serving)
}

async fn health() -> Json<Health> {
    Json(Health {
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn submit_job<R: Runner + Send + Sync + 'static>(
    State(serving): State<Arc<Serving<R>>>,
    Json(submission): Json<Submission>,
) -> Result<Json<Queued>, (StatusCode, Json<Refused>)> {
    submit::accept(&serving, submission)
        .await
        .map(Json)
        .map_err(|refused| (StatusCode::UNPROCESSABLE_ENTITY, Json(refused)))
}
