// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Typed API errors: `{"error": {"code": "...", "message": "..."}}`.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// Machine-readable error body.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable `snake_case` identifier, safe to match on.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

/// Envelope returned for every non-2xx response.
#[derive(Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// The error.
    pub error: ErrorBody,
}

/// An error with its HTTP status.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Stable error code.
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

impl ApiError {
    /// Build an error.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400.
    #[must_use]
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// 401.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required",
        )
    }

    /// 403.
    #[must_use]
    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    /// 404.
    #[must_use]
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// 409.
    #[must_use]
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 422.
    #[must_use]
    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    /// 500.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// Body for embedding in job records.
    #[must_use]
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code.to_owned(),
            message: self.message.clone(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            }),
        )
            .into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        let code = match r {
            JsonRejection::MissingJsonContentType(_) => "unsupported_media_type",
            JsonRejection::BytesRejection(_) => "body_too_large",
            _ => "invalid_json",
        };
        Self::new(r.status(), code, r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        Self::new(r.status(), "invalid_query", r.body_text())
    }
}

impl From<helia_core::Error> for ApiError {
    fn from(e: helia_core::Error) -> Self {
        use helia_core::Error as E;
        match e {
            E::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
                Self::not_found("not_found", "file not found")
            }
            E::Io(_) | E::Tiff(_) | E::Backend(_) => Self::internal(e.to_string()),
            E::Cancelled => Self::conflict("cancelled", "operation cancelled"),
            E::Unsupported(m) => Self::unprocessable("unsupported", m),
            E::Ffmpeg(f) => {
                use helia_core::io::ffmpeg::FfmpegError as F;
                match f {
                    F::NotFound { .. } | F::TooOld { .. } => Self::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "ffmpeg_unavailable",
                        f.to_string(),
                    ),
                    F::DiskSpace { .. } => Self::new(
                        StatusCode::INSUFFICIENT_STORAGE,
                        "insufficient_storage",
                        f.to_string(),
                    ),
                    F::Truncated(_) => Self::unprocessable("truncated_video", f.to_string()),
                    F::Failed { .. } => Self::internal(f.to_string()),
                    F::Probe(_) | F::NoVideoStream | F::Unsupported(_) => {
                        Self::unprocessable("invalid_capture", f.to_string())
                    }
                }
            }
            E::Ser(_) | E::Fits(_) | E::Avi(_) | E::Invalid(_) => {
                Self::unprocessable("invalid_capture", e.to_string())
            }
        }
    }
}

/// Result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;
