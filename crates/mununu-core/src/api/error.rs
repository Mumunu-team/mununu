use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::fmt;

/// Error detail for API responses
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Standard error response
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: ErrorDetail,
}

/// API error types
#[derive(Debug)]
pub enum ApiError {
    /// Bad request (400)
    BadRequest {
        message: String,
        details: Option<String>,
    },
    /// Internal server error (500)
    Internal {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Not found (404)
    NotFound { resource: String },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::BadRequest { message, .. } => write!(f, "Bad request: {}", message),
            ApiError::Internal { message, .. } => write!(f, "Internal error: {}", message),
            ApiError::NotFound { resource } => write!(f, "Resource not found: {}", resource),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApiError::Internal { source, .. } => source.as_ref().map(|e| e.as_ref() as _),
            _ => None,
        }
    }
}

impl ApiError {
    /// The HTTP status and the body every transport reports this error with: the HTTP response
    /// is `(status, body)`, and the MCP tool result's text and `structuredContent` are the same
    /// `body` (mununu MCP, 2026-11 — one error shape, two surfaces).
    pub fn response_body(&self) -> (StatusCode, ErrorResponse) {
        let (status, code, message, details) = match self {
            ApiError::BadRequest { message, details } => (
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                message.clone(),
                details.clone(),
            ),
            ApiError::Internal {
                message, source, ..
            } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                message.clone(),
                source.as_ref().map(|s| s.to_string()),
            ),
            ApiError::NotFound { resource } => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                format!("Resource not found: {}", resource),
                None,
            ),
        };
        (
            status,
            ErrorResponse {
                success: false,
                error: ErrorDetail {
                    code: code.to_string(),
                    message,
                    details,
                },
            },
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_response) = self.response_body();
        (status, Json(error_response)).into_response()
    }
}

/// API result type
pub type ApiResult<T> = Result<T, ApiError>;

/// Convert string errors to ApiError
impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        ApiError::Internal {
            message: msg,
            source: None,
        }
    }
}

/// Convert &str errors to ApiError
impl From<&str> for ApiError {
    fn from(msg: &str) -> Self {
        ApiError::Internal {
            message: msg.to_string(),
            source: None,
        }
    }
}

/// Helper to convert core errors to API errors
pub fn convert_error(err: impl std::error::Error + Send + Sync + 'static) -> ApiError {
    ApiError::Internal {
        message: err.to_string(),
        source: Some(Box::new(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn test_api_error_display() {
        let err = ApiError::BadRequest {
            message: "Invalid input".to_string(),
            details: None,
        };
        assert!(err.to_string().contains("Invalid input"));

        let err = ApiError::NotFound {
            resource: "user".to_string(),
        };
        assert!(err.to_string().contains("user"));
    }

    #[test]
    fn test_api_error_from_string() {
        let err: ApiError = "test error".to_string().into();
        match err {
            ApiError::Internal { message, .. } => assert_eq!(message, "test error"),
            _ => panic!("Expected Internal error"),
        }
    }

    #[test]
    fn test_api_error_from_str() {
        let err: ApiError = "test error".into();
        match err {
            ApiError::Internal { message, .. } => assert_eq!(message, "test error"),
            _ => panic!("Expected Internal error"),
        }
    }

    #[test]
    fn test_convert_error() {
        let std_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let api_err = convert_error(std_err);
        match api_err {
            ApiError::Internal { message, source } => {
                assert!(message.contains("file not found"));
                assert!(source.is_some());
            }
            _ => panic!("Expected Internal error"),
        }
    }

    #[tokio::test]
    async fn test_bad_request_response() {
        let err = ApiError::BadRequest {
            message: "Invalid request".to_string(),
            details: Some("Missing required field".to_string()),
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_not_found_response() {
        let err = ApiError::NotFound {
            resource: "user".to_string(),
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_internal_error_response() {
        let err = ApiError::Internal {
            message: "Internal error".to_string(),
            source: None,
        };
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
