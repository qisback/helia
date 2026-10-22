// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! In-process API tests (`tower::ServiceExt::oneshot`) plus one real WebSocket.
#![allow(clippy::many_single_char_names)] // terse test bindings: f, s, v, h, b

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use futures_util::StreamExt;
use helia_core::testutil::{
    CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
};
use helia_server::{Server, ServerBuilder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A fresh temporary directory whose name cannot read as a filter.
///
/// Helia guesses the filter from the folder a capture is in (and the one above), and the
/// random names `tempfile` makes sometimes spell one ("Ha", "Na", "656"), which changes the
/// recommended look, the Bayer channel and the output. The name here is letters that are in
/// no filter name, unique to the process and the call.
fn scratch() -> tempfile::TempDir {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    const LETTERS: &[u8; 10] = b"qxzkvwjyfp";
    let n = N.fetch_add(1, Ordering::Relaxed);
    let code = |mut v: usize| {
        let mut s = String::new();
        loop {
            s.push(char::from(LETTERS[v % 10]));
            v /= 10;
            if v == 0 {
                return s;
            }
        }
    };
    tempfile::Builder::new()
        .prefix(&format!(
            "zz{}z{}",
            code(std::process::id() as usize),
            code(n)
        ))
        .rand_bytes(0)
        .tempdir()
        .expect("tempdir")
}

fn capture_bytes(w: usize, h: usize, frames: usize) -> Vec<u8> {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: w,
        height: h,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    ser_bytes(&cap.frames)
}

fn small() -> &'static [u8] {
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| capture_bytes(96, 80, 16))
}

/// Big enough that a keep-everything multipoint stack takes a while.
fn slow() -> &'static [u8] {
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| capture_bytes(192, 160, 240))
}

struct Fixture {
    dir: tempfile::TempDir,
    app: Router,
    server: Option<Server>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(|b| b)
    }

    fn with(f: impl FnOnce(ServerBuilder) -> ServerBuilder) -> Self {
        let dir = scratch();
        std::fs::write(dir.path().join("sun.ser"), small()).expect("write");
        let server = f(Server::builder().data_root(dir.path()))
            .build()
            .expect("server");
        Self {
            dir,
            app: server.router(),
            server: Some(server),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write_slow(&self) {
        std::fs::write(self.root().join("slow.ser"), slow()).expect("write");
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .expect("request");
        let res = self.app.clone().oneshot(req).await.expect("response");
        let (parts, body) = res.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes().to_vec();
        (parts.status, parts.headers, bytes)
    }

    async fn json(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (s, _, b) = self.call(method, uri, body, None).await;
        let v = serde_json::from_slice(&b).unwrap_or(Value::Null);
        (s, v)
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.json(Method::GET, uri, None).await
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.json(Method::POST, uri, Some(body)).await
    }

    async fn wait_state(&self, id: u64, want: &str) -> Value {
        let t0 = Instant::now();
        loop {
            let (_, v) = self.get(&format!("/api/v1/jobs/{id}")).await;
            if v["state"] == want {
                return v;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(120),
                "job {id} stuck in {v}, wanted {want}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn job(input: &str, output: &str) -> Value {
    json!({"input": input, "output": output, "config": {"backend": "cpu"}})
}

fn slow_job(output: &str) -> Value {
    json!({"input": "slow.ser", "output": output, "config": {
        "backend": "cpu", "keep": "100%", "ap_size": 16}})
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn health_version_and_error_shape() {
    let f = Fixture::new();
    let (s, v) = f.get("/api/v1/health").await;
    assert_eq!((s, &v["status"]), (StatusCode::OK, &json!("ok")));
    let (s, v) = f.get("/api/v1/version").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["backends"]["cpu"]["available"], true);
    assert!(v["backends"]["gpu"]["available"].is_boolean());
    assert_eq!(v["auth_required"], false);
    assert!(v["build"]["commit"].is_string());
    assert!(v["build"]["dirty"].is_boolean());
    assert!(v["build"]["wgpu"].is_string());
    assert_eq!(v["licence"]["licence"], "GPL-3.0-or-later");
    assert_eq!(
        v["licence"]["copyright"],
        "Copyright (C) 2026 Laurence Horrocks-Barlow"
    );
    let (s, v) = f.get("/api/v1/nothing").await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "no_such_route"));
    let (s, v) = f.json(Method::PUT, "/api/v1/health", None).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
    );
}

#[tokio::test]
async fn openapi_lists_routes_and_docs_is_html() {
    let f = Fixture::new();
    let (s, v) = f.get("/api/v1/openapi.json").await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["openapi"].as_str().is_some_and(|s| s.starts_with("3.")));
    for p in [
        "/api/v1/health",
        "/api/v1/jobs",
        "/api/v1/jobs/{id}",
        "/api/v1/jobs/{id}/events",
        "/api/v1/captures/inspect",
    ] {
        assert!(v["paths"].get(p).is_some(), "missing {p}");
    }
    let (s, h, b) = f.call(Method::GET, "/api/v1/docs", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .expect("ct")
            .starts_with("text/html")
    );
    assert!(String::from_utf8_lossy(&b).contains("Helia API"));
}

#[tokio::test]
async fn auth_is_required_except_health_and_docs() {
    let f = Fixture::with(|b| b.token("s3cret-token"));
    let (s, ..) = f.call(Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, ..) = f.call(Method::GET, "/api/v1/docs", None, None).await;
    assert_eq!(s, StatusCode::OK);
    for uri in [
        "/api/v1/version",
        "/api/v1/jobs",
        "/api/v1/openapi.json",
        "/api/v1/nothing",
    ] {
        let (s, h, b) = f.call(Method::GET, uri, None, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(h[header::WWW_AUTHENTICATE], "Bearer");
        let v: Value = serde_json::from_slice(&b).expect("json");
        assert_eq!(code(&v), "unauthorized");
    }
    for bad in ["wrong", "s3cret-token-", "s3cret-toke", ""] {
        let (s, ..) = f
            .call(Method::GET, "/api/v1/version", None, Some(bad))
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{bad:?}");
    }
    let (s, ..) = f
        .call(Method::GET, "/api/v1/version", None, Some("s3cret-token"))
        .await;
    assert_eq!(s, StatusCode::OK);
    // Wrong scheme with the right secret.
    let req = Request::builder()
        .uri("/api/v1/version")
        .header(header::AUTHORIZATION, "Basic s3cret-token")
        .body(Body::empty())
        .expect("req");
    let res = f.app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn bind_policy() {
    let any: std::net::SocketAddr = "0.0.0.0:0".parse().expect("addr");
    let dir = scratch();
    let b = || Server::builder().data_root(dir.path()).bind(any);
    assert!(b().build().is_err(), "non-loopback without token must fail");
    assert!(b().token("   ").build().is_err(), "blank token must fail");
    assert!(b().token("abc").build().is_ok());
    let v6: std::net::SocketAddr = "[::1]:0".parse().expect("addr");
    assert!(
        Server::builder()
            .data_root(dir.path())
            .bind(v6)
            .build()
            .is_ok()
    );
    assert!(
        Server::builder()
            .data_root(dir.path().join("missing"))
            .build()
            .is_err()
    );
    assert!(
        Server::builder()
            .data_root(dir.path())
            .cors_origin("not an origin")
            .build()
            .is_err()
    );
}

#[tokio::test]
async fn token_file_is_trimmed() {
    let dir = scratch();
    let tf = dir.path().join("tok");
    std::fs::write(&tf, "abc123\n").expect("write");
    let s = Server::builder()
        .data_root(dir.path())
        .token_file(&tf)
        .expect("file")
        .build()
        .expect("server");
    let req = Request::builder()
        .uri("/api/v1/version")
        .header(header::AUTHORIZATION, "bearer abc123")
        .body(Body::empty())
        .expect("req");
    let res = s.router().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn foreign_host_header_is_refused_on_loopback() {
    let f = Fixture::new();
    let req = Request::builder()
        .uri("/api/v1/health")
        .header(header::HOST, "evil.example:7870")
        .body(Body::empty())
        .expect("req");
    let res = f.app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let req = Request::builder()
        .uri("/api/v1/health")
        .header(header::HOST, "localhost:7870")
        .body(Body::empty())
        .expect("req");
    assert_eq!(
        f.app.clone().oneshot(req).await.expect("res").status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn cors_is_off_by_default_and_opt_in() {
    let f = Fixture::new();
    let req = |o: &str| {
        Request::builder()
            .uri("/api/v1/health")
            .header(header::ORIGIN, o)
            .body(Body::empty())
            .expect("req")
    };
    let res = f
        .app
        .clone()
        .oneshot(req("http://a.example"))
        .await
        .expect("res");
    assert!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );

    let f = Fixture::with(|b| b.cors_origin("http://a.example"));
    let res = f
        .app
        .clone()
        .oneshot(req("http://a.example"))
        .await
        .expect("res");
    assert_eq!(
        res.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://a.example"
    );
    let res = f
        .app
        .clone()
        .oneshot(req("http://b.example"))
        .await
        .expect("res");
    assert!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn inspect_reports_header_fields() {
    let f = Fixture::new();
    let (s, v) = f
        .post("/api/v1/captures/inspect", json!({"path": "sun.ser"}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["format"], "ser");
    assert_eq!(
        (v["width"].as_u64(), v["height"].as_u64()),
        (Some(96), Some(80))
    );
    assert_eq!(v["frames"], 16);
    assert_eq!(v["bit_depth"], 16);
    assert_eq!(v["colour"], "mono");
    assert!(v["sidecar"].is_null());

    let (s, v) = f
        .post("/api/v1/captures/inspect", json!({"path": "nope.ser"}))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));

    std::fs::write(f.root().join("notes.txt"), "x").expect("write");
    let (s, v) = f
        .post("/api/v1/captures/inspect", json!({"path": "notes.txt"}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_capture")
    );

    std::fs::write(f.root().join("bad.ser"), b"not a ser file at all").expect("write");
    let (s, v) = f
        .post("/api/v1/captures/inspect", json!({"path": "bad.ser"}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_capture")
    );
}

#[tokio::test]
async fn frame_preview_is_a_png_and_honours_max_width() {
    let f = Fixture::new();
    let (s, _, b) = f
        .call(
            Method::GET,
            "/api/v1/captures/frame?path=sun.ser&index=3",
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&b[..4], &[0x89, b'P', b'N', b'G']);
    assert_eq!(u32::from_be_bytes(b[16..20].try_into().expect("w")), 96);

    let (_, _, b) = f
        .call(
            Method::GET,
            "/api/v1/captures/frame?path=sun.ser&max_width=32&channel=g",
            None,
            None,
        )
        .await;
    assert_eq!(u32::from_be_bytes(b[16..20].try_into().expect("w")), 32);

    let (s, v) = f.get("/api/v1/captures/frame?path=sun.ser&index=99").await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "frame_out_of_range")
    );
    let (s, v) = f
        .get("/api/v1/captures/frame?path=sun.ser&max_width=2")
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "invalid_max_width")
    );
    let (s, v) = f.get("/api/v1/captures/frame?index=1").await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "invalid_query"));
}

