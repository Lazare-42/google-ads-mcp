use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error(
        "Google Ads API returned {status} (request ID: {request_id}){}",
        detail_suffix(.detail)
    )]
    Api {
        status: reqwest::StatusCode,
        request_id: String,
        /// Google Ads error codes and messages only; never the raw response body.
        detail: Option<String>,
    },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

fn detail_suffix(detail: &Option<String>) -> String {
    detail
        .as_deref()
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default()
}

pub type Result<T> = std::result::Result<T, Error>;
