use actix_web::ResponseError;
use actix_web::http::StatusCode;
use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum ProxyError {
    #[error("batcher unavailable")]
    BatcherUnavailable,

    #[error("service shutting down")]
    ServiceShutdown,

    #[error("upstream {code}: {body}")]
    Upstream { code: u16, body: String },

    #[error("request error: {0}")]
    Request(String),

    #[error("embedding count mismatch: expected {expected}, got {got}")]
    CountMismatch { expected: usize, got: usize },

    #[error("proxy receiver error: {0}")]
    Receiver(#[from] tokio::sync::oneshot::error::RecvError),
}

impl ResponseError for ProxyError {
    fn status_code(&self) -> StatusCode {
        match self {
            ProxyError::BatcherUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ProxyError::ServiceShutdown => StatusCode::SERVICE_UNAVAILABLE,
            ProxyError::Upstream { code, .. } => StatusCode::from_u16(*code).unwrap_or(StatusCode::BAD_GATEWAY),
            ProxyError::Request(_) => StatusCode::BAD_GATEWAY,
            ProxyError::CountMismatch { .. } => StatusCode::BAD_GATEWAY,
            ProxyError::Receiver(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl From<reqwest::Error> for ProxyError {
    fn from(e: reqwest::Error) -> Self {
        ProxyError::Request(e.to_string())
    }
}
