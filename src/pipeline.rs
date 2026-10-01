use std::path::{Path, PathBuf};

use reqwest::Client;

use crate::article::{looks_blocked, parse_article, ParseError};
use crate::error::ClipError;
use crate::fetch::{self, FetchFail};
use crate::store::{self, ClipRecord};
use crate::urlutil::{canonical_article_url, is_zhihu_column, parse_article_url};

pub struct ClipOutput {
    pub status: &'static str,
    pub message: String,
    pub title: String,
    pub account: String,
    pub dir: String,
    pub markdown_path: String,
    pub preview_path: String,
    pub preview_url: String,
    pub source_url: String,
    pub image_count: usize,
    pub image_failed: usize,
}

pub async fn clip_article(
    client: &Client,
    output: &Path,
    url_raw: &str,
    html_override: Option<String>,
    force: bool,
) -> Result<ClipOutput, ClipError> {
    let page_url = parse_article_url(url_raw)?;
    let canonical = canonical_article_url(&page_url);
    let source_url = page_url.to_string();
    if !force {
        if let Some(record) = store::find_existing(output, &canonical).await? {
            return Ok(output_from_record(
                output,
                record,
                "exists",
                "这篇已经保存过",
            ));
        }
    }

    let html_override = html_override.filter(|html| !html.trim().is_empty());
    let html = if let Some(html) = html_override {
        if !is_zhihu_column(&page_url) && looks_blocked(&html) {
            return Err(ClipError::Blocked);
        }
        if !has_article(&page_url, &html) {
            return Err(ClipError::Empty);
        }
        html
    } else {
        match fetch::fetch_article(client, source_url.as_str()).await {
            Ok(html) => html,
            Err(FetchFail::Blocked) => return Err(ClipError::Blocked),
            Err(FetchFail::Message(message)) => return Err(ClipError::Fetch(message)),
        }
    };

    let article = parse_article(&html, &source_url).map_err(|err| match err {
        ParseError::Blocked => ClipError::Blocked,
        ParseError::NoContent => ClipError::Empty,
    })?;
    tracing::info!(
        url = %canonical,
        title = %article.title,
        images = article.images.len(),
        "文章已解析"
    );
    let saved =
        store::save_article(output, &page_url, &canonical, &source_url, &article, client).await?;
    tracing::info!(path = %saved.markdown_path.display(), images = saved.record.image_count, "文章已保存");
    Ok(output_from_saved(
        saved.record,
        saved.markdown_path,
        saved.preview_path,
    ))
}

fn output_from_saved(
    record: ClipRecord,
    markdown_path: PathBuf,
    preview_path: PathBuf,
) -> ClipOutput {
    ClipOutput {
        status: "saved",
        message: "已保存".into(),
        title: record.title,
        account: record.account,
        dir: record.dir.clone(),
        markdown_path: display_path(&markdown_path),
        preview_path: display_path(&preview_path),
        preview_url: preview_url(&record.dir),
        source_url: record.source_url,
        image_count: record.image_count,
        image_failed: record.image_failed,
    }
}

fn output_from_record(
    output: &Path,
    record: ClipRecord,
    status: &'static str,
    message: &str,
) -> ClipOutput {
    let dir = output.join(&record.dir);
    ClipOutput {
        status,
        message: message.into(),
        title: record.title,
        account: record.account,
        dir: record.dir.clone(),
        markdown_path: display_path(&dir.join("article.md")),
        preview_path: display_path(&dir.join("preview.html")),
        preview_url: preview_url(&record.dir),
        source_url: record.source_url,
        image_count: record.image_count,
        image_failed: record.image_failed,
    }
}

fn has_article(url: &url::Url, html: &str) -> bool {
    if is_zhihu_column(url) {
        crate::article::zhihu_has_body(html)
    } else {
        crate::article::wechat_has_body(html)
    }
}

pub fn display_path(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(relative) = path.strip_prefix(&cwd) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    path.to_string_lossy().to_string()
}

pub fn preview_url(dir: &str) -> String {
    const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'/')
        .add(b'\\')
        .add(b'%');
    format!(
        "/files/{}/preview.html",
        percent_encoding::utf8_percent_encode(dir, PATH_SEGMENT)
    )
}
