use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{FixedOffset, Utc};
use reqwest::header::{CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use url::Url;

use crate::article::{
    escape_html, ext_for_kind, ext_from_content_type, is_html_content_type, looks_like_html,
    sniff_ext, Article, ImageAsset, MediaKind,
};
use crate::error::ClipError;
use crate::fetch::build_media_client;
use crate::urlutil::{article_key, is_public_media_url, slugify};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipRecord {
    pub canonical_url: String,
    pub source_url: String,
    pub title: String,
    pub account: String,
    pub author: String,
    pub published: String,
    pub dir: String,
    pub saved_at: String,
    pub image_count: usize,
    #[serde(default)]
    pub image_failed: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    clips: Vec<ClipRecord>,
}

pub struct SavedArticle {
    pub record: ClipRecord,
    pub markdown_path: PathBuf,
    pub preview_path: PathBuf,
}

struct DownloadDone {
    provisional: String,
    replacement: String,
    saved: bool,
    kind: MediaKind,
}

pub async fn find_existing(
    output: &Path,
    canonical: &str,
) -> Result<Option<ClipRecord>, ClipError> {
    let index = load_index(output).await?;
    Ok(index
        .clips
        .into_iter()
        .find(|record| record.canonical_url == canonical && markdown_exists(output, &record.dir)))
}

pub async fn list_records(output: &Path) -> Result<Vec<ClipRecord>, ClipError> {
    let mut records = load_index(output).await?.clips;
    records.retain(|record| markdown_exists(output, &record.dir));
    records.sort_by(|left, right| right.saved_at.cmp(&left.saved_at));
    Ok(records)
}

pub fn markdown_exists(output: &Path, dir: &str) -> bool {
    output.join(dir).join("article.md").is_file()
}

pub async fn save_article(
    output: &Path,
    page_url: &Url,
    canonical_url: &str,
    source_url: &str,
    article: &Article,
    _client: &reqwest::Client,
) -> Result<SavedArticle, ClipError> {
    let mut index = load_index(output).await?;
    let dir_name = allocate_dir(output, &index, page_url, canonical_url, article);
    let dir = output.join(&dir_name);
    tokio::fs::create_dir_all(dir.join("images"))
        .await
        .map_err(|err| ClipError::Store(format!("创建目录失败：{err}")))?;

    let downloads = download_images(source_url, &dir, &article.images).await?;
    let mut markdown = article.to_markdown();
    let mut preview = article.to_preview_html();
    let mut saved = 0;
    let mut failed = 0;
    for item in &downloads {
        if item.saved {
            saved += 1;
            if item.replacement != item.provisional {
                markdown = markdown.replace(&item.provisional, &item.replacement);
                preview = preview.replace(
                    &escape_html(&item.provisional),
                    &escape_html(&item.replacement),
                );
            }
        } else {
            failed += 1;
            let label = item.kind.missing_label();
            markdown = scrub_markdown(&markdown, &item.provisional, label);
            preview = scrub_preview(&preview, &item.provisional, label);
        }
    }

    let markdown_path = dir.join("article.md");
    let preview_path = dir.join("preview.html");
    write_atomic(&markdown_path, markdown.as_bytes()).await?;
    write_atomic(&preview_path, preview.as_bytes()).await?;

    let record = ClipRecord {
        canonical_url: canonical_url.to_string(),
        source_url: source_url.to_string(),
        title: article.title.clone(),
        account: article.account.clone(),
        author: article.author.clone(),
        published: article.published_display.clone(),
        dir: dir_name,
        saved_at: Utc::now().to_rfc3339(),
        image_count: saved,
        image_failed: failed,
    };
    if let Some(existing) = index
        .clips
        .iter_mut()
        .find(|item| item.canonical_url == record.canonical_url)
    {
        *existing = record.clone();
    } else {
        index.clips.push(record.clone());
    }
    write_index(output, &index).await?;
    Ok(SavedArticle {
        record,
        markdown_path,
        preview_path,
    })
}

fn allocate_dir(
    output: &Path,
    index: &IndexFile,
    page_url: &Url,
    canonical_url: &str,
    article: &Article,
) -> String {
    if let Some(existing) = index
        .clips
        .iter()
        .find(|record| record.canonical_url == canonical_url)
    {
        return existing.dir.clone();
    }
    let date = article
        .published_cst
        .map(|ts| ts.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| {
            Utc::now()
                .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("cst"))
                .format("%Y-%m-%d")
                .to_string()
        });
    let slug = slugify(&article.title, 24);
    let key = article_key(page_url);
    let desired = if key.is_empty() {
        format!("{date}-{slug}")
    } else {
        format!("{date}-{slug}-{key}")
    };
    if !output.join(&desired).exists() {
        return desired;
    }
    for suffix in 2..100 {
        let candidate = format!("{desired}-{suffix}");
        if !output.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{desired}-dup")
}

async fn download_images(
    referer: &str,
    dir: &Path,
    images: &[ImageAsset],
) -> Result<Vec<DownloadDone>, ClipError> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let client = build_media_client();
    let sem = Arc::new(Semaphore::new(4));
    let mut tasks = Vec::with_capacity(images.len());
    for image in images {
        let client = client.clone();
        let dir = dir.to_path_buf();
        let referer = referer.to_string();
        let image = image.clone();
        let sem = sem.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await;
            download_one(client, dir, referer, image).await
        }));
    }
    let mut done = Vec::with_capacity(tasks.len());
    for task in tasks {
        done.push(
            task.await
                .map_err(|err| ClipError::Store(format!("媒体任务中断：{err}")))?,
        );
    }
    Ok(done)
}

