use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, REFERER, USER_AGENT};
use reqwest::{redirect, Client};

use url::Url;

use crate::article::{looks_blocked, zhihu_has_body};
use crate::urlutil::{host_allowed, is_zhihu_column, is_zhihu_host, media_redirect_allowed};

pub const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
const WECHAT_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.50(0x18003232) NetType/WIFI Language/zh_CN";

const MAX_HTML_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug)]
pub enum FetchFail {
    Blocked,
    Message(String),
}

pub fn build_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_UA));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(std::io::Error::other("重定向次数过多"));
            }
            let host = attempt.url().host_str().unwrap_or("");
            if host_allowed(host) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .expect("创建 HTTP 客户端失败")
}

pub fn build_media_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_UA));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(std::io::Error::other("重定向次数过多"));
            }
            if media_redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::other("媒体跳转到了非公网地址"))
            }
        }))
        .build()
        .expect("创建媒体下载客户端失败")
}

pub async fn fetch_article(client: &Client, url: &str) -> Result<String, FetchFail> {
    if is_zhihu_column_url(url) {
        return fetch_zhihu(url).await;
    }
    let first = get_html(client, url, CHROME_UA).await?;
    if has_article(&first) {
        return Ok(first);
    }
    let second = get_html(client, url, WECHAT_UA).await?;
    if has_article(&second) {
        return Ok(second);
    }
    if looks_blocked(&first) || looks_blocked(&second) {
        Err(FetchFail::Blocked)
    } else {
        Err(FetchFail::Message("页面里没有文章正文".into()))
    }
}

async fn get_html(client: &Client, url: &str, user_agent: &str) -> Result<String, FetchFail> {
    let response = client
        .get(url)
        .header(USER_AGENT, user_agent)
        .header(REFERER, "https://mp.weixin.qq.com/")
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(|err| FetchFail::Message(format!("网络错误：{err}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| FetchFail::Message(format!("读取页面失败：{err}")))?;
    if text.len() > MAX_HTML_BYTES {
        return Err(FetchFail::Message("页面过大".into()));
    }
    if !status.is_success() {
        return Err(FetchFail::Message(format!("HTTP {status}")));
    }
    Ok(text)
}

fn has_article(html: &str) -> bool {
    html.contains("id=\"js_content\"") || html.contains("id='js_content'")
}

fn is_zhihu_column_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .is_some_and(|url| is_zhihu_column(&url))
}

pub fn build_zhihu_client() -> Client {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_UA));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(std::io::Error::other("重定向次数过多"));
            }
            let host = attempt.url().host_str().unwrap_or("");
            if is_zhihu_host(host) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .expect("创建知乎 HTTP 客户端失败")
}

async fn fetch_zhihu(url: &str) -> Result<String, FetchFail> {
    let client = build_zhihu_client();
    let response = client
        .get(url)
        .header(REFERER, "https://zhuanlan.zhihu.com/")
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(|err| FetchFail::Message(format!("网络错误：{err}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| FetchFail::Message(format!("读取页面失败：{err}")))?;
    if text.len() > MAX_HTML_BYTES {
        return Err(FetchFail::Message("页面过大".into()));
    }
    if status.is_success() && zhihu_has_body(&text) {
        Ok(text)
    } else {
        Err(FetchFail::Blocked)
    }
}

#[cfg(test)]
fn zhihu_response_needs_page(html: &str) -> bool {
    !zhihu_has_body(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zhihu_shell_asks_for_the_open_page() {
        let html = "<html><head><meta id=\"zh-zse-ck\"></head><body>验证</body></html>";
        assert!(zhihu_response_needs_page(html));
        assert!(!zhihu_response_needs_page(
            "<div class=\"Post-RichText\"><p>正文</p></div>"
        ));
    }
}
