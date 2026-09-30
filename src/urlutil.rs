use url::Url;

use crate::error::ClipError;

pub fn parse_article_url(raw: &str) -> Result<Url, ClipError> {
    let raw = raw.trim();
    let mut url = Url::parse(raw).map_err(|_| ClipError::BadUrl)?;
    if url.scheme() == "http" {
        url.set_scheme("https").map_err(|_| ClipError::BadUrl)?;
    }
    if url.scheme() != "https" || url.host_str() != Some("mp.weixin.qq.com") {
        return Err(ClipError::BadUrl);
    }
    let path = url.path().trim_end_matches('/');
    let short = path
        .strip_prefix("/s/")
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let query_article = path == "/s" && url.query().is_some();
    if short.is_none() && !query_article {
        return Err(ClipError::BadUrl);
    }
    Ok(url)
}

pub fn canonical_article_url(url: &Url) -> String {
    let path = url.path().trim_end_matches('/');
    if let Some(id) = path.strip_prefix("/s/") {
        if !id.is_empty() && !id.contains('/') {
            return format!("https://mp.weixin.qq.com/s/{id}");
        }
    }
    let mut out = Url::parse("https://mp.weixin.qq.com/s").expect("canonical base");
    {
        let mut query = out.query_pairs_mut();
        for key in ["__biz", "mid", "idx", "sn"] {
            if let Some((_, value)) = url.query_pairs().find(|(name, _)| name == key) {
                query.append_pair(key, &value);
            }
        }
    }
    out.to_string()
}

pub fn article_key(url: &Url) -> String {
    let path = url.path().trim_end_matches('/');
    if let Some(id) = path.strip_prefix("/s/") {
        if !id.is_empty() && !id.contains('/') {
            return sanitize_id(id);
        }
    }
    if let Some((_, sn)) = url.query_pairs().find(|(name, _)| name == "sn") {
        let sn = sanitize_id(&sn);
        if !sn.is_empty() {
            return sn;
        }
    }
    let mid = url
        .query_pairs()
        .find(|(name, _)| name == "mid")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| "article".into());
    let idx = url
        .query_pairs()
        .find(|(name, _)| name == "idx")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| "1".into());
    sanitize_id(&format!("{mid}-{idx}"))
}

pub fn slugify(title: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.chars().count() >= max_chars {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(32)
        .collect()
}

pub fn host_allowed(host: &str) -> bool {
    host == "mp.weixin.qq.com"
        || host == "mmbiz.qpic.cn"
        || host == "res.wx.qq.com"
        || host.ends_with(".qpic.cn")
        || host.ends_with(".qlogo.cn")
        || host.ends_with(".weixin.qq.com")
        || host.ends_with(".wx.qq.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_link_and_strips_tracking() {
        let url = parse_article_url("https://mp.weixin.qq.com/s/abc123?chksm=1&scene=4").unwrap();
        assert_eq!(
            canonical_article_url(&url),
            "https://mp.weixin.qq.com/s/abc123"
        );
        assert_eq!(article_key(&url), "abc123");
    }

    #[test]
    fn accepts_legacy_query_link() {
        let url = parse_article_url(
            "http://mp.weixin.qq.com/s?__biz=MzA&mid=10&idx=2&sn=snValue&scene=1",
        )
        .unwrap();
        assert!(canonical_article_url(&url).contains("sn=snValue"));
        assert!(canonical_article_url(&url).contains("mid=10"));
        assert!(!canonical_article_url(&url).contains("scene="));
        assert_eq!(article_key(&url), "snValue");
    }

    #[test]
    fn rejects_other_hosts() {
        assert!(parse_article_url("https://example.com/s/abc").is_err());
        assert!(parse_article_url("https://mp.weixin.qq.com/cgi-bin/home").is_err());
    }

    #[test]
    fn slug_keeps_chinese() {
        assert_eq!(slugify("Electron 要慌了？", 20), "Electron-要慌了");
    }
}
