//! API error envelope.
//!
//! Handlers return `Result<_, AppError>`; axum renders
//! `{ "error": { "code": "...", "message": "..." } }` with the status mapped
//! from the code. `internal` never leaks its cause: the cause is logged with
//! `tracing::error!` and the client sees a generic message.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

/// Stable machine-readable error codes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Validation,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    AgentUnreachable,
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::AgentUnreachable => "agent_unreachable",
            Self::Internal => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Validation => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::AgentUnreachable => StatusCode::BAD_GATEWAY,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Handler error with a code, safe message and status.
#[derive(Debug)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub status: StatusCode,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let status = code.status();
        Self {
            code,
            message: message.into(),
            status,
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::RateLimited, message)
    }

    pub fn agent_unreachable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AgentUnreachable, message)
    }

    pub fn internal() -> Self {
        Self::new(ErrorCode::Internal, "internal error")
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "database error");
        Self::internal()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = %e, "internal error");
        Self::internal()
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": {
                "code": self.code.as_str(),
                "message": self.message,
            }
        });
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_map_to_expected_status() {
        let cases = [
            (ErrorCode::Validation, 400u16),
            (ErrorCode::Unauthorized, 401),
            (ErrorCode::Forbidden, 403),
            (ErrorCode::NotFound, 404),
            (ErrorCode::Conflict, 409),
            (ErrorCode::RateLimited, 429),
            (ErrorCode::AgentUnreachable, 502),
            (ErrorCode::Internal, 500),
        ];
        for (code, status) in cases {
            let err = AppError::new(code.clone(), "msg");
            assert_eq!(err.status.as_u16(), status, "code {code:?}");
            assert_eq!(err.code, code);
        }
    }

    #[test]
    fn internal_error_never_leaks_detail() {
        let inner = anyhow::anyhow!("super-secret-db-password=hunter2");
        let err: AppError = inner.into();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(
            !err.message.contains("hunter2"),
            "internal message must not leak: {}",
            err.message
        );
    }

    #[test]
    fn sqlx_error_maps_to_internal_without_leak() {
        // A sqlx error carrying potentially sensitive text must still map to
        // a generic internal message.
        let inner = sqlx::Error::Protocol("secret-token-abc".to_string());
        let err: AppError = inner.into();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(!err.message.contains("secret-token-abc"));
    }
}