// ---- path confinement -----------------------------------------------------

#[tokio::test]
async fn traversal_is_403_everywhere() {
    let f = Fixture::new();
    for p in ["../sun.ser", "sub/../../x.ser", "..\\x.ser", "a/../sun.ser"] {
        let (s, v) = f.post("/api/v1/captures/inspect", json!({"path": p})).await;
        // Backslash is a separator on Windows only; elsewhere it is a name.
        if p.contains('\\') && !cfg!(windows) {
            continue;
        }
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, "path_traversal"),
            "{p}"
        );
        let enc = p.replace('/', "%2F").replace('\\', "%5C");
        let (s, v) = f.get(&format!("/api/v1/captures/frame?path={enc}")).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
        assert_eq!(code(&v), "path_traversal");
        let (s, _) = f.post("/api/v1/jobs", job(p, "o.tif")).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "input {p}");
        let (s, _) = f
            .post("/api/v1/jobs", job("sun.ser", &p.replace(".ser", ".tif")))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "output {p}");
    }
}

#[tokio::test]
async fn absolute_paths_outside_root_are_403_inside_are_fine() {
    let f = Fixture::new();
    let outside = scratch();
    let victim = outside.path().join("secret.ser");
    std::fs::write(&victim, small()).expect("write");
    let abs = victim.to_str().expect("utf8");
    let (s, v) = f
        .post("/api/v1/captures/inspect", json!({"path": abs}))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_outside_root"));
    let (s, _) = f.post("/api/v1/jobs", job(abs, "o.tif")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let out_abs = outside.path().join("o.tif");
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            job("sun.ser", out_abs.to_str().expect("utf8")),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_outside_root"));
    assert!(!out_abs.exists());

    // An absolute path that points back inside the root is accepted.
    let inside = f.root().canonicalize().expect("canon").join("sun.ser");
    let (s, _) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": inside.to_str().expect("utf8")}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}

fn try_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = if target.is_dir() {
        // A junction needs no privilege and is resolved by canonicalize too.
        std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .and_then(|o| {
                    if o.status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other("mklink failed"))
                    }
                })
        })
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    #[cfg(not(any(unix, windows)))]
    let r: std::io::Result<()> = Err(std::io::Error::other("no symlinks"));
    r.is_ok()
}

#[tokio::test]
async fn symlink_escapes_are_403() {
    let f = Fixture::new();
    let outside = scratch();
    let victim = outside.path().join("secret.ser");
    std::fs::write(&victim, small()).expect("write");
    // File link, directory link and a dangling link.
    let file_link = f.root().join("link.ser");
    let dir_link = f.root().join("linkdir");
    let have_dir = try_symlink(outside.path(), &dir_link);
    let have_file = try_symlink(&victim, &file_link);
    if !have_dir && !have_file {
        eprintln!("skipped: cannot create symbolic links here (needs privilege on Windows)");
        return;
    }
    if have_file {
        let (s, v) = f
            .post("/api/v1/captures/inspect", json!({"path": "link.ser"}))
            .await;
        assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_outside_root"));
    } else {
        eprintln!("note: file symlink not available, directory link only");
    }
    if !have_dir {
        return;
    }
    let dangling = f.root().join("dangling.tif");
    let have_dangling = try_symlink(&outside.path().join("not-yet.tif"), &dangling);
    let (s, v) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": "linkdir/secret.ser"}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_outside_root"));
    let (s, _) = f
        .post("/api/v1/jobs", job("linkdir/secret.ser", "o.tif"))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Writing through a directory link or a dangling link must not escape.
    let (s, _) = f
        .post("/api/v1/jobs", job("sun.ser", "linkdir/new.tif"))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!outside.path().join("new.tif").exists());
    if have_dangling {
        let (s, _) = f.post("/api/v1/jobs", job("sun.ser", "dangling.tif")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(!outside.path().join("not-yet.tif").exists());
    }
}

// ---- request hygiene -------------------------------------------------------

#[tokio::test]
async fn oversized_and_malformed_bodies_are_rejected() {
    let f = Fixture::new();
    let big = json!({"input": "sun.ser", "output": "o.tif", "pad": "x".repeat(100_000)});
    let (s, v) = f.post("/api/v1/jobs", big).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")
    );

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/jobs")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("{}"))
        .expect("req");
    let res = f.app.clone().oneshot(req).await.expect("res");
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let (s, v) = f.post("/api/v1/jobs", json!({"input": "sun.ser"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(code(&v), "invalid_json");
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "o.tif", "config": {"nope": 1}}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")
    );
}

#[tokio::test]
async fn job_config_is_validated_before_queueing() {
    let f = Fixture::new();
    let bad = |cfg: Value| json!({"input": "sun.ser", "output": "o.tif", "config": cfg});
    for cfg in [
        json!({"keep": "25%", "keep_frames": 3}),
        json!({"keep_frames": 0}),
        json!({"keep": "banana"}),
        json!({"sigma_clip": -1.0}),
        json!({"robust": "huber", "robust_k": 0.0}),
        json!({"robust": "huber", "sigma_clip": 2.5}),
        json!({"robust_k": 2.0}),
        json!({"ap_size": 2}),
        json!({"colour": "true", "channel": "g"}),
        json!({"colour": "true", "invert": true}),
        json!({"colour": "true", "white_balance": "warm"}),
        json!({"colour": "true", "white_balance": "1,2"}),
        json!({"white_balance": "off"}),
        json!({"align_channels": false}),
        json!({"flatten": "sideways"}),
        json!({"limb_law": "cubic"}),
        json!({"ap_auto": true, "ap_size": 48}),
        json!({"ap_auto": true, "multipoint": false}),
        json!({"ap_passes": 3}),
        json!({"ap_passes": 2, "multipoint": false}),
        json!({"ap_smooth": true, "multipoint": false}),
        json!({"aperture_mm": -5.0}),
        json!({"sunspots": {"umbra": 0.95, "penumbra": 0.9}}),
        json!({"field_rotation": {"reference": "later"}, "site": "51.5,-0.1"}),
        json!({"sunspots": {"penumbra": 1.5}}),
        json!({"sunspots": {"group_gap_deg": -1.0}}),
        json!({"sunspots": {"wolf_k": 0.0}}),
        json!({"autocrop_margin": 10.0}),
        json!({"autocrop": true, "autocrop_margin": -1.0}),
        json!({"autocrop": true, "autocrop_margin": 500.0}),
        json!({"bayer": "xyz"}),
        json!({"preset": "teal"}),
        json!({"colour": "teal"}),
        json!({"colour_strength": 2.0}),
        json!({"align": "fft"}),
    ] {
        let (s, v) = f.post("/api/v1/jobs", bad(cfg.clone())).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config"),
            "{cfg}"
        );
    }
    let (s, v) = f.post("/api/v1/jobs", job("sun.ser", "o.jpg")).await;
    assert_eq!(
        (s, code(&v)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_output_format"
        )
    );
    let (s, v) = f.post("/api/v1/jobs", job("missing.ser", "o.tif")).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));
    std::fs::write(f.root().join("bad.ser"), b"junk").expect("write");
    let (s, v) = f.post("/api/v1/jobs", job("bad.ser", "o.tif")).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_capture")
    );
    assert_eq!(f.get("/api/v1/jobs").await.1, json!([]));
}

#[tokio::test]
async fn existing_output_needs_overwrite() {
    let f = Fixture::new();
    std::fs::write(f.root().join("o.tif"), b"old").expect("write");
    let (s, v) = f.post("/api/v1/jobs", job("sun.ser", "o.tif")).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_exists"));
    let mut body = job("sun.ser", "o.tif");
    body["overwrite"] = json!(true);
    let (s, v) = f.post("/api/v1/jobs", body).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    f.wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert!(
        std::fs::metadata(f.root().join("o.tif"))
            .expect("meta")
            .len()
            > 100
    );
}

// ---- jobs ------------------------------------------------------------------