async fn download_one(
    client: reqwest::Client,
    dir: PathBuf,
    referer: String,
    image: ImageAsset,
) -> DownloadDone {
    let fail = |reason: &str| {
        tracing::warn!(url = %image.url, reason, "媒体下载失败，正文改为缺失说明");
        DownloadDone {
            provisional: image.relative_path.clone(),
            replacement: image.relative_path.clone(),
            saved: false,
            kind: image.kind,
        }
    };
    if !is_public_media_url(&image.url) {
        return fail("地址不是公网");
    }
    let response = match client
        .get(&image.url)
        .header(REFERER, referer)
        .header("Accept", "*/*")
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return fail(&err.to_string()),
    };
    if !is_public_media_url(response.url().as_str()) {
        return fail("跳转到了非公网地址");
    }
    if !response.status().is_success() {
        return fail(&format!("HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if content_type.as_deref().is_some_and(is_html_content_type) {
        return fail("响应是网页");
    }
    let max_bytes = image.kind.max_bytes();
    if response
        .content_length()
        .is_some_and(|len| len == 0 || len > max_bytes as u64)
    {
        return fail("媒体为空或过大");
    }
    let mut bytes = Vec::new();
    let mut response = response;
    loop {
        let chunk = match response.chunk().await {
            Ok(chunk) => chunk,
            Err(err) => return fail(&err.to_string()),
        };
        let Some(chunk) = chunk else {
            break;
        };
        if bytes.len() + chunk.len() > max_bytes {
            return fail("媒体为空或过大");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return fail("媒体为空或过大");
    }
    if looks_like_html(&bytes) && sniff_ext(&bytes).is_none() {
        return fail("响应是网页");
    }
    let ext = sniff_ext(&bytes)
        .or_else(|| content_type.as_deref().and_then(ext_from_content_type))
        .unwrap_or_else(|| ext_for_kind(image.kind, &image.url));
    let final_rel = swap_ext(&image.relative_path, ext);
    let path = dir.join(&final_rel);
    if let Some(parent) = path.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return fail(&err.to_string());
        }
    }
    if let Err(err) = tokio::fs::write(&path, &bytes).await {
        return fail(&err.to_string());
    }
    DownloadDone {
        provisional: image.relative_path,
        replacement: final_rel,
        saved: true,
        kind: image.kind,
    }
}

