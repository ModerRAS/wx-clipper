use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub close_wechat_tab: AtomicBool,
}

#[derive(Debug, Deserialize, Serialize)]
struct SettingsBody {
    #[serde(default)]
    close_wechat_tab: bool,
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

pub async fn serve(
    addr: &str,
    output: PathBuf,
    origin: String,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(&output).await?;
    let state = Arc::new(AppState {
        close_wechat_tab: AtomicBool::new(read_close_wechat_tab(&output)),
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
        .route("/api/settings", get(get_settings).post(set_settings))
        .nest_service("/files", ServeDir::new(output))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .layer(cors);
    #[cfg(windows)]
    crate::wechat_watch::start(state.clone());
    let app = app.with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
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

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsBody> {
    Json(SettingsBody {
        close_wechat_tab: state.close_wechat_tab.load(Ordering::Relaxed),
    })
}

async fn set_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> Response {
    if let Err(err) = write_close_wechat_tab(&state.output, body.close_wechat_tab).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
    }
    state
        .close_wechat_tab
        .store(body.close_wechat_tab, Ordering::Relaxed);
    Json(body).into_response()
}

fn settings_path(output: &Path) -> PathBuf {
    output.join("settings.json")
}

fn read_close_wechat_tab(output: &Path) -> bool {
    std::fs::read_to_string(settings_path(output))
        .ok()
        .map(|text| close_wechat_tab_from_json(&text))
        .unwrap_or(false)
}

pub(crate) fn close_wechat_tab_from_json(text: &str) -> bool {
    serde_json::from_str::<SettingsBody>(text)
        .map(|body| body.close_wechat_tab)
        .unwrap_or(false)
}

async fn write_close_wechat_tab(output: &Path, enabled: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&SettingsBody {
        close_wechat_tab: enabled,
    })
    .map_err(|err| err.to_string())?;
    tokio::fs::write(settings_path(output), bytes)
        .await
        .map_err(|err| err.to_string())
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

#[cfg(test)]
mod tests {
    use super::close_wechat_tab_from_json;

    #[test]
    fn missing_settings_keep_wechat_tab_open() {
        assert!(!close_wechat_tab_from_json(""));
        assert!(!close_wechat_tab_from_json("{}"));
        assert!(!close_wechat_tab_from_json("not json"));
    }

    #[test]
    fn settings_remember_closing_the_wechat_tab() {
        assert!(close_wechat_tab_from_json(r#"{"close_wechat_tab":true}"#));
        assert!(!close_wechat_tab_from_json(
            r#"{"close_wechat_tab":false}"#
        ));
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