#[tokio::test]
async fn job_runs_and_result_decodes() {
    let f = Fixture::new();
    let (s, v) = f
        .post("/api/v1/jobs", job("sun.ser", "out/result.fits"))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    assert_eq!(v["input"], "sun.ser");
    assert_eq!(v["output"], "out/result.fits");

    let done = f.wait_state(id, "succeeded").await;
    assert_eq!(done["progress"], 1.0);
    assert_eq!(done["summary"]["frames"], 16);
    assert_eq!(done["summary"]["width"], 96);
    // How whole frames were aligned, with the choice, its reason and a confidence.
    let al = &done["summary"]["alignment"];
    assert_eq!(al["requested"], "auto");
    assert!(
        matches!(al["method"].as_str(), Some("limb" | "phase")),
        "{al}"
    );
    assert!(al["reason"].as_str().is_some_and(|r| !r.is_empty()), "{al}");
    assert!(
        al["confidence"]
            .as_f64()
            .is_some_and(|c| (0.0..=1.0).contains(&c)),
        "{al}"
    );
    assert!(done["timings"].as_array().is_some_and(|t| !t.is_empty()));
    assert!(
        done["backend"]
            .as_str()
            .is_some_and(|b| b.contains("CPU") || b.contains("cpu"))
    );

    let (s, h, bytes) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/result"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/fits");
    let saved: PathBuf = f.root().join("downloaded.fits");
    std::fs::write(&saved, &bytes).expect("write");
    let img = helia_core::io::load_image(&saved).expect("decodes");
    assert_eq!((img.width, img.height), (96, 80));
    assert_eq!(
        bytes,
        std::fs::read(f.root().join("out/result.fits")).expect("read")
    );

    let (s, _, png) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/preview.png?max_width=48"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(&png[1..4], b"PNG");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().expect("w")), 48);

    let (_, list) = f.get("/api/v1/jobs").await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));

    // A finished job is forgotten by DELETE; the file stays.
    let (s, ..) = f
        .call(Method::DELETE, &format!("/api/v1/jobs/{id}"), None, None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        f.get(&format!("/api/v1/jobs/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(f.root().join("out/result.fits").exists());
}

#[tokio::test]
async fn tiff_result_has_tiff_headers() {
    let f = Fixture::new();
    let (_, v) = f.post("/api/v1/jobs", job("sun.ser", "r.tif")).await;
    let id = v["id"].as_u64().expect("id");
    f.wait_state(id, "succeeded").await;
    let (s, h, b) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/result"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/tiff");
    assert!(b.starts_with(b"II*\0") || b.starts_with(b"MM\0*"));
}

#[tokio::test]
async fn unknown_and_unready_jobs() {
    let f = Fixture::new();
    f.write_slow();
    let (s, v) = f.get("/api/v1/jobs/999").await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "job_not_found"));
    let (s, v) = f.get("/api/v1/jobs/abc").await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "invalid_job_id"));

    let (_, v) = f.post("/api/v1/jobs", slow_job("s.tif")).await;
    let id = v["id"].as_u64().expect("id");
    let (s, v) = f.get(&format!("/api/v1/jobs/{id}/result")).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "not_ready"));
    let (s, v) = f.get(&format!("/api/v1/jobs/{id}/preview.png")).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "not_ready"));
    f.json(Method::DELETE, &format!("/api/v1/jobs/{id}"), None)
        .await;
    f.wait_state(id, "cancelled").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn running_job_can_be_cancelled() {
    let f = Fixture::new();
    f.write_slow();
    let (s, v) = f.post("/api/v1/jobs", slow_job("cancelled.tif")).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let id = v["id"].as_u64().expect("id");
    // Wait until the pipeline reports progress, so the flag is what stops it.
    let t0 = Instant::now();
    loop {
        let (_, v) = f.get(&format!("/api/v1/jobs/{id}")).await;
        assert_ne!(
            v["state"], "succeeded",
            "finished before it could be cancelled"
        );
        if v["state"] == "running" && v["progress"].as_f64().is_some_and(|p| p > 0.0) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "never started: {v}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (s, v) = f
        .json(Method::DELETE, &format!("/api/v1/jobs/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let v = f.wait_state(id, "cancelled").await;
    assert!(v["error"].is_null());
    assert!(!f.root().join("cancelled.tif").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_is_fifo_bounded_and_queued_jobs_cancel() {
    let f = Fixture::with(|b| b.max_queue(3));
    f.write_slow();
    let (_, a) = f.post("/api/v1/jobs", slow_job("a.tif")).await;
    let ia = a["id"].as_u64().expect("a");
    // Until `a` holds the worker permit it still counts as queued, which would
    // put `b` at position 1 on a busy machine.
    f.wait_state(ia, "running").await;
    let (_, b) = f.post("/api/v1/jobs", slow_job("b.tif")).await;
    let (_, c) = f.post("/api/v1/jobs", slow_job("c.tif")).await;
    let (ib, ic) = (b["id"].as_u64().expect("b"), c["id"].as_u64().expect("c"));
    assert_eq!(b["state"], "queued");
    assert_eq!(b["queue_position"], 0);
    assert_eq!(c["queue_position"], 1);

    // Limit reached.
    let (s, v) = f.post("/api/v1/jobs", slow_job("d.tif")).await;
    assert_eq!((s, code(&v)), (StatusCode::TOO_MANY_REQUESTS, "queue_full"));
    // Same output as an active job.
    let (s, v) = f.post("/api/v1/jobs", slow_job("b.tif")).await;
    assert!(
        matches!(code(&v), "queue_full" | "output_in_use"),
        "{s} {v}"
    );

    // Only one runs at a time.
    let (_, list) = f.get("/api/v1/jobs").await;
    let running = list
        .as_array()
        .expect("array")
        .iter()
        .filter(|j| j["state"] == "running")
        .count();
    assert!(running <= 1);

    // A queued job cancels at once and frees its slot.
    let (s, v) = f
        .json(Method::DELETE, &format!("/api/v1/jobs/{ib}"), None)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(v["state"], "cancelled");
    assert_eq!(
        f.get(&format!("/api/v1/jobs/{ic}")).await.1["queue_position"],
        0
    );
    let (s, v) = f.post("/api/v1/jobs", slow_job("d.tif")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id_d = v["id"].as_u64().expect("d");

    for id in [ia, ic, id_d] {
        f.json(Method::DELETE, &format!("/api/v1/jobs/{id}"), None)
            .await;
        f.wait_state(id, "cancelled").await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn same_output_is_refused_while_active() {
    let f = Fixture::new();
    f.write_slow();
    let (_, a) = f.post("/api/v1/jobs", slow_job("same.tif")).await;
    let (s, v) = f.post("/api/v1/jobs", slow_job("same.tif")).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_in_use"));
    let id = a["id"].as_u64().expect("id");
    f.json(Method::DELETE, &format!("/api/v1/jobs/{id}"), None)
        .await;
    f.wait_state(id, "cancelled").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_jobs_are_trimmed_to_the_retention_limit() {
    let f = Fixture::with(|b| b.max_retained_jobs(2));
    let mut ids = Vec::new();
    for i in 0..4 {
        let (s, v) = f
            .post("/api/v1/jobs", job("sun.ser", &format!("r{i}.tif")))
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        let id = v["id"].as_u64().expect("id");
        f.wait_state(id, "succeeded").await;
        ids.push(id);
    }
    // Trimming happens on submit, so the newest finished jobs remain.
    let (_, list) = f.get("/api/v1/jobs").await;
    assert!(list.as_array().expect("array").len() <= 3, "{list}");
    assert_eq!(
        f.get(&format!("/api/v1/jobs/{}", ids[0])).await.0,
        StatusCode::NOT_FOUND
    );
}

// ---- WebSocket -------------------------------------------------------------

async fn serve(f: &mut Fixture) -> std::net::SocketAddr {
    let server = f.server.take().expect("server");
    let bound = server.bind().await.expect("bind");
    let addr = bound.local_addr();
    tokio::spawn(bound.serve(std::future::pending()));
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_streams_progress_to_completion() {
    let mut f = Fixture::with(|b| b.bind("127.0.0.1:0".parse().expect("addr")));
    f.write_slow();
    let addr = serve(&mut f).await;

    // The first job occupies the worker until the socket is connected (it is
    // cancelled below), so the second is certainly observed from the queue onwards.
    let (_, a) = f.post("/api/v1/jobs", slow_job("first.tif")).await;
    // Until it holds the worker permit the first job still counts as queued, and the
    // second could take the permit first on a busy machine.
    f.wait_state(a["id"].as_u64().expect("first id"), "running")
        .await;
    let (_, b) = f
        .post(
            "/api/v1/jobs",
            json!({
        "input": "slow.ser", "output": "second.fits",
        "config": {"backend": "cpu", "keep": "60%", "ap_size": 32}}),
        )
        .await;
    let id = b["id"].as_u64().expect("id");
    let ia = a["id"].as_u64().expect("first id");

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/jobs/{id}/events"))
            .await
            .expect("connect");
    // The snapshot is the first message; release the worker only after it, so the
    // second job's start is an event the stream carries.
    let first = ws.next().await.expect("snapshot").expect("frame");
    let first: Value = match first {
        tokio_tungstenite::tungstenite::Message::Text(t) => {
            serde_json::from_str(t.as_str()).expect("json")
        }
        other => panic!("expected a text snapshot, got {other:?}"),
    };
    assert_eq!(first["job"]["state"], "queued", "{first}");
    let (s, _) = f
        .json(Method::DELETE, &format!("/api/v1/jobs/{ia}"), None)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let mut msgs: Vec<Value> = vec![first];
    let t0 = Instant::now();
    while let Some(m) = ws.next().await {
        let m = m.expect("frame");
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
            msgs.push(serde_json::from_str(t.as_str()).expect("json"));
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "stream stalled");
    }
    assert_eq!(msgs[0]["type"], "snapshot");
    assert_eq!(msgs[0]["job"]["id"], id);
    let last = msgs.last().expect("messages");
    assert_eq!(last["type"], "state");
    assert_eq!(last["state"], "succeeded");
    let progress: Vec<f64> = msgs
        .iter()
        .filter(|m| m["type"] == "progress")
        .map(|m| m["progress"].as_f64().expect("p"))
        .collect();
    assert!(
        progress.len() >= 3,
        "expected progress events, got {msgs:?}"
    );
    assert!(
        progress.windows(2).all(|w| w[1] >= w[0]),
        "monotonic: {progress:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m["type"] == "state" && m["state"] == "running")
    );

    let done = f.wait_state(id, "succeeded").await;
    assert_eq!(done["summary"]["frames"], 240);
    let (s, _, bytes) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/result"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let p = f.root().join("dl.fits");
    std::fs::write(&p, bytes).expect("write");
    assert_eq!(helia_core::io::load_image(&p).expect("decode").width, 192);

    // Connecting to a finished job gives a snapshot, then closes.
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/jobs/{id}/events"))
            .await
            .expect("connect");
    let mut n = 0;
    while let Some(m) = ws.next().await {
        if m.expect("frame").is_text() {
            n += 1;
        }
    }
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_requires_the_token_and_a_real_job() {
    let mut f = Fixture::with(|b| {
        b.bind("127.0.0.1:0".parse().expect("addr"))
            .token("tok-123")
    });
    let addr = serve(&mut f).await;
    let denied =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/jobs/1/events")).await;
    match denied {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected an HTTP 401, got {other:?}"),
    }
    let req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://{addr}/api/v1/jobs/42/events"),
    )
    .map(|mut r| {
        r.headers_mut()
            .insert(header::AUTHORIZATION, "Bearer tok-123".parse().expect("hv"));
        r
    })
    .expect("req");
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::NOT_FOUND);
        }
        other => panic!("expected 404 for an unknown job, got {other:?}"),
    }
}

// ---- presets and colour -----------------------------------------------------

#[tokio::test]
async fn presets_are_listed_with_their_recommendations() {
    let f = Fixture::new();
    let (s, v) = f.get("/api/v1/presets").await;
    assert_eq!(s, StatusCode::OK);
    let list = v["presets"].as_array().expect("presets");
    let ids: Vec<&str> = list.iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(
        ids,
        ["ha", "cak", "cah", "mg", "na", "he-d3", "hbeta", "white"]
    );
    let ha = &list[0];
    assert_eq!(ha["channel"], "r");
    assert_eq!(ha["wavelength"], "656.3 nm");
    assert_eq!(ha["keep_percent"], 25.0);
    assert!(ha["reason"].as_str().is_some_and(|r| r.contains("red")));
    let mid = ha["colour"]["mid_grey_rgb"].as_array().expect("mid grey");
    assert!(mid[0].as_u64() > mid[2].as_u64(), "orange: {mid:?}");
}

#[tokio::test]
async fn inspect_reports_what_would_be_recommended() {
    let f = Fixture::new();
    std::fs::write(f.root().join("sun_CaK_001.ser"), small()).expect("write");
    let (s, v) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": "sun_CaK_001.ser"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recommended"]["filter"], "cak");
    assert_eq!(v["recommended"]["filter_source"], "sun_CaK_001");
    assert_eq!(v["recommended"]["keep_percent"], 30.0);
    assert!(v["recommended"]["ap_size"].as_u64().is_some());
    assert_eq!(
        v["recommended"]["channel"], "lum",
        "mono capture ignores channels"
    );
}

#[tokio::test]
async fn a_preset_job_writes_a_colour_tiff_and_png() {
    let f = Fixture::new();
    for (out, ctype) in [("ha.tif", "image/tiff"), ("ha.png", "image/png")] {
        let (s, v) = f
            .post(
                "/api/v1/jobs",
                json!({"input": "sun.ser", "output": out,
                       "config": {"backend": "cpu", "preset": "ha", "ap_size": 32}}),
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        let id = v["id"].as_u64().expect("id");
        let done = f.wait_state(id, "succeeded").await;
        assert_eq!(done["summary"]["filter"], "ha");
        assert_eq!(done["summary"]["filter_source"], "chosen");
        assert!(
            done["summary"]["colour"]
                .as_str()
                .is_some_and(|c| c.contains("preset ha") && c.contains("presentation"))
        );
        let (s, h, bytes) = f
            .call(
                Method::GET,
                &format!("/api/v1/jobs/{id}/result"),
                None,
                None,
            )
            .await;
        assert_eq!(
            (s, h[header::CONTENT_TYPE].to_str().unwrap_or("")),
            (StatusCode::OK, ctype)
        );
        if out.ends_with("tif") {
            let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).expect("tiff");
            assert_eq!(dec.colortype().expect("colour"), tiff::ColorType::RGB(16));
        } else {
            let mut r = png::Decoder::new(std::io::Cursor::new(bytes))
                .read_info()
                .expect("png");
            let mut buf = vec![0u8; r.output_buffer_size().expect("size")];
            let info = r.next_frame(&mut buf).expect("pixels");
            assert_eq!(info.color_type, png::ColorType::Rgb);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn loading_and_colourising_are_reported_as_stages() {
    let mut f = Fixture::with(|b| b.bind("127.0.0.1:0".parse().expect("addr")));
    f.write_slow();
    let addr = serve(&mut f).await;
    // The first job holds the worker until the socket is connected (it is cancelled
    // below), so the second is observed from the queue on.
    let (_, a) = f.post("/api/v1/jobs", slow_job("first.tif")).await;
    let ia = a["id"].as_u64().expect("first id");
    // Until it holds the worker permit the first job still counts as queued, and the
    // second could take the permit first on a busy machine.
    f.wait_state(ia, "running").await;
    let (_, b) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "second.tif",
                   "config": {"backend": "cpu", "preset": "mg", "ap_size": 32}}),
        )
        .await;
    let id = b["id"].as_u64().expect("id");
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/jobs/{id}/events"))
            .await
            .expect("connect");
    // Wait for the snapshot, then release the worker.
    let snapshot = ws.next().await.expect("snapshot").expect("frame");
    assert!(snapshot.is_text(), "{snapshot:?}");
    let (s, _) = f
        .json(Method::DELETE, &format!("/api/v1/jobs/{ia}"), None)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let mut stages: Vec<String> = Vec::new();
    while let Some(m) = ws.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m.expect("frame") {
            let v: Value = serde_json::from_str(t.as_str()).expect("json");
            if let Some(st) = v["stage"].as_str()
                && stages.last().map(String::as_str) != Some(st)
            {
                stages.push(st.to_owned());
            }
        }
    }
    let pos = |n: &str| stages.iter().position(|s| s == n);
    assert!(pos("opening") < pos("quality"), "{stages:?}");
    assert!(pos("finish") < pos("colourise"), "{stages:?}");
    assert!(pos("colourise") < pos("write"), "{stages:?}");
}

/// A SER file whose header says mono although the frames are an RGGB mosaic.
fn mislabelled() -> &'static [u8] {
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| {
        let frames: Vec<_> = (0..12)
            .map(|i| {
                helia_core::testutil::mosaic_disc(
                    160,
                    128,
                    Some(helia_core::BayerPattern::Rggb),
                    [0.94, 1.0, 0.01],
                    3 + i,
                )
            })
            .collect();
        let mut h =
            helia_core::io::ser::SerHeader::new(160, 128, helia_core::io::ser::ColorId::Mono, 8);
        h.instrument = "ZWO ASI482MC".into();
        helia_core::testutil::ser_bytes_with(&frames, h)
    })
}

#[tokio::test]
async fn inspect_shows_the_header_and_finds_a_mosaic_the_header_denies() {
    let f = Fixture::new();
    std::fs::write(f.root().join("mono-label.ser"), mislabelled()).expect("write");
    let (s, v) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": "mono-label.ser"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // The header, as written.
    assert_eq!(v["header"]["colour_id"], 0);
    assert_eq!(v["header"]["colour_meaning"], "MONO");
    assert_eq!(v["header"]["file_id"], "LUCAM-RECORDER");
    assert_eq!(v["header"]["pixel_depth"], 8);
    assert_eq!(v["header"]["instrument"], "ZWO ASI482MC");
    assert!(v["header"]["date_time"].is_null());
    assert!(v["header"]["timestamps"].is_null());
    // What Helia will use, and why.
    assert_eq!(v["colour"], "bayer");
    assert_eq!(v["bayer"], "rggb");
    assert_eq!(v["effective"]["bayer"]["value"], "rggb");
    assert_eq!(v["effective"]["bayer"]["source"], "detection");
    assert_eq!(v["effective"]["bit_depth"]["source"], "header");
    assert_eq!(v["detection"]["verdict"], "mosaic");
    assert_eq!(v["detection"]["pattern"], "rggb");
    assert!(v["detection"]["spread"].as_f64().is_some_and(|s| s > 1.0));
    assert_eq!(v["notices"][0]["level"], "warning");
    assert!(
        v["notices"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("header says mono") && t.contains("RGGB")),
        "{v}"
    );
    assert_eq!(v["overrides"], json!([]));
    // An override wins and is reported as such; the pixels are not checked.
    let (s, v) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": "mono-label.ser", "bayer": "none", "bit_depth": 8, "endian": "big"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["bayer"], serde_json::Value::Null);
    assert_eq!(v["effective"]["bayer"]["value"], "none");
    assert_eq!(v["effective"]["bayer"]["source"], "override");
    assert_eq!(v["effective"]["bit_depth"]["value"], 8);
    assert_eq!(v["effective"]["bit_depth"]["source"], "override");
    assert_eq!(v["effective"]["byte_order"]["value"], "big");
    assert_eq!(v["effective"]["byte_order"]["source"], "override");
    assert!(v["detection"].is_null());
    assert_eq!(
        v["overrides"],
        json!(["--bayer none", "--bit-depth 8", "--endian big"])
    );
    // The header is still the file's own.
    assert_eq!(v["header"]["pixel_depth"], 8);
    // A bit depth the file cannot have is refused, not guessed at.
    let (s, v) = f
        .post(
            "/api/v1/captures/inspect",
            json!({"path": "mono-label.ser", "bit_depth": 16}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_capture"),
        "{v}"
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("truncated"))
    );
    // A true mono capture is not flagged.
    let (_, v) = f
        .post("/api/v1/captures/inspect", json!({"path": "sun.ser"}))
        .await;
    assert_eq!(v["colour"], "mono");
    assert_eq!(v["effective"]["bayer"]["value"], "none");
    assert!(v["notices"].as_array().is_some_and(Vec::is_empty), "{v}");
}

#[tokio::test]
async fn open_overrides_are_validated_and_ser_only() {
    let f = Fixture::new();
    for body in [
        json!({"path": "sun.ser", "bayer": "xyz"}),
        json!({"path": "sun.ser", "bit_depth": 0}),
        json!({"path": "sun.ser", "bit_depth": 17}),
        json!({"path": "sun.ser", "endian": "sideways"}),
    ] {
        let (s, v) = f.post("/api/v1/captures/inspect", body.clone()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config"),
            "{body}"
        );
    }
    let job_body = |cfg: Value| json!({"input": "sun.ser", "output": "o.tif", "config": cfg});
    for cfg in [json!({"bit_depth": 99}), json!({"endian": "middle"})] {
        let (s, v) = f.post("/api/v1/jobs", job_body(cfg.clone())).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config"),
            "{cfg}"
        );
    }
    // The frame preview takes the same overrides.
    let (s, _, bytes) = f
        .call(
            Method::GET,
            "/api/v1/captures/frame?path=sun.ser&flip_rows=true&endian=little",
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(bytes.starts_with(b"\x89PNG"));
}

#[tokio::test]
async fn a_job_debayers_a_mislabelled_capture_and_records_it() {
    let f = Fixture::new();
    std::fs::write(f.root().join("mono-label.ser"), mislabelled()).expect("write");
    let run = |config: Value, out: &str| {
        let body = json!({"input": "mono-label.ser", "output": out, "config": config});
        (f.post("/api/v1/jobs", body), out.to_owned())
    };
    let (req, _) = run(json!({"keep": "50%", "multipoint": false}), "auto.fits");
    let (s, v) = req.await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert_eq!(done["summary"]["bayer"], "rggb");
    assert_eq!(done["summary"]["bayer_source"], "detection");
    assert!(
        done["summary"]["notices"][0]
            .as_str()
            .is_some_and(|t| t.contains("Bayer mosaic")),
        "{done}"
    );
    let fits = std::fs::read(f.root().join("auto.fits")).expect("output");
    let text = String::from_utf8_lossy(&fits);
    assert!(
        text.contains("Capture read as: Bayer RGGB (detection)"),
        "the FITS header records how the capture was read"
    );
    // The user's word wins, and is recorded.
    let (req, _) = run(
        json!({"keep": "50%", "multipoint": false, "bayer": "none"}),
        "none.fits",
    );
    let (s, v) = req.await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert_eq!(done["summary"]["bayer"], Value::Null);
    assert_eq!(done["summary"]["bayer_source"], "override");
    let fits = std::fs::read(f.root().join("none.fits")).expect("output");
    let text = String::from_utf8_lossy(&fits);
    assert!(text.contains("Overrides: --bayer none"));
}

#[tokio::test]
async fn a_job_sharpens_edits_and_records_the_edits() {
    let f = Fixture::new();
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "sharp.tif",
                   "config": {"backend": "cpu", "preset": "ha", "ap_size": 32,
                              "sharpen": "wavelet",
                              "edits": [
                                  {"op": "levels", "gamma": 1.3},
                                  {"op": "crop", "x": 4, "y": 4, "width": 80, "height": 60},
                                  {"op": "flip", "axis": "vertical"}]}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let summary = &done["summary"];
    assert_eq!(
        (summary["width"].as_u64(), summary["height"].as_u64()),
        (Some(80), Some(60))
    );
    assert_eq!(summary["edited"], true);
    let lines: Vec<&str> = summary["edits"]
        .as_array()
        .expect("edit lines")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        lines.iter().any(|l| l.contains("wavelet sharpen")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("crop 80x60 at (4,4)")),
        "{lines:?}"
    );
    let (_, _, bytes) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/result"),
            None,
            None,
        )
        .await;
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).expect("tiff");
    assert_eq!(dec.dimensions().expect("size"), (80, 60));
    let history = dec
        .get_tag_ascii_string(tiff::tags::Tag::ImageDescription)
        .expect("description");
    assert!(history.contains("sharpening runs on the mono stack before any colour"));
    assert!(history.contains("Helia edit list (JSON)"));
}