fn scrub_markdown(markdown: &str, path: &str, label: &str) -> String {
    let needle = format!("]({path})");
    let mut out = String::new();
    let mut rest = markdown;
    while let Some(end_rel) = rest.find(&needle) {
        let start_rel = rest[..end_rel].rfind('[').unwrap_or(end_rel);
        let start = if start_rel > 0 && rest.as_bytes()[start_rel - 1] == b'!' {
            start_rel - 1
        } else {
            start_rel
        };
        out.push_str(&rest[..start]);
        out.push_str(label);
        rest = &rest[end_rel + needle.len()..];
    }
    out.push_str(rest);
    out
}

fn scrub_preview(html: &str, path: &str, label: &str) -> String {
    let html = html
        .replace(&format!("url(&#39;{path}&#39;)"), "none")
        .replace(&format!("url('{path}')"), "none")
        .replace(&format!("url(&quot;{path}&quot;)"), "none")
        .replace(&format!("url(\"{path}\")"), "none")
        .replace(&format!("url({path})"), "none");
    let html = scrub_src_elements(&html, path, label);
    scrub_poster(&html, path, label)
}

fn scrub_src_elements(html: &str, path: &str, label: &str) -> String {
    let needle = format!("src=\"{path}\"");
    let mut out = String::new();
    let mut rest = html;
    while let Some(rel) = rest.find(&needle) {
        let Some(start) = rest[..rel].rfind('<') else {
            break;
        };
        let tag = tag_name_at(&rest[start..]);
        let end = if tag == "audio" || tag == "video" {
            find_element_end(rest, start, tag)
        } else {
            find_tag_end(rest, start)
        };
        let element = &rest[start..end];
        out.push_str(&rest[..start]);
        if (tag == "audio" || tag == "video") && element_has_other_src(element, path) {
            out.push_str(label);
            out.push_str(&strip_attr(element, "src", path));
        } else {
            out.push_str(label);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn scrub_poster(html: &str, path: &str, label: &str) -> String {
    let needle = format!("poster=\"{path}\"");
    let mut html = html.to_string();
    while let Some(idx) = html.find(&needle) {
        let start = html[..idx].rfind('<').unwrap_or(idx);
        let mut next = String::new();
        next.push_str(&html[..start]);
        next.push_str(label);
        next.push_str(&html[start..idx]);
        let after_name = idx + "poster=\"".len();
        let value_end = html[after_name..]
            .find('"')
            .map(|offset| after_name + offset + 1)
            .unwrap_or(html.len());
        if html.as_bytes().get(idx.saturating_sub(1)) == Some(&b' ') {
            next.pop();
        }
        next.push_str(&html[value_end..]);
        html = next;
    }
    html
}

fn strip_attr(element: &str, name: &str, path: &str) -> String {
    element
        .replace(&format!(" {name}=\"{path}\""), "")
        .replace(&format!("{name}=\"{path}\""), "")
}

fn element_has_other_src(element: &str, path: &str) -> bool {
    let mut rest = element;
    while let Some(index) = rest.find("src=\"") {
        rest = &rest[index + 5..];
        if let Some(end) = rest.find('"') {
            if &rest[..end] != path {
                return true;
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    false
}

fn tag_name_at(html: &str) -> &str {
    let rest = html.trim_start_matches('<');
    let end = rest
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '>' || ch == '/')
        .unwrap_or(rest.len());
    &rest[..end]
}

fn find_tag_end(html: &str, start: usize) -> usize {
    html[start..]
        .find('>')
        .map(|index| start + index + 1)
        .unwrap_or(html.len())
}

fn find_element_end(html: &str, start: usize, tag: &str) -> usize {
    let close = format!("</{tag}>");
    html[start..]
        .find(&close)
        .map(|index| start + index + close.len())
        .unwrap_or_else(|| find_tag_end(html, start))
}

fn swap_ext(path: &str, ext: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, _)) => format!("{stem}.{ext}"),
        None => format!("{path}.{ext}"),
    }
}

fn index_path(output: &Path) -> PathBuf {
    output.join("index.json")
}

async fn load_index(output: &Path) -> Result<IndexFile, ClipError> {
    let path = index_path(output);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| ClipError::Store(format!("索引损坏：{err}"))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(IndexFile::default()),
        Err(err) => Err(ClipError::Store(format!("读取索引失败：{err}"))),
    }
}

async fn write_index(output: &Path, index: &IndexFile) -> Result<(), ClipError> {
    tokio::fs::create_dir_all(output)
        .await
        .map_err(|err| ClipError::Store(format!("创建目录失败：{err}")))?;
    let bytes = serde_json::to_vec_pretty(index)
        .map_err(|err| ClipError::Store(format!("写入索引失败：{err}")))?;
    write_atomic(&index_path(output), &bytes).await
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ClipError> {
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|err| ClipError::Store(format!("写入 {} 失败：{err}", path.display())))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|err| ClipError::Store(format!("保存 {} 失败：{err}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_download_becomes_missing_text() {
        let markdown = "![瓜](images/img-001.png)\n\n[音频](audio/audio-001.mp3)\n\n![](images/img-002.jpg)\n\n[视频](video/video-001.mp4)\n\n[文档](https://example.com/docs)\n";
        let preview = "<img src=\"images/img-001.png\" alt=\"瓜\"><audio src=\"audio/audio-001.mp3\" controls></audio><video poster=\"images/img-002.jpg\" src=\"video/video-001.mp4\" controls><source src=\"video/video-002.webm\"></video><section style=\"background-image:url(&#39;images/img-002.jpg&#39;)\"></section>";

        let markdown = scrub_markdown(markdown, "images/img-001.png", "图片缺失");
        let preview = scrub_preview(preview, "images/img-001.png", "图片缺失");
        assert_eq!(markdown.lines().next().unwrap(), "图片缺失");
        assert!(preview.contains("图片缺失"));
        assert!(!preview.contains("images/img-001.png"));
        assert!(preview.contains("audio/audio-001.mp3"));

        let markdown = scrub_markdown(&markdown, "audio/audio-001.mp3", "音频缺失");
        let preview = scrub_preview(&preview, "audio/audio-001.mp3", "音频缺失");
        assert!(markdown.contains("音频缺失"));
        assert!(!markdown.contains("audio/audio-001.mp3"));
        assert!(preview.contains("音频缺失"));
        assert!(!preview.contains("<audio"));

        let markdown = scrub_markdown(&markdown, "video/video-001.mp4", "视频缺失");
        let preview = scrub_preview(&preview, "video/video-001.mp4", "视频缺失");
        assert!(markdown.contains("视频缺失"));
        assert!(markdown.contains("![](images/img-002.jpg)"));
        assert!(!markdown.contains("video/video-001.mp4"));
        assert!(markdown.contains("[文档](https://example.com/docs)"));
        assert!(preview.contains("视频缺失"));
        assert!(!preview.contains("video/video-001.mp4"));
        assert!(preview.contains("images/img-002.jpg"));

        let preview = scrub_preview(&preview, "images/img-002.jpg", "图片缺失");
        let markdown = scrub_markdown(&markdown, "images/img-002.jpg", "图片缺失");
        assert!(markdown.contains("图片缺失"));
        assert!(!markdown.contains("images/img-002.jpg"));
        assert!(!preview.contains("images/img-002.jpg"));
        assert!(preview.contains("background-image:none"));
        assert!(!preview.contains("poster="));
        assert!(preview.contains("video/video-002.webm"));
        assert!(!preview.contains("https://"));
    }
}
