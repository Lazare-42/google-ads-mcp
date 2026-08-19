use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("authentication error: {0}")]
    Auth(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("Google Ads API returned {status}: {body}")]
    Api {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