#[tokio::test]
async fn a_job_takes_registax_style_wavelets() {
    let f = Fixture::new();
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "rs.tif",
                   "config": {"backend": "cpu", "preset": "ha", "ap_size": 32,
                              "wavelets": "registax:l1=100/0.14/0.2,l2=100/0.13/0.15,filter=gaussian,scheme=linear,initial=1,step=0"}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let lines: Vec<&str> = done["summary"]["edits"]
        .as_array()
        .expect("edit lines")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("linear, initial 1 step 0") && l.contains("Gaussian filter")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn a_job_chooses_alignment_points_and_crops_to_the_disc() {
    let f = Fixture::new();
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "auto.tif",
                   "config": {"backend": "cpu", "ap_auto": true, "autocrop": true,
                              "autocrop_margin": 20.0}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let summary = &done["summary"];
    let auto = &summary["auto_ap"];
    assert!(auto["size"].as_u64().is_some_and(|n| n >= 32), "{summary}");
    assert_eq!(auto["step"].as_u64(), auto["size"].as_u64().map(|n| n / 2));
    assert!(auto["text"].as_str().is_some_and(|t| t.contains("px")));
    let crop = &summary["crop"];
    let (w, h) = (
        summary["width"].as_u64().expect("width"),
        summary["height"].as_u64().expect("height"),
    );
    assert_eq!((crop["w"].as_u64(), crop["h"].as_u64()), (Some(w), Some(h)));
    assert!(w < 96, "the 96 x 80 frame was cropped: {w} x {h}");
    // Off, the summary says nothing about either.
    let (_, v) = f.post("/api/v1/jobs", job("sun.ser", "plain.tif")).await;
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert!(done["summary"]["auto_ap"].is_null() && done["summary"]["crop"].is_null());
}

#[tokio::test]
async fn a_job_reports_the_limb_darkening_and_can_flatten_it() {
    let f = Fixture::new();
    let run = |cfg: Value, out: &'static str| {
        let f = &f;
        async move {
            let (s, v) = f
                .post(
                    "/api/v1/jobs",
                    json!({"input": "sun.ser", "output": out, "config": cfg}),
                )
                .await;
            assert_eq!(s, StatusCode::ACCEPTED, "{v}");
            f.wait_state(v["id"].as_u64().expect("id"), "succeeded")
                .await
        }
    };
    let plain = run(json!({"backend": "cpu"}), "plain.fits").await;
    let limb = &plain["summary"]["limb_darkening"];
    assert_eq!(limb["law"], "quadratic", "{plain}");
    assert_eq!(limb["names"], json!(["u1", "u2"]));
    // A 27 pixel disc with noise: only the shape of the answer is checked.
    assert_eq!(limb["coefficients"].as_array().map(Vec::len), Some(2));
    assert!(
        limb["limb_level"]
            .as_f64()
            .is_some_and(|l| l > 0.0 && l < 1.5)
    );
    assert!(
        limb["radius_px"]
            .as_f64()
            .is_some_and(|r| (20.0..35.0).contains(&r))
    );
    let neckel = run(
        json!({"backend": "cpu", "limb_law": "linear", "flatten": "profile", "sharpen_fits": true}),
        "flat.fits",
    )
    .await;
    assert_eq!(neckel["summary"]["limb_darkening"]["law"], "linear");
    let edits = neckel["summary"]["edits"].to_string();
    assert!(edits.contains("flatten the limb darkening"), "{edits}");
}

