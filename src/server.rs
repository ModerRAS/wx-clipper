use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::error::ClipError;
use crate::fetch::build_client;
use crate::pipeline::{self, display_path, preview_url, ClipOutput};
use crate::store::{self, ClipRecord};

pub struct AppState {
    pub output: PathBuf,
    pub origin: String,
    pub client: reqwest::Client,
    pub lock: Mutex<()>,
}

#[derive(Deserialize)]
struct ClipRequest {
    url: String,
    html: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
struct ClipResponse {
    ok: bool,
    status: String,
    message: String,
    title: String,
    account: String,
    dir: String,
    markdown: String,
    preview_path: String,
    preview_url: String,
    source_url: String,
    image_count: usize,
    image_failed: usize,
    need_html: bool,
}

#[derive(Serialize)]
struct ClipList {
    clips: Vec<ClipListItem>,
}

#[derive(Serialize)]
struct ClipListItem {
    title: String,
    account: String,
    author: String,
    published: String,
    saved_at: String,
    dir: String,
    source_url: String,
    image_count: usize,
    image_failed: usize,
    markdown: String,
    preview_url: String,
}

pub async fn serve(addr: &str, output: PathBuf, origin: String) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(&output).await?;
    let state = Arc::new(AppState {
        output: output.clone(),
        origin,
        client: build_client(),
        lock: Mutex::new(()),
    });
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    let app = Router::new()
        .route("/", get(home))
        .route("/health", get(health))
        .route("/wx-clipper.user.js", get(userscript))
        .route("/api/clip", post(clip))
        .route("/api/clips", get(list_clips))
        .nest_service("/files", ServeDir::new(output))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(cors)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

async fn home() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn userscript(State(state): State<Arc<AppState>>) -> Response {
    let body = include_str!("../userscript/wx-clipper.user.js")
        .replace("http://127.0.0.1:17331", &state.origin);
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "inline; filename=\"wx-clipper.user.js\"",
            ),
        ],
        body,
    )
        .into_response()
}

async fn clip(State(state): State<Arc<AppState>>, Json(req): Json<ClipRequest>) -> Response {
    let _guard = state.lock.lock().await;
    match pipeline::clip_article(&state.client, &state.output, &req.url, req.html, req.force).await
    {
        Ok(output) => (StatusCode::OK, Json(ClipResponse::from_output(output))).into_response(),
        Err(err) => {
            let status = match &err {
                ClipError::BadUrl => StatusCode::BAD_REQUEST,
                ClipError::Blocked => StatusCode::CONFLICT,
                ClipError::Empty => StatusCode::UNPROCESSABLE_ENTITY,
                ClipError::Fetch(_) => StatusCode::BAD_GATEWAY,
                ClipError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(ClipResponse::from_error(&err))).into_response()
        }
    }
}

async fn list_clips(State(state): State<Arc<AppState>>) -> Response {
    match store::list_records(&state.output).await {
        Ok(records) => {
            let output = state.output.clone();
            let clips = records
                .into_iter()
                .map(|record| ClipListItem::from_record(&output, record))
                .collect();
            Json(ClipList { clips }).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

impl ClipResponse {
    fn from_output(output: ClipOutput) -> Self {
        Self {
            ok: true,
            status: output.status.into(),
            message: output.message,
            title: output.title,
            account: output.account,
            dir: output.dir,
            markdown: output.markdown_path,
            preview_path: output.preview_path,
            preview_url: output.preview_url,
            source_url: output.source_url,
            image_count: output.image_count,
            image_failed: output.image_failed,
            need_html: false,
        }
    }

    fn from_error(err: &ClipError) -> Self {
        Self {
            ok: false,
            status: err.status_code().into(),
            message: err.to_string(),
            title: String::new(),
            account: String::new(),
            dir: String::new(),
            markdown: String::new(),
            preview_path: String::new(),
            preview_url: String::new(),
            source_url: String::new(),
            image_count: 0,
            image_failed: 0,
            need_html: err.need_html(),
        }
    }
}

impl ClipListItem {
    fn from_record(output: &std::path::Path, record: ClipRecord) -> Self {
        Self {
            markdown: display_path(&output.join(&record.dir).join("article.md")),
            preview_url: preview_url(&record.dir),
            title: record.title,
            account: record.account,
            author: record.author,
            published: record.published,
            saved_at: record.saved_at,
            dir: record.dir,
            source_url: record.source_url,
            image_count: record.image_count,
            image_failed: record.image_failed,
        }
    }
}
