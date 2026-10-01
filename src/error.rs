use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClipError {
    #[error("这不是支持的文章链接")]
    BadUrl,
    #[error("抓取失败：{0}")]
    Fetch(String),
    #[error("直接抓链接被拦住了，需要浏览器里已经打开的页面")]
    Blocked,
    #[error("页面里没有文章正文")]
    Empty,
    #[error("保存失败：{0}")]
    Store(String),
}

impl ClipError {
    pub fn status_code(&self) -> &'static str {
        match self {
            ClipError::BadUrl => "bad_url",
            ClipError::Fetch(_) => "fetch_error",
            ClipError::Blocked => "blocked",
            ClipError::Empty => "empty",
            ClipError::Store(_) => "store_error",
        }
    }

    pub fn need_html(&self) -> bool {
        matches!(self, ClipError::Blocked)
    }
}