/// A mono capture whose frames are stamped every ninety seconds from
/// 2022-07-17 15:32 UTC.
fn stamped() -> &'static [u8] {
    use helia_core::io::ser::{ColorId, SerHeader};
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 96,
            height: 80,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 16,
                noise: 0.02,
                ..CaptureConfig::default()
            },
        );
        helia_core::testutil::ser_bytes_stamped(
            &cap.frames,
            SerHeader::new(96, 80, ColorId::Mono, 16),
            637_936_687_290_000_000,
            900_000_000,
        )
    })
}

#[tokio::test]
async fn a_job_undoes_the_field_rotation_and_reports_it() {
    let f = Fixture::new();
    std::fs::write(f.root().join("stamped.ser"), stamped()).expect("write");
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({
                "input": "stamped.ser",
                "output": "rot.fits",
                "config": {
                    "backend": "cpu",
                    "multipoint": false,
                    "site": "51.5,-0.1,30",
                    "field_rotation": {"reference": "first"}
                }
            }),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    let r = &done["summary"]["field_rotation"];
    assert!(r.is_object(), "{done}");
    // Twenty-two minutes at a few tenths of a degree a minute.
    assert!(
        r["span_deg"]
            .as_f64()
            .is_some_and(|d| d.abs() > 0.5 && d.abs() < 15.0),
        "{r}"
    );
    assert!(r["max_turn_deg"].as_f64().is_some_and(|d| d > 0.5), "{r}");
    assert_eq!(r["frames_stamped"], 16);
    assert!(
        r["reference_time"]
            .as_str()
            .is_some_and(|t| t.starts_with("2022-07-17T15:"))
    );
    assert!(r["lines"].as_array().is_some_and(|l| !l.is_empty()));
    let fits = std::fs::read(f.root().join("rot.fits")).expect("fits");
    assert!(String::from_utf8_lossy(&fits).contains("FIELD-ROTATION"));
    // Without the option the summary says nothing about it.
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "stamped.ser", "output": "plain.fits", "config": {"backend": "cpu"}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert!(done["summary"]["field_rotation"].is_null());
    // A capture with no timestamps fails the job with a reason.
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "bad.fits", "config": {"backend": "cpu", "site": "51.5,-0.1", "field_rotation": {}}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let failed = f.wait_state(v["id"].as_u64().expect("id"), "failed").await;
    assert_eq!(failed["error"]["code"], "invalid_config", "{failed}");
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("time of each frame"))
    );
}

