use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{FixedOffset, Utc};
use reqwest::header::{CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use url::Url;

use crate::article::{
    escape_html, ext_from_content_type, ext_from_url, sniff_ext, Article, ImageAsset,
};
use crate::error::ClipError;
use crate::urlutil::{article_key, slugify};

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

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
    client: &reqwest::Client,
) -> Result<SavedArticle, ClipError> {
    let mut index = load_index(output).await?;
    let dir_name = allocate_dir(output, &index, page_url, canonical_url, article);
    let dir = output.join(&dir_name);
    tokio::fs::create_dir_all(dir.join("images"))
        .await
        .map_err(|err| ClipError::Store(format!("创建目录失败：{err}")))?;

    let downloads = download_images(client, source_url, &dir, &article.images).await?;
    let mut markdown = article.to_markdown();
    let mut preview = article.to_preview_html();
    let mut saved = 0;
    let mut failed = 0;
    for item in &downloads {
        if item.saved {
            saved += 1;
        } else {
            failed += 1;
        }
        if item.replacement != item.provisional {
            markdown = markdown.replace(&item.provisional, &item.replacement);
            preview = preview.replace(&item.provisional, &escape_html(&item.replacement));
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
    client: &reqwest::Client,
    referer: &str,
    dir: &Path,
    images: &[ImageAsset],
) -> Result<Vec<DownloadDone>, ClipError> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
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
                .map_err(|err| ClipError::Store(format!("图片任务中断：{err}")))?,
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
        tracing::warn!(url = %image.url, reason, "图片下载失败，Markdown 保留原链接");
        DownloadDone {
            provisional: image.relative_path.clone(),
            replacement: image.url.clone(),
            saved: false,
        }
    };
    let response = match client
        .get(&image.url)
        .header(REFERER, referer)
        .header("Accept", "image/avif,image/webp,image/*,*/*;q=0.8")
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return fail(&err.to_string()),
    };
    if !response.status().is_success() {
        return fail(&format!("HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => return fail(&err.to_string()),
    };
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return fail("图片为空或过大");
    }
    let ext = sniff_ext(&bytes)
        .or_else(|| content_type.as_deref().and_then(ext_from_content_type))
        .unwrap_or_else(|| ext_from_url(&image.url));
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
    }
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
