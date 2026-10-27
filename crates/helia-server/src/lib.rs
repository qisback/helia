// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! HTTP/JSON API for Helia, embeddable in the CLI and the GUI.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let server = helia_server::Server::builder()
//!     .data_root("/data/captures")
//!     .bind("127.0.0.1:7870".parse()?)
//!     .build()?;
//! let bound = server.bind().await?;
//! println!("listening on {}", bound.local_addr());
//! bound.serve(std::future::pending()).await?;
//! # Ok(()) }
//! ```
//!
//! Routes live under `/api/v1`; see the API chapter of the book (`docs/book`). Every client path is
//! confined to the data root ([`paths::Sandbox`]).
#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod imaging;
pub mod jobs;
pub mod openapi;
pub mod paths;
pub mod presets;
mod routes;
mod sequences;

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub use config::{ConfigError, ServerBuilder, ServerConfig};
pub use error::{ApiError, ApiResult};

use jobs::JobManager;
use paths::Sandbox;

/// Shared by all handlers.
#[derive(Clone)]
pub(crate) struct AppState {
    pub sandbox: Arc<Sandbox>,
    pub jobs: JobManager,
    pub token: Option<Arc<str>>,
    pub cfg: Arc<ServerConfig>,
    pub gpu: Arc<tokio::sync::OnceCell<routes::GpuInfo>>,
}

/// A configured, not yet listening server.
pub struct Server {
    state: AppState,
}

impl Server {
    /// Start configuring.
    #[must_use]
    pub fn builder() -> ServerBuilder {
        ServerBuilder::default()
    }

    pub(crate) fn from_config(cfg: ServerConfig) -> Result<Self, ConfigError> {
        let sandbox = Sandbox::new(&cfg.data_root).map_err(|source| ConfigError::DataRoot {
            path: cfg.data_root.clone(),
            source,
        })?;
        Ok(Self {
            state: AppState {
                sandbox: Arc::new(sandbox),
                jobs: JobManager::new(cfg.workers, cfg.max_queue, cfg.max_retained_jobs),
                token: cfg.token.as_deref().map(Arc::from),
                cfg: Arc::new(cfg),
                gpu: Arc::new(tokio::sync::OnceCell::new()),
            },
        })
    }

    /// The canonical data root.
    #[must_use]
    pub fn data_root(&self) -> &std::path::Path {
        self.state.sandbox.root()
    }

    /// The router, for in-process use (tests) or mounting in a larger app.
    /// Job tasks are spawned on the ambient tokio runtime.
    pub fn router(&self) -> Router {
        build_router(&self.state)
    }

    /// Bind the configured address.
    ///
    /// # Errors
    /// The socket cannot be bound.
    pub async fn bind(self) -> std::io::Result<BoundServer> {
        let listener = TcpListener::bind(self.state.cfg.bind).await?;
        Ok(BoundServer {
            router: build_router(&self.state),
            jobs: self.state.jobs,
            listener,
        })
    }
}

/// A server that holds its socket.
pub struct BoundServer {
    router: Router,
    jobs: JobManager,
    listener: TcpListener,
}

impl BoundServer {
    /// Address actually bound (useful with port 0).
    ///
    /// # Panics
    /// Never in practice: a bound listener always has a local address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr().expect("bound listener address")
    }

    /// Serve until `shutdown` completes, then cancel running jobs and drain.
    ///
    /// # Errors
    /// I/O failure in the accept loop.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let jobs = self.jobs;
        axum::serve(self.listener, self.router)
            .with_graceful_shutdown(async move {
                shutdown.await;
                // Open WebSockets only close once their job ends.
                jobs.shutdown();
            })
            .await
    }
}

fn build_router(state: &AppState) -> Router {
    let api = Router::new()
        .route("/health", get(routes::health))
        .route("/version", get(routes::version))
        .route("/presets", get(routes::presets))
        .route("/presets/{id}/lut", get(routes::preset_lut))
        .route("/captures/inspect", axum::routing::post(routes::inspect))
        .route("/captures/frame", get(routes::frame))
        .route(
            "/jobs",
            axum::routing::post(routes::create_job).get(routes::list_jobs),
        )
        .route(
            "/jobs/{id}",
            get(routes::get_job).delete(routes::delete_job),
        )
        .route("/jobs/{id}/result", get(routes::job_result))
        .route("/jobs/{id}/preview.png", get(routes::job_preview))
        .route("/jobs/{id}/sunspots.csv", get(routes::job_sunspots_csv))
        .route("/jobs/{id}/sunspots.png", get(routes::job_sunspots_png))
        .route("/jobs/{id}/report", get(routes::job_report))
        .route("/jobs/{id}/report/{file}", get(routes::job_report_file))
        .route("/jobs/{id}/events", get(routes::job_events))
        .route("/timelapse", axum::routing::post(sequences::timelapse))
        .route("/mosaic", axum::routing::post(sequences::mosaic))
        .route(
            "/solar-derotation",
            axum::routing::post(sequences::solar_derotation),
        )
        .route("/openapi.json", get(routes::openapi_json))
        .route("/docs", get(routes::docs));
    let mut app = Router::new()
        .nest("/api/v1", api)
        .fallback(routes::not_found)
        .method_not_allowed_fallback(routes::method_not_allowed)
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state.clone());
    if !state.cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state
            .cfg
            .cors_origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        // Outermost, so preflights are answered before authentication.
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        );
    }
    app
}

/// Host check (DNS-rebinding defence) and bearer authentication.
async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if st.cfg.bind.ip().is_loopback() {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned)
            .or_else(|| req.uri().authority().map(ToString::to_string));
        if host.is_some_and(|h| !host_allowed(&h, &st.cfg.allowed_hosts)) {
            return ApiError::forbidden(
                "bad_host",
                "the Host header is not a loopback name; see --allow-host",
            )
            .into_response();
        }
    }
    if let Some(expected) = &st.token {
        let path = req.uri().path();
        // The health probe and the static docs page carry no data.
        let public = path == "/api/v1/health" || path == "/api/v1/docs";
        if !public {
            let supplied = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split_once(' '))
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                .map(|(_, t)| t.trim());
            // Length is not a secret; contents are compared in constant time.
            let ok = supplied.is_some_and(|s| bool::from(s.as_bytes().ct_eq(expected.as_bytes())));
            if !ok {
                return ApiError::unauthorized().into_response();
            }
        }
    }
    next.run(req).await
}

fn host_allowed(host: &str, extra: &[String]) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    name == "localhost"
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        || extra.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn hosts() {
        assert!(host_allowed("localhost:7870", &[]));
        assert!(host_allowed("127.0.0.1", &[]));
        assert!(host_allowed("[::1]:80", &[]));
        assert!(!host_allowed("evil.example", &[]));
        assert!(!host_allowed("192.168.1.2:7870", &[]));
        assert!(host_allowed("helia.lan", &["helia.lan".into()]));
    }
}