#[tokio::test]
async fn a_job_reports_sunspots_and_serves_the_table_and_overlay() {
    let f = Fixture::new();
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "spots.fits", "config": {"backend": "cpu", "sunspots": {"wolf_k": 0.6}}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let spots = &done["summary"]["sunspots"];
    assert!(spots.is_object(), "{done}");
    assert_eq!(spots["k"].as_f64().map(|k| (k * 10.0).round()), Some(6.0));
    let groups = spots["groups"].as_u64().expect("groups");
    let n = spots["spots"].as_u64().expect("spots");
    assert!(
        spots["wolf"]
            .as_f64()
            .is_some_and(|w| (w - 0.6 * (10 * groups + n) as f64).abs() < 1e-3)
    );
    assert!(spots["spot_table"].is_array() && spots["group_table"].is_array());
    let (s, h, csv) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/sunspots.csv"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .expect("type")
            .starts_with("text/csv")
    );
    assert!(String::from_utf8_lossy(&csv).starts_with("spot,group,"));
    let (s, h, png) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/sunspots.png"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
    // The FITS header carries the summary.
    let fits = std::fs::read(f.root().join("spots.fits")).expect("fits");
    assert!(String::from_utf8_lossy(&fits).contains("SPOTWOLF"));
    // A job that did not ask has nothing to serve.
    let (s, v) = f.post("/api/v1/jobs", job("sun.ser", "plain.tif")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    assert!(done["summary"]["sunspots"].is_null());
    let (s, v) = f.get(&format!("/api/v1/jobs/{id}/sunspots.png")).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "sunspots_missing"));
}

#[tokio::test]
async fn a_job_serves_the_science_report_when_asked() {
    let f = Fixture::new();
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "sci.fits", "config": {"backend": "cpu", "report": true, "ap_residual": true}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let files: Vec<&str> = done["summary"]["report"]["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    for want in [
        "report.html",
        "quality-sorted.png",
        "frame-quality.csv",
        "frame-quality.json",
        "ap-grid.png",
        "ap-residual.png",
        "ap-table.csv",
        "histogram.png",
        "image-science.json",
    ] {
        assert!(files.contains(&want), "{want} in {files:?}");
    }
    let (s, h, html) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/report"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .expect("type")
            .starts_with("text/html")
    );
    assert!(String::from_utf8_lossy(&html).contains("data:image/png;base64,"));
    let (s, h, png) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/report/quality-order.png"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(&png[1..4], b"PNG");
    let (s, v) = f.get(&format!("/api/v1/jobs/{id}/report/nope.png")).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "report_file_missing")
    );
    // A capture with no time of its own takes `date` for the heliographic grids.
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "dated.tif", "config": {"backend": "cpu", "report": true, "date": "2026-10-20T11:30:00"}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let done = f
        .wait_state(v["id"].as_u64().expect("id"), "succeeded")
        .await;
    assert!(
        done["summary"]["report"]["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|x| x == "grid-stonyhurst.png"),
        "{done}"
    );
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "bad.tif", "config": {"backend": "cpu", "report": true, "date": "yesterday"}}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    // A job that did not ask has no report.
    let (s, v) = f.post("/api/v1/jobs", job("sun.ser", "plain2.tif")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    assert!(done["summary"]["report"].is_null());
    let (s, v) = f.get(&format!("/api/v1/jobs/{id}/report")).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "report_missing"));
}

/// A raw colour capture of the synthetic Sun: red at half and blue at 0.7 of
/// the green's brightness.
fn colour_capture_bytes() -> &'static [u8] {
    use helia_core::{BayerPattern, Shift};
    static B: OnceLock<Vec<u8>> = OnceLock::new();
    B.get_or_init(|| {
        let sun = SyntheticSun::new(&SyntheticConfig {
            width: 128,
            height: 112,
            ..SyntheticConfig::default()
        });
        let cap = SyntheticCapture::generate(
            &sun,
            &CaptureConfig {
                frames: 16,
                noise: 0.01,
                ..CaptureConfig::default()
            },
        );
        let frames = helia_core::testutil::colour_mosaic(
            &cap.frames,
            [0.5, 1.0, 0.7],
            [
                Shift::new(-0.5, -0.6),
                Shift::default(),
                Shift::new(0.4, 0.5),
            ],
            BayerPattern::Rggb,
        );
        let h = helia_core::io::ser::SerHeader::new(
            128,
            112,
            helia_core::io::ser::ColorId::Bayer(BayerPattern::Rggb),
            16,
        );
        helia_core::testutil::ser_bytes_with(&frames, h)
    })
}

#[tokio::test]
async fn a_true_colour_job_writes_a_balanced_colour_image() {
    let f = Fixture::new();
    std::fs::write(f.root().join("colour.ser"), colour_capture_bytes()).expect("write");
    let run = |cfg: Value, out: &'static str| {
        let f = &f;
        async move {
            let (s, v) = f
                .post(
                    "/api/v1/jobs",
                    json!({"input": "colour.ser", "output": out, "config": cfg}),
                )
                .await;
            assert_eq!(s, StatusCode::ACCEPTED, "{v}");
            f.wait_state(v["id"].as_u64().expect("id"), "succeeded")
                .await
        }
    };
    let done = run(
        json!({"backend": "cpu", "colour": "true", "keep": "40%"}),
        "t.tif",
    )
    .await;
    let summary = &done["summary"];
    assert_eq!(summary["colour"], "true colour", "{summary}");
    let tc = &summary["true_colour"];
    let gains: Vec<f64> = tc["gains"]
        .as_array()
        .expect("gains")
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    // Red is half and blue 0.7 as bright as green: the gains undo that.
    assert!((gains[0] / gains[1] - 2.0).abs() < 0.15, "{tc}");
    assert!((gains[2] / gains[1] - 1.0 / 0.7).abs() < 0.12, "{tc}");
    let ch = tc["channels"].as_array().expect("channels");
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[0]["channel"], "red");
    assert_eq!(ch[0]["applied"], true);
    assert!(tc["notes"].as_array().is_some_and(|n| !n.is_empty()));
    let (_, _, bytes) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{}/result", done["id"]),
            None,
            None,
        )
        .await;
    let (img, _) = helia_core::io::any::read_tiff_bytes(&bytes).expect("tiff");
    let helia_core::io::any::Loaded::Rgb(rgb) = img else {
        panic!("the result is not colour");
    };
    let (cx, cy) = (rgb.width / 2, rgb.height / 2);
    let p = rgb.pixel(cx, cy);
    assert!(
        (p[0] / p[1] - 1.0).abs() < 0.1 && (p[2] / p[1] - 1.0).abs() < 0.1,
        "{p:?}"
    );
    // No alignment of the channels, no balance, and a FITS cube.
    let done = run(
        json!({"backend": "cpu", "colour": "true", "white_balance": "off", "align_channels": false}),
        "t.fits",
    )
    .await;
    assert!(done["summary"]["true_colour"]["channels"].is_null());
    let g = &done["summary"]["true_colour"]["gains"];
    assert!((g[0].as_f64().unwrap_or(0.0) - g[1].as_f64().unwrap_or(1.0)).abs() < 1e-6);
    let fits = std::fs::read(f.root().join("t.fits")).expect("fits");
    let (planes, _) =
        helia_core::io::fits::read_fits_planes(std::io::Cursor::new(fits)).expect("planes");
    assert_eq!(planes.len(), 3);
    // A mono capture cannot be stacked in true colour.
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "bad.tif", "config": {"colour": "true"}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let failed = f.wait_state(id, "failed").await;
    assert_eq!(failed["error"]["code"], "invalid_config", "{failed}");
}

#[tokio::test]
async fn fits_output_is_unedited_unless_the_job_asks() {
    let f = Fixture::new();
    let mut sizes = Vec::new();
    for (out, fits_too) in [("plain.fits", false), ("asked.fits", true)] {
        let (_, v) = f
            .post(
                "/api/v1/jobs",
                json!({"input": "sun.ser", "output": out,
                       "config": {"backend": "cpu", "ap_size": 32, "sharpen": "unsharp",
                                  "edits": [{"op": "crop", "x": 0, "y": 0, "width": 64, "height": 48}],
                                  "sharpen_fits": fits_too}}),
            )
            .await;
        let id = v["id"].as_u64().expect("id");
        let done = f.wait_state(id, "succeeded").await;
        sizes.push((
            done["summary"]["width"].as_u64(),
            done["summary"]["edited"].clone(),
        ));
    }
    assert_eq!(sizes[0], (Some(96), json!(false)));
    assert_eq!(sizes[1], (Some(64), json!(true)));
}

#[tokio::test]
async fn bad_edits_are_refused_up_front() {
    let f = Fixture::new();
    for config in [
        json!({"sharpen": "wobble"}),
        json!({"edits": [{"op": "sparkle"}]}),
        json!({"edits": [{"op": "levels", "black": 0.9, "white": 0.1}]}),
        json!({"edits": [{"op": "wavelet", "layers": [{"gain": -1.0}]}]}),
        json!({"edits": [{"op": "unsharp", "radius": 0.0}]}),
        json!({"wavelets": "registax:l9=100/0.1"}),
        json!({"wavelets": "registax:l1=100/2"}),
        json!({"wavelets": "registax:l1=100/0.1", "sharpen": "unsharp"}),
    ] {
        let (s, v) = f
            .post(
                "/api/v1/jobs",
                json!({"input": "sun.ser", "output": "x.tif", "config": config}),
            )
            .await;
        assert!(
            s == StatusCode::UNPROCESSABLE_ENTITY || s == StatusCode::BAD_REQUEST,
            "{config}: {s} {v}"
        );
    }
    // A crop that does not fit this capture fails the job, not the request.
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "x.tif",
                   "config": {"backend": "cpu", "ap_size": 32,
                              "edits": [{"op": "crop", "x": 90, "y": 0, "width": 50, "height": 10}]}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let failed = f.wait_state(id, "failed").await;
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("crop")),
        "{failed}"
    );
}

/// A capture through a sensor with dust, vignetting, a fixed pattern and hot
/// pixels, plus the dark and flat frames of the same sensor, as files.
fn write_defective_rig(root: &Path) {
    use helia_core::testutil::{DefectConfig, SensorDefects};
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 96,
        height: 80,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 16,
            noise: 0.01,
            ..CaptureConfig::default()
        },
    );
    let sensor = SensorDefects::new(96, 80, 4, &DefectConfig::default());
    let lights: Vec<helia_core::Frame> = cap
        .frames
        .iter()
        .map(|f| {
            let mut d = f.clone();
            d.data.iter_mut().for_each(|v| *v *= 0.75);
            sensor.apply(&mut d);
            d
        })
        .collect();
    std::fs::write(root.join("sun.ser"), ser_bytes(&lights)).expect("write");
    std::fs::create_dir_all(root.join("cal/darks")).expect("mkdir");
    std::fs::write(
        root.join("cal/darks.ser"),
        ser_bytes(&sensor.dark_frames(12, 0.004, 1)),
    )
    .expect("write");
    std::fs::write(
        root.join("cal/flats.ser"),
        ser_bytes(&sensor.flat_frames(12, 0.6, 0.004, 2)),
    )
    .expect("write");
}

#[tokio::test]
async fn a_job_calibrates_from_capture_files_and_records_it() {
    let f = Fixture::new();
    write_defective_rig(f.root());
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "cal.fits",
                   "config": {"backend": "cpu", "ap_size": 32,
                              "calibration": {"dark": "cal/darks.ser", "flat": "cal/flats.ser",
                                              "flat_dark": "cal/darks.ser"}}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    let lines = done["summary"]["calibration"].as_array().expect("lines");
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().is_some_and(|l| l.starts_with("dark:"))),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().is_some_and(|l| l.starts_with("flat:")))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().is_some_and(|l| l.starts_with("bad pixels:")))
    );
    let (s, _, bytes) = f
        .call(
            Method::GET,
            &format!("/api/v1/jobs/{id}/result"),
            None,
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(28_800)]).into_owned();
    for step in ["DARK-SUBTRACTION", "FLAT-FIELDING", "BAD-PIXEL-CORRECTION"] {
        assert!(head.contains(step), "{step} missing from the FITS header");
    }
}

#[tokio::test]
async fn a_job_without_calibration_says_nothing_about_it() {
    let f = Fixture::new();
    let (_, v) = f.post("/api/v1/jobs", job("sun.ser", "plain.fits")).await;
    let id = v["id"].as_u64().expect("id");
    let done = f.wait_state(id, "succeeded").await;
    assert_eq!(done["summary"]["calibration"], json!([]));
}

#[tokio::test]
async fn calibration_config_is_validated_up_front() {
    let f = Fixture::new();
    write_defective_rig(f.root());
    let post = |cal: Value| {
        f.post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "x.tif", "config": {"calibration": cal}}),
        )
    };
    let (s, v) = post(json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"dark": "cal/darks.ser", "combine": "mean"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    // The flat is either an ordinary one or solved from displaced frames.
    let (s, v) = post(json!({"flat": "cal/flats.ser", "displaced": "cal/flats.ser"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"displaced": "cal/missing.ser"})).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));
    let (s, v) = post(json!({"dark": "cal/missing.ser"})).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));
    let (s, v) = post(json!({"dark": "../etc"})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_traversal"));
    let (s, v) = post(json!({"dark": "cal/darks.ser", "nope": 1})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")
    );
}

#[tokio::test]
async fn a_calibration_of_the_wrong_size_fails_the_job_with_a_clear_message() {
    let f = Fixture::new();
    write_defective_rig(f.root());
    std::fs::write(f.root().join("cal/other.ser"), capture_bytes(64, 48, 6)).expect("write");
    let (s, v) = f
        .post(
            "/api/v1/jobs",
            json!({"input": "sun.ser", "output": "x.fits",
                   "config": {"backend": "cpu", "calibration": {"dark": "cal/other.ser"}}}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_u64().expect("id");
    let failed = f.wait_state(id, "failed").await;
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("same camera")),
        "{failed}"
    );
}

/// Three stacked images of one disc, moved by known amounts, in the data root.
fn write_moving_stacks(root: &Path) -> Vec<(f32, f32)> {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 224,
        height: 192,
        radius_frac: 0.36,
        ..SyntheticConfig::default()
    });
    let moves = vec![(0.0, 0.0), (4.3, -2.6), (-3.1, 5.4)];
    std::fs::create_dir_all(root.join("stacks")).expect("dir");
    for (i, &(dx, dy)) in moves.iter().enumerate() {
        let frame = sun.render_clean(helia_core::Shift::new(dx, dy), None);
        helia_core::io::save_image(&root.join(format!("stacks/s{i}.tif")), &frame).expect("save");
    }
    moves
}

#[tokio::test]
async fn a_timelapse_registers_stacks_and_writes_frames_and_a_movie() {
    let f = Fixture::new();
    let moves = write_moving_stacks(f.root());
    let (s, v) = f
        .post(
            "/api/v1/timelapse",
            json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif", "stacks/s2.tif"],
                   "times": ["2026-10-20T14:30:00", "2026-10-20T14:35:00", "2026-10-20T14:40:00"],
                   "output": "tl", "reference": "first", "movie": "apng", "fps": 4,
                   "deflicker": "match"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let frames = v["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 3);
    for (fr, &(dx, dy)) in frames.iter().zip(&moves) {
        let sx = fr["shift_x"].as_f64().expect("sx") as f32;
        let sy = fr["shift_y"].as_f64().expect("sy") as f32;
        assert!((sx - dx).abs() < 0.15 && (sy - dy).abs() < 0.15, "{fr}");
    }
    assert_eq!(frames[0]["method"], "reference");
    assert_eq!(frames[1]["method"], "limb+phase");
    assert_eq!(frames[1]["time"], "2026-10-20T14:35:00.000");
    assert_eq!(v["movie_kind"], "apng");
    assert_eq!(v["table"], "tl/timelapse.csv");
    for name in [
        "tl/frame-0001.png",
        "tl/frame-0003.png",
        "tl/timelapse.png",
        "tl/timelapse.csv",
    ] {
        assert!(f.root().join(name).is_file(), "{name}");
    }
    // A second run needs overwrite.
    let (s, v) = f
        .post(
            "/api/v1/timelapse",
            json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif"], "output": "tl", "movie": "none"}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_exists"));
}

#[tokio::test]
async fn a_timelapse_refuses_what_it_cannot_use() {
    let f = Fixture::new();
    write_moving_stacks(f.root());
    let post = |body: Value| f.post("/api/v1/timelapse", body);
    let (s, v) = post(json!({"inputs": ["stacks/s0.tif"], "output": "o"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"inputs": ["sun.ser", "stacks/s1.tif"], "output": "o"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_input")
    );
    let (s, v) = post(json!({"inputs": ["stacks/s0.tif", "stacks/nope.tif"], "output": "o"})).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));
    let (s, v) =
        post(json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif"], "output": "../o"})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "path_traversal"));
    let (s, v) = post(
        json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif"], "output": "o",
                             "deflicker": "smooth", "deflicker_window": 4}),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(
        json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif"], "output": "o",
                             "times": ["2026-10-20T14:30:00"]}),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) =
        post(json!({"inputs": ["stacks/s0.tif", "stacks/s1.tif"], "output": "o", "nope": 1})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")
    );
}

/// Two views of a spotted Sun six hours apart, as TIFFs named by their times.
fn write_rotating_sun(root: &Path) {
    use helia_core::derotate::{DiscGeometry, Orientation, RotationLaw, synth::render_sun};
    let disc = DiscGeometry {
        cx: 320.0,
        cy: 260.0,
        r: 240.0,
    };
    let feats = [
        (0.0, -5.0),
        (35.0, -12.0),
        (-35.0, -8.0),
        (20.0, -35.0),
        (-20.0, 10.0),
    ];
    let t0 = helia_core::io::solarnet::unix_from_iso("2026-10-20T09:00:00").expect("time");
    std::fs::create_dir_all(root.join("sun")).expect("dir");
    for (name, h) in [("sun_20261020_090000", 0.0), ("sun_20261020_150000", 6.0)] {
        let f = render_sun(
            640,
            520,
            disc,
            t0 + h * 3600.0,
            t0,
            RotationLaw::Howard,
            Orientation::default(),
            &feats,
        );
        helia_core::io::save_image(&root.join(format!("sun/{name}.tif")), &f).expect("save");
    }
}

#[tokio::test]
async fn solar_derotation_brings_images_to_one_time() {
    let f = Fixture::new();
    write_rotating_sun(f.root());
    let (s, v) = f
        .post(
            "/api/v1/solar-derotation",
            json!({"inputs": ["sun/sun_20261020_090000.tif", "sun/sun_20261020_150000.tif"],
                   "output": "d", "to": "first", "law": "howard"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["target"], "2026-10-20T09:00:00.000");
    let images = v["images"].as_array().expect("images");
    assert_eq!(images.len(), 2);
    assert!(images[0]["max_shift_px"].as_f64().expect("n") < 1e-6);
    let moved = images[1]["centre_shift_px"].as_f64().expect("n");
    assert!((moved - 13.9).abs() < 0.3, "{moved}");
    assert_eq!(images[1]["dt_s"], -21600.0);
    assert!(
        f.root()
            .join("d/sun_20261020_150000-derotated.tif")
            .is_file()
    );
    // Again without overwrite.
    let (s, v) = f
        .post(
            "/api/v1/solar-derotation",
            json!({"inputs": ["sun/sun_20261020_090000.tif"], "output": "d"}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_exists"));
}

#[tokio::test]
async fn solar_derotation_refuses_what_it_cannot_use() {
    let f = Fixture::new();
    write_rotating_sun(f.root());
    std::fs::copy(
        f.root().join("sun/sun_20261020_090000.tif"),
        f.root().join("sun/plain.tif"),
    )
    .expect("copy");
    let post = |b: Value| f.post("/api/v1/solar-derotation", b);
    let (s, v) = post(json!({"inputs": ["sun/plain.tif"], "output": "o"})).await;
    assert_eq!((s, code(&v)), (StatusCode::UNPROCESSABLE_ENTITY, "no_time"));
    let (s, v) = post(json!({"inputs": ["sun/plain.tif"], "times": ["x"], "output": "o"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"inputs": ["sun/plain.tif"], "times": ["2026-10-20T09:00:00"], "output": "o", "law": "fast"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"inputs": ["sun/plain.tif"], "times": ["2026-10-20T09:00:00"], "output": "o", "nope": 1})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")
    );
    let (s, v) =
        post(json!({"inputs": ["sun.ser"], "times": ["2026-10-20T09:00:00"], "output": "o"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_input")
    );
}

#[tokio::test]
async fn a_timelapse_can_derotate() {
    let f = Fixture::new();
    write_rotating_sun(f.root());
    let (s, v) = f
        .post(
            "/api/v1/timelapse",
            json!({"inputs": ["sun/sun_20261020_150000.tif", "sun/sun_20261020_090000.tif"],
                   "output": "tl", "movie": "none", "reference": "first",
                   "derotate": {"law": "snodgrass"}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Sorted by time, the reference is the 09:00 view and the 15:00 one is derotated.
    let frames = v["frames"].as_array().expect("frames");
    assert_eq!(frames[0]["input"], "sun/sun_20261020_090000.tif");
    assert!(frames[1]["derotated_px"].as_f64().expect("n") > 10.0);
    assert!(
        v["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .any(|n| n.as_str().is_some_and(|t| t.contains("derotated to")))
    );
    // With `report` the diagnostics are written beside the frames.
    let (s, v) = f
        .post(
            "/api/v1/timelapse",
            json!({"inputs": ["sun/sun_20261020_150000.tif", "sun/sun_20261020_090000.tif"],
                   "output": "tl3", "movie": "none", "reference": "first",
                   "derotate": {"law": "snodgrass"}, "report": true}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["report"], "tl3/report/report.html");
    for file in [
        "report.html",
        "timelapse-registration.png",
        "timelapse-derotation.png",
        "timelapse-frames.csv",
    ] {
        assert!(f.root().join("tl3/report").join(file).is_file(), "{file}");
    }
    let (s, v) = f
        .post(
            "/api/v1/timelapse",
            json!({"inputs": ["sun/sun_20261020_150000.tif", "sun/sun_20261020_090000.tif"],
                   "output": "tl2", "movie": "none", "derotate": {"law": "x,y"}}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
}

/// Four overlapping panels of one synthetic Sun, written as TIFFs under `panels/`.
fn write_mosaic_panels(root: &Path) {
    use helia_core::resample::{Kernel, sample};
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 520,
        height: 440,
        radius_frac: 0.43,
        granulation: 0.15,
        spots: 5,
        ..SyntheticConfig::default()
    });
    let cuts = [
        ((8.0f64, 10.0f64), 0.0f64, (1.0f32, 0.0f32)),
        ((210.0, 14.0), 1.0, (0.93, 0.015)),
        ((12.0, 170.0), -0.8, (1.06, -0.01)),
        ((214.0, 176.0), 0.6, (0.97, 0.008)),
    ];
    std::fs::create_dir_all(root.join("panels")).expect("dir");
    for (i, &((x0, y0), deg, (g, o))) in cuts.iter().enumerate() {
        let (w, h) = (310usize, 270usize);
        let mut f = helia_core::Frame::zeros(w, h);
        let (s, c) = deg.to_radians().sin_cos();
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f64 - 154.5, y as f64 - 134.5);
                let (qx, qy) = (c * u - s * v + 154.5 + x0, s * u + c * v + 134.5 + y0);
                f.data[y * w + x] =
                    sample(sun.truth(), qx as f32, qy as f32, Kernel::Lanczos3) * 0.55 * g + o;
            }
        }
        helia_core::io::save_image(&root.join(format!("panels/p{}.tif", i + 1)), &f).expect("save");
    }
}

#[tokio::test]
async fn a_mosaic_joins_panels_and_reports_where_they_went() {
    let f = Fixture::new();
    write_mosaic_panels(f.root());
    let (s, v) = f
        .post(
            "/api/v1/mosaic",
            json!({"inputs": ["panels/p1.tif", "panels/p2.tif", "panels/p3.tif", "panels/p4.tif"],
                   "output": "out/sun.tif", "max_rotation_deg": 3, "feather_px": 10}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let panels = v["panels"].as_array().expect("panels");
    assert_eq!(panels.len(), 4);
    let want = [
        (0.0, 0.0, 0.0),
        (202.0, 4.0, 1.0),
        (4.0, 160.0, -0.8),
        (206.0, 166.0, 0.6),
    ];
    for (p, &(x, y, a)) in panels.iter().zip(&want) {
        let (px, py, pa) = (
            p["x"].as_f64().expect("x"),
            p["y"].as_f64().expect("y"),
            p["angle_deg"].as_f64().expect("a"),
        );
        assert!(
            (px - x).abs() < 0.3 && (py - y).abs() < 0.3 && (pa - a).abs() < 0.08,
            "{p}"
        );
    }
    assert!(
        (panels[1]["gain"].as_f64().expect("g") - 1.0 / 0.93).abs() < 0.03,
        "{}",
        panels[1]
    );
    assert!(v["pairs"].as_array().expect("pairs").len() >= 4);
    assert_eq!(v["panels_table"], "out/sun-panels.csv");
    for name in ["out/sun.tif", "out/sun-panels.csv", "out/sun-pairs.csv"] {
        assert!(f.root().join(name).is_file(), "{name}");
    }
    assert!(v["disc"].is_array());
    // Again without overwrite.
    let (s, v) = f
        .post(
            "/api/v1/mosaic",
            json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "out/sun.tif"}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_exists"));
}

#[tokio::test]
async fn a_mosaic_refuses_what_it_cannot_use() {
    let f = Fixture::new();
    write_mosaic_panels(f.root());
    let post = |b: Value| f.post("/api/v1/mosaic", b);
    let (s, v) = post(json!({"inputs": ["panels/p1.tif"], "output": "o.tif"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"inputs": ["panels/p1.tif", "sun.ser"], "output": "o.tif"})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_input")
    );
    let (s, v) =
        post(json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "o.xyz"})).await;
    assert_eq!(
        (s, code(&v)),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_output_format"
        )
    );
    let (s, v) = post(json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "o.tif", "max_rotation_deg": 90})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_config")
    );
    let (s, v) = post(json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "panels/p1.tif", "overwrite": true})).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "output_is_input"));
    let (s, v) = post(
        json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "o.tif", "derotate": {}}),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "mosaic_failed")
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("time of every panel")),
        "{v}"
    );
    let (s, v) =
        post(json!({"inputs": ["panels/p1.tif", "panels/p2.tif"], "output": "o.tif", "nope": 1}))
            .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_json")
    );
}
