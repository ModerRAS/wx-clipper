use std::sync::LazyLock;

use chrono::{DateTime, FixedOffset, Utc};
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use url::Url;

static CREATE_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"create_time\s*=\s*["'](\d+)["']"#).expect("create_time regex"));
static MSG_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"msg_title\s*=\s*'((?:\\'|[^'])*)'"#).expect("msg_title regex"));
static NICKNAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"nickname\s*=\s*htmlDecode\("((?:\\.|[^"\\])*)"\)"#).expect("nickname regex")
});

const CST: i32 = 8 * 3600;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("微信返回了环境验证")]
    Blocked,
    #[error("页面里没有文章正文")]
    NoContent,
}

#[derive(Debug, Clone)]
pub struct ImageAsset {
    pub url: String,
    pub relative_path: String,
}

#[derive(Debug)]
pub struct Article {
    pub title: String,
    pub account: String,
    pub author: String,
    pub published_display: String,
    pub published_cst: Option<DateTime<FixedOffset>>,
    pub digest: String,
    pub cover: String,
    pub source_url: String,
    pub body_markdown: String,
    pub body_html: String,
    pub images: Vec<ImageAsset>,
}

impl Article {
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("---\n");
        push_yaml(&mut out, "title", &self.title);
        push_yaml(&mut out, "account", &self.account);
        push_yaml(&mut out, "author", &self.author);
        push_yaml(&mut out, "published", &self.published_display);
        push_yaml(&mut out, "source", &self.source_url);
        push_yaml(&mut out, "digest", &self.digest);
        push_yaml(&mut out, "cover", &self.cover);
        out.push_str("---\n\n");
        if !self.title.is_empty() {
            out.push_str("# ");
            out.push_str(&escape_inline(&self.title).replace('\n', " "));
            out.push_str("\n\n");
        }
        out.push_str(&self.body_markdown);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    pub fn to_preview_html(&self) -> String {
        let mut meta = Vec::new();
        if !self.account.is_empty() {
            meta.push(escape_html(&self.account));
        }
        if !self.author.is_empty() {
            meta.push(escape_html(&self.author));
        }
        if !self.published_display.is_empty() {
            meta.push(escape_html(&self.published_display));
        }
        let meta = meta.join(" · ");
        format!(
            r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ margin: 0; background: #f4f1ea; color: #1c1915; font: 18px/1.75 "Iowan Old Style", Palatino, "Songti SC", "Noto Serif SC", serif; }}
  main {{ max-width: 42rem; margin: 0 auto; padding: 48px 20px 96px; }}
  h1 {{ font-size: 2rem; line-height: 1.25; margin: 0 0 0.6rem; letter-spacing: -0.02em; }}
  h2, h3, h4 {{ line-height: 1.35; margin: 1.6em 0 0.6em; }}
  .meta {{ color: #6d665c; font: 14px/1.6 "Segoe UI", "PingFang SC", "Noto Sans SC", sans-serif; margin: 0 0 2rem; }}
  .meta a {{ color: #0b6b4f; }}
  img {{ max-width: 100%; height: auto; }}
  p img {{ display: block; margin: 1rem auto; }}
  pre {{ background: #242220; color: #f3efe6; padding: 14px 16px; overflow: auto; border-radius: 10px; font: 13px/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
  code {{ font: inherit; }}
  p code, li code {{ background: #e7e1d6; padding: 0.1em 0.35em; border-radius: 4px; font-size: 0.9em; }}
  blockquote {{ margin: 1.2rem 0; padding: 0.2rem 0 0.2rem 1rem; border-left: 3px solid #c8bfb0; color: #3f3a34; }}
  a {{ color: #0b6b4f; }}
  table {{ border-collapse: collapse; width: 100%; font-size: 0.95rem; }}
  th, td {{ border: 1px solid #d9d1c5; padding: 6px 10px; text-align: left; vertical-align: top; }}
  th {{ background: #e7e0d6; }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #1b1916; color: #f3efe6; }}
    .meta {{ color: #b7b0a4; }}
    .meta a, a {{ color: #8dcfb0; }}
    pre {{ background: #11100e; }}
    p code, li code {{ background: #2c2925; }}
    blockquote {{ border-color: #5c564c; color: #e4ded3; }}
    th, td {{ border-color: #3c3832; }}
    th {{ background: #2a2723; }}
  }}
</style>
</head>
<body>
<main>
  <h1>{title}</h1>
  <p class="meta">{meta}<br><a href="{source}">原文</a></p>
  {body}
</main>
</body>
</html>
"#,
            title = escape_html(&self.title),
            meta = meta,
            source = escape_html(&self.source_url),
            body = self.body_html,
        )
    }
}

pub fn looks_blocked(html: &str) -> bool {
    let has_article = html.contains("id=\"js_content\"") || html.contains("id='js_content'");
    if has_article && (html.contains("activity-name") || html.contains("msg_title")) {
        return false;
    }
    html.contains("环境异常")
        || html.contains("访问过于频繁")
        || html.contains("操作频繁")
        || html.contains("请完成验证")
}

pub fn parse_article(html: &str, source_url: &str) -> Result<Article, ParseError> {
    if looks_blocked(html) {
        return Err(ParseError::Blocked);
    }
    let document = Html::parse_document(html);
    let content_sel = Selector::parse("#js_content").expect("selector");
    let content = document
        .select(&content_sel)
        .next()
        .ok_or(ParseError::NoContent)?;

    let mut builder = Builder::default();
    let blocks = render_children_blocks(&mut builder, content);
    let body_markdown = join_markdown(&blocks);
    let body_html = join_html(&blocks);
    if body_markdown.trim().is_empty() && builder.images.is_empty() {
        return Err(ParseError::NoContent);
    }

    let mut title = coalesce([
        select_text(&document, "#activity-name"),
        nonempty(meta_content(&document, "og:title")),
        capture_js(html, &MSG_TITLE).map(|value| decode_js_string(&value)),
    ]);
    if title.is_empty() {
        title = "未命名文章".into();
    }
    let account = coalesce([
        select_text(&document, "#js_name"),
        capture_js(html, &NICKNAME).map(|value| decode_js_string(&value)),
    ]);
    let author = select_text(&document, "#js_author_name").unwrap_or_default();
    let published_cst = CREATE_TIME
        .captures(html)
        .and_then(|cap| cap.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok())
        .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
        .map(|ts| ts.with_timezone(&FixedOffset::east_opt(CST).expect("cst")));
    let published_display = published_cst
        .map(|ts| ts.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();

    Ok(Article {
        title,
        account,
        author,
        published_display,
        published_cst,
        digest: meta_content(&document, "og:description"),
        cover: meta_content(&document, "og:image"),
        source_url: source_url.to_string(),
        body_markdown,
        body_html,
        images: builder.images,
    })
}

fn nonempty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn coalesce(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn capture_js(html: &str, re: &Regex) -> Option<String> {
    re.captures(html)
        .and_then(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
}

fn decode_js_string(raw: &str) -> String {
    let unescaped = raw
        .replace("\\/", "/")
        .replace("\\'", "'")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");
    html_unescape(&unescaped)
}

fn html_unescape(raw: &str) -> String {
    raw.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn select_text(document: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).expect("selector");
    let text = document.select(&sel).next()?.text().collect::<String>();
    let cleaned = clean_text(&text);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn meta_content(document: &Html, property: &str) -> String {
    let sel = Selector::parse(&format!("meta[property='{property}']")).expect("selector");
    document
        .select(&sel)
        .next()
        .and_then(|el| el.attr("content"))
        .map(clean_text)
        .unwrap_or_default()
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_yaml(out: &mut String, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&yaml_quote(value));
    out.push('\n');
}

fn yaml_quote(value: &str) -> String {
    let flat = value.replace(['\n', '\r'], " ");
    format!("\"{}\"", flat.replace('\\', "\\\\").replace('"', "\\\""))
}

#[derive(Default)]
struct Builder {
    images: Vec<ImageAsset>,
}

enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Code {
        lang: String,
        text: String,
    },
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Table {
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Code(String),
    Link { href: String, children: Vec<Inline> },
    Image { alt: String, path: String },
    LineBreak,
    Span(Vec<Inline>),
}

fn render_children_blocks(builder: &mut Builder, el: ElementRef) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut inline_acc = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => inline_acc.extend(text_inlines(text)),
            Node::Element(_) => {
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                if is_hidden(child_el) {
                    continue;
                }
                let name = child_el.value().name();
                if is_inline_tag(name) {
                    if let Some(inline) = render_inline_element(builder, child_el) {
                        inline_acc.push(inline);
                    }
                } else {
                    flush_inline(&mut inline_acc, &mut blocks);
                    blocks.extend(render_element(builder, child_el));
                }
            }
            _ => {}
        }
    }
    flush_inline(&mut inline_acc, &mut blocks);
    blocks
}

fn flush_inline(inline_acc: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
    if inline_acc.is_empty() {
        return;
    }
    blocks.extend(blocks_from_inlines(std::mem::take(inline_acc)));
}

fn render_element(builder: &mut Builder, el: ElementRef) -> Vec<Block> {
    if is_hidden(el) {
        return Vec::new();
    }
    let name = el.value().name();
    match name {
        "script" | "style" | "noscript" | "button" | "svg" | "form" | "input" | "textarea"
        | "canvas" | "iframe" => embed_or_empty(el),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name.as_bytes()[1] - b'0';
            let inlines = render_inline_children(builder, el);
            if inline_is_empty(&inlines) {
                Vec::new()
            } else {
                vec![Block::Heading { level, inlines }]
            }
        }
        "pre" => {
            let (lang, text) = code_block(el);
            let text = text.trim_matches(['\n', '\r']).to_string();
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![Block::Code { lang, text }]
            }
        }
        "ul" => list_block(builder, el, false),
        "ol" => list_block(builder, el, true),
        "blockquote" => {
            let inner = render_children_blocks(builder, el);
            if inner.is_empty() {
                Vec::new()
            } else {
                vec![Block::Quote(inner)]
            }
        }
        "hr" => vec![Block::Rule],
        "table" => {
            let rows = table_rows(builder, el);
            if rows.is_empty() {
                Vec::new()
            } else {
                vec![Block::Table { rows }]
            }
        }
        "mpvoice" | "mp-common-mpaudio" => voice_block(el),
        "p" => {
            if contains_block_child(el) {
                render_children_blocks(builder, el)
            } else {
                paragraph(render_inline_children(builder, el))
            }
        }
        _ => {
            if !contains_block_child(el) {
                paragraph(render_inline_children(builder, el))
            } else {
                render_children_blocks(builder, el)
            }
        }
    }
}

fn paragraph(inlines: Vec<Inline>) -> Vec<Block> {
    blocks_from_inlines(inlines)
}

fn blocks_from_inlines(inlines: Vec<Inline>) -> Vec<Block> {
    let inlines = flatten_spans(inlines);
    let mut blocks = Vec::new();
    let mut buf = Vec::new();
    for inline in inlines {
        if matches!(inline, Inline::Image { .. }) {
            push_text_paragraph(&mut buf, &mut blocks);
            blocks.push(Block::Paragraph(vec![inline]));
        } else {
            buf.push(inline);
        }
    }
    push_text_paragraph(&mut buf, &mut blocks);
    blocks
}

fn flatten_spans(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Span(children) => out.extend(flatten_spans(children)),
            other => out.push(other),
        }
    }
    out
}

fn push_text_paragraph(buf: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
    if inline_is_empty(buf) {
        buf.clear();
    } else {
        blocks.push(Block::Paragraph(std::mem::take(buf)));
    }
}

fn list_block(builder: &mut Builder, el: ElementRef, ordered: bool) -> Vec<Block> {
    let mut items = Vec::new();
    for child in el.child_elements() {
        if child.value().name() != "li" || is_hidden(child) {
            continue;
        }
        let blocks = render_children_blocks(builder, child);
        if blocks.is_empty() {
            continue;
        }
        items.push(blocks);
    }
    if items.is_empty() {
        Vec::new()
    } else {
        vec![Block::List { ordered, items }]
    }
}

fn table_rows(builder: &mut Builder, el: ElementRef) -> Vec<Vec<Vec<Inline>>> {
    let mut rows = Vec::new();
    collect_rows(builder, el, &mut rows);
    rows
}

fn collect_rows(builder: &mut Builder, el: ElementRef, rows: &mut Vec<Vec<Vec<Inline>>>) {
    for child in el.child_elements() {
        if is_hidden(child) {
            continue;
        }
        match child.value().name() {
            "tr" => {
                let mut cells = Vec::new();
                for cell in child.child_elements() {
                    if matches!(cell.value().name(), "td" | "th") && !is_hidden(cell) {
                        cells.push(render_inline_children(builder, cell));
                    }
                }
                if !cells.is_empty() {
                    rows.push(cells);
                }
            }
            "thead" | "tbody" | "tfoot" => collect_rows(builder, child, rows),
            _ => {}
        }
    }
}

fn voice_block(el: ElementRef) -> Vec<Block> {
    let name = el
        .attr("name")
        .or_else(|| el.attr("data-name"))
        .unwrap_or("音频")
        .trim();
    let label = if name.is_empty() { "音频" } else { name };
    vec![Block::Paragraph(vec![Inline::Text(format!(
        "音频：{label}"
    ))])]
}

fn embed_or_empty(el: ElementRef) -> Vec<Block> {
    if el.value().name() != "iframe" {
        return Vec::new();
    }
    let Some(src) = el.attr("src").or_else(|| el.attr("data-src")) else {
        return Vec::new();
    };
    let Some(href) = normalize_href(src) else {
        return Vec::new();
    };
    vec![Block::Paragraph(vec![Inline::Link {
        href,
        children: vec![Inline::Text("嵌入内容".into())],
    }])]
}

fn render_inline_children(builder: &mut Builder, el: ElementRef) -> Vec<Inline> {
    let mut out = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => out.extend(text_inlines(text)),
            Node::Element(_) => {
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                if is_hidden(child_el) {
                    continue;
                }
                let name = child_el.value().name();
                if matches!(name, "script" | "style" | "button" | "svg" | "noscript") {
                    continue;
                }
                if let Some(inline) = render_inline_element(builder, child_el) {
                    out.push(inline);
                }
            }
            _ => {}
        }
    }
    out
}

fn render_inline_element(builder: &mut Builder, el: ElementRef) -> Option<Inline> {
    if is_hidden(el) {
        return None;
    }
    match el.value().name() {
        "br" => Some(Inline::LineBreak),
        "img" => image_inline(builder, el),
        "strong" | "b" => {
            let inner = render_inline_children(builder, el);
            (!inline_is_empty(&inner)).then_some(Inline::Bold(inner))
        }
        "em" | "i" => {
            let inner = render_inline_children(builder, el);
            (!inline_is_empty(&inner)).then_some(Inline::Italic(inner))
        }
        "code" => {
            let text = raw_inline_text(el);
            let text = tidy_single_line(&text);
            (!text.is_empty()).then_some(Inline::Code(text))
        }
        "a" => {
            let inner = render_inline_children(builder, el);
            match el.attr("href").and_then(normalize_href) {
                Some(href) if !inline_is_empty(&inner) || !href.is_empty() => Some(Inline::Link {
                    href,
                    children: inner,
                }),
                _ if !inline_is_empty(&inner) => Some(Inline::Span(inner)),
                _ => None,
            }
        }
        _ => {
            let inner = render_inline_children(builder, el);
            (!inner.is_empty()).then_some(Inline::Span(inner))
        }
    }
}

fn image_inline(builder: &mut Builder, el: ElementRef) -> Option<Inline> {
    let raw = el
        .attr("data-src")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| el.attr("data-original"))
        .filter(|value| !value.trim().is_empty())
        .or_else(|| el.attr("src"))?;
    let url = normalize_img_url(raw)?;
    let alt = el.attr("alt").unwrap_or("").trim().to_string();
    Some(builder.push_image(url, alt))
}

impl Builder {
    fn push_image(&mut self, url: String, alt: String) -> Inline {
        if is_wechat_cdn(&url) {
            if let Some(existing) = self.images.iter().find(|img| img.url == url) {
                return Inline::Image {
                    alt,
                    path: existing.relative_path.clone(),
                };
            }
            let relative_path = format!(
                "images/img-{:03}.{}",
                self.images.len() + 1,
                ext_from_url(&url)
            );
            self.images.push(ImageAsset {
                url,
                relative_path: relative_path.clone(),
            });
            Inline::Image {
                alt,
                path: relative_path,
            }
        } else {
            Inline::Image { alt, path: url }
        }
    }
}

fn text_inlines(raw: &str) -> Vec<Inline> {
    let replaced = raw.replace('\u{00a0}', " ").replace('\u{200b}', "");
    if replaced.chars().all(char::is_whitespace) {
        if replaced.chars().any(|ch| ch == ' ') {
            return vec![Inline::Text(" ".into())];
        }
        return Vec::new();
    }
    let collapsed = collapse_ws(&replaced);
    if collapsed.is_empty() {
        Vec::new()
    } else {
        vec![Inline::Text(collapsed)]
    }
}

fn collapse_ws(value: &str) -> String {
    let mut out = String::new();
    let mut prev_ws = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            prev_ws = false;
            out.push(ch);
        }
    }
    out
}

fn raw_inline_text(el: ElementRef) -> String {
    let mut out = String::new();
    for piece in el.text() {
        out.push_str(&piece.replace('\u{00a0}', " "));
    }
    out
}

fn code_block(el: ElementRef) -> (String, String) {
    let mut lang = lang_of(el);
    if lang.is_empty() {
        if let Some(code) = el
            .child_elements()
            .find(|child| child.value().name() == "code")
        {
            lang = lang_of(code);
        }
    }
    let mut text = String::new();
    collect_code(el, &mut text);
    (lang, text)
}

fn collect_code(el: ElementRef, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(text) => {
                let value = text.replace('\u{00a0}', " ").replace('\u{200b}', "");
                out.push_str(&value);
            }
            Node::Element(element) => {
                if element.name() == "br" {
                    out.push('\n');
                    continue;
                }
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                collect_code(child_el, out);
                if matches!(element.name(), "p" | "div" | "li" | "section") && !out.ends_with('\n')
                {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
}

fn lang_of(el: ElementRef) -> String {
    let Some(class_name) = el.attr("class") else {
        return String::new();
    };
    for part in class_name.split_whitespace() {
        if let Some(lang) = part
            .strip_prefix("language-")
            .or_else(|| part.strip_prefix("lang-"))
        {
            if valid_lang(lang) {
                return normalize_lang(lang);
            }
        }
        if let Some(lang) = part.strip_prefix("code-snippet__") {
            if valid_lang(lang) && !matches!(lang, "fix" | "scroll" | "code" | "snippet") {
                return normalize_lang(lang);
            }
        }
    }
    if let Some(lang) = el.attr("data-lang") {
        if valid_lang(lang) {
            return normalize_lang(lang);
        }
    }
    String::new()
}

fn normalize_lang(lang: &str) -> String {
    match lang {
        "js" => "javascript".into(),
        "py" => "python".into(),
        "ts" => "typescript".into(),
        "sh" => "bash".into(),
        other => other.to_string(),
    }
}

fn valid_lang(lang: &str) -> bool {
    !lang.is_empty()
        && lang.len() <= 24
        && lang
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '#')
}

fn is_hidden(el: ElementRef) -> bool {
    if el.attr("hidden").is_some() {
        return true;
    }
    let Some(style) = el.attr("style") else {
        return false;
    };
    style.split(';').any(|part| {
        let mut pieces = part.splitn(2, ':');
        let key = pieces.next().unwrap_or("").trim();
        let value = pieces.next().unwrap_or("").trim();
        let value = value.split('!').next().unwrap_or("").trim();
        key.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none")
    })
}

fn is_inline_tag(name: &str) -> bool {
    matches!(
        name,
        "span"
            | "strong"
            | "b"
            | "em"
            | "i"
            | "a"
            | "code"
            | "br"
            | "img"
            | "u"
            | "s"
            | "del"
            | "sub"
            | "sup"
            | "font"
            | "mark"
            | "small"
            | "label"
            | "wbr"
    )
}

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "hr"
            | "figure"
            | "figcaption"
            | "header"
            | "footer"
    )
}

fn contains_block_child(el: ElementRef) -> bool {
    el.child_elements()
        .any(|child| is_block_tag(child.value().name()))
}

fn inline_is_empty(inlines: &[Inline]) -> bool {
    emit_inlines(inlines).trim().is_empty()
}

fn join_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        let piece = emit_markdown_block(block);
        if !piece.is_empty() {
            out.push_str(&piece);
        }
    }
    finish_text(&out)
}

fn join_html(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        out.push_str(&emit_html_block(block));
    }
    out
}

fn finish_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn emit_markdown_block(block: &Block) -> String {
    match block {
        Block::Paragraph(inlines) => {
            let text = tidy_paragraph(&emit_inlines(inlines));
            if text.is_empty() {
                String::new()
            } else {
                format!("{text}\n\n")
            }
        }
        Block::Heading { level, inlines } => {
            let text = tidy_single_line(&emit_inlines(inlines));
            if text.is_empty() {
                String::new()
            } else {
                format!("{} {text}\n\n", "#".repeat(*level as usize))
            }
        }
        Block::Code { lang, text } => {
            let fence = fence_for(text);
            format!("{fence}{lang}\n{text}\n{fence}\n\n")
        }
        Block::List { ordered, items } => {
            let mut out = String::new();
            for (index, item) in items.iter().enumerate() {
                let rendered = join_markdown(item).trim().to_string();
                if rendered.is_empty() {
                    continue;
                }
                let marker = if *ordered {
                    format!("{}. ", index + 1)
                } else {
                    "- ".into()
                };
                let mut lines = rendered.lines();
                if let Some(first) = lines.next() {
                    out.push_str(&marker);
                    out.push_str(first);
                    out.push('\n');
                    for line in lines {
                        if line.is_empty() {
                            out.push('\n');
                        } else {
                            out.push_str("  ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                }
            }
            if out.is_empty() {
                String::new()
            } else {
                out.push('\n');
                out
            }
        }
        Block::Quote(inner) => {
            let rendered = join_markdown(inner);
            let rendered = rendered.trim_end();
            if rendered.is_empty() {
                return String::new();
            }
            let mut out = String::new();
            for line in rendered.lines() {
                if line.is_empty() {
                    out.push_str(">\n");
                } else {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out.push('\n');
            out
        }
        Block::Table { rows } => emit_markdown_table(rows),
        Block::Rule => "---\n\n".into(),
    }
}

fn emit_markdown_table(rows: &[Vec<Vec<Inline>>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let render_row = |cells: &Vec<Vec<Inline>>| {
        let parts: Vec<String> = cells
            .iter()
            .map(|cell| tidy_single_line(&emit_inlines(cell)).replace('|', "\\|"))
            .collect();
        format!("| {} |\n", parts.join(" | "))
    };
    let mut out = render_row(&rows[0]);
    out.push('|');
    for _ in 0..rows[0].len().max(1) {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows.iter().skip(1) {
        out.push_str(&render_row(row));
    }
    out.push('\n');
    out
}

fn emit_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        out.push_str(&emit_inline(inline));
    }
    out
}

fn emit_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) => escape_inline(text),
        Inline::Bold(children) => wrap_affix("**", &emit_inlines(children)),
        Inline::Italic(children) => wrap_affix("*", &emit_inlines(children)),
        Inline::Code(text) => {
            if text.contains('`') {
                format!("`` {text} ``")
            } else {
                format!("`{text}`")
            }
        }
        Inline::Link { href, children } => {
            let mut text = emit_inlines(children).trim().to_string();
            if text.is_empty() {
                text = escape_inline(href);
            }
            format!("[{text}]({})", markdown_dest(href))
        }
        Inline::Image { alt, path } => {
            let alt = alt.replace(['\n', '\r'], " ");
            format!("![{}]({})", escape_inline(&alt), markdown_dest(path))
        }
        Inline::LineBreak => "\n".into(),
        Inline::Span(children) => emit_inlines(children),
    }
}

fn wrap_affix(mark: &str, inner: &str) -> String {
    let inner = inner.trim();
    if inner.is_empty() {
        String::new()
    } else {
        format!("{mark}{inner}{mark}")
    }
}

fn emit_html_block(block: &Block) -> String {
    match block {
        Block::Paragraph(inlines) => {
            let inner = emit_html_inlines(inlines).trim().to_string();
            if inner.is_empty() {
                String::new()
            } else {
                format!("<p>{inner}</p>\n")
            }
        }
        Block::Heading { level, inlines } => {
            let text = emit_html_inlines(inlines).trim().to_string();
            if text.is_empty() {
                String::new()
            } else {
                format!("<h{level}>{text}</h{level}>\n")
            }
        }
        Block::Code { lang, text } => {
            if lang.is_empty() {
                format!("<pre><code>{}</code></pre>\n", escape_html(text))
            } else {
                format!(
                    "<pre><code class=\"language-{lang}\">{}</code></pre>\n",
                    escape_html(text)
                )
            }
        }
        Block::List { ordered, items } => {
            let tag = if *ordered { "ol" } else { "ul" };
            let mut out = format!("<{tag}>\n");
            for item in items {
                out.push_str("<li>\n");
                out.push_str(&join_html(item));
                out.push_str("</li>\n");
            }
            out.push_str(&format!("</{tag}>\n"));
            out
        }
        Block::Quote(inner) => format!("<blockquote>\n{}</blockquote>\n", join_html(inner)),
        Block::Table { rows } => emit_html_table(rows),
        Block::Rule => "<hr>\n".into(),
    }
}

fn emit_html_table(rows: &[Vec<Vec<Inline>>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("<table>\n");
    for (index, row) in rows.iter().enumerate() {
        let cell = if index == 0 { "th" } else { "td" };
        out.push_str("<tr>");
        for item in row {
            out.push('<');
            out.push_str(cell);
            out.push('>');
            out.push_str(emit_html_inlines(item).trim());
            out.push_str("</");
            out.push_str(cell);
            out.push('>');
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
    out
}

fn emit_html_inlines(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        out.push_str(&emit_html_inline(inline));
    }
    out
}

fn emit_html_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) => escape_html(text),
        Inline::Bold(children) => {
            format!("<strong>{}</strong>", emit_html_inlines(children).trim())
        }
        Inline::Italic(children) => format!("<em>{}</em>", emit_html_inlines(children).trim()),
        Inline::Code(text) => format!("<code>{}</code>", escape_html(text)),
        Inline::Link { href, children } => {
            let text = emit_html_inlines(children).trim().to_string();
            let text = if text.is_empty() {
                escape_html(href)
            } else {
                text
            };
            format!(
                "<a href=\"{}\">{}</a>",
                escape_html(&markdown_dest(href)),
                text
            )
        }
        Inline::Image { alt, path } => format!(
            "<img src=\"{}\" alt=\"{}\">",
            escape_html(path),
            escape_html(alt)
        ),
        Inline::LineBreak => "<br>\n".into(),
        Inline::Span(children) => emit_html_inlines(children),
    }
}

fn tidy_paragraph(value: &str) -> String {
    let mut lines: Vec<&str> = value.lines().map(str::trim_end).collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn tidy_single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fence_for(text: &str) -> String {
    let mut size = 3;
    while text.contains(&"`".repeat(size)) {
        size += 1;
    }
    "`".repeat(size)
}

fn escape_inline(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '[' | ']' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

pub fn escape_html(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn markdown_dest(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

fn normalize_href(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("javascript:") || lower.starts_with("weixin:") || href.starts_with('#') {
        return None;
    }
    if href.starts_with("//") {
        return Some(format!("https:{href}"));
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(href.to_string());
    }
    None
}

pub fn normalize_img_url(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches(|ch| ch == '"' || ch == '\'');
    if raw.is_empty() || raw.starts_with("data:") {
        return None;
    }
    let with_scheme = if let Some(rest) = raw.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        raw.to_string()
    };
    let url = Url::parse(&with_scheme).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    Some(url.to_string())
}

pub fn is_wechat_cdn(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host == "mmbiz.qpic.cn" || host.ends_with(".qpic.cn") || host.ends_with(".qlogo.cn")
}

pub fn ext_from_url(url: &str) -> &'static str {
    let Ok(parsed) = Url::parse(url) else {
        return "jpg";
    };
    for (key, value) in parsed.query_pairs() {
        if key == "wx_fmt" {
            return map_ext(&value);
        }
    }
    parsed
        .path()
        .rsplit('.')
        .next()
        .map(map_ext)
        .unwrap_or("jpg")
}

pub fn map_ext(value: &str) -> &'static str {
    match value.to_ascii_lowercase().as_str() {
        "png" => "png",
        "gif" => "gif",
        "webp" => "webp",
        "jpg" | "jpeg" => "jpg",
        _ => "jpg",
    }
}

pub fn ext_from_content_type(content_type: &str) -> Option<&'static str> {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    match content_type {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

pub fn sniff_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some("png");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: &str = r#"你好 **世界**

## 小节

![示意图](images/img-001.png)

```
fn main() {}
let x = 1;
```

- 甲
- 乙

> 引用一句

| 名 | 值 |
| --- | --- |
| 体积 | 6MB |

[文档](https://example.com/docs)

点我

![示意图](images/img-001.png)

![外链](https://example.com/a.png)
"#;

    #[test]
    fn converts_wechat_like_html() {
        let html = include_str!("../tests/fixtures/sample.html");
        let article = parse_article(html, "https://mp.weixin.qq.com/s/abc").unwrap();
        assert_eq!(article.title, "示例标题");
        assert_eq!(article.account, "测试号");
        assert_eq!(article.author, "作者甲");
        assert_eq!(article.published_display, "2026-09-30 07:30:00");
        assert_eq!(article.digest, "摘要");
        assert_eq!(article.body_markdown, EXPECTED);
        assert!(!article.body_markdown.contains("隐藏"));
        assert_eq!(article.images.len(), 1);
        assert!(article.images[0].url.contains("wx_fmt=png"));
        assert_eq!(article.images[0].relative_path, "images/img-001.png");
        assert!(article.body_html.contains("<strong>世界</strong>"));
        assert!(article.body_html.contains("src=\"images/img-001.png\""));
        let markdown = article.to_markdown();
        assert!(markdown.contains("title: \"示例标题\""));
        assert!(markdown.contains("cover: \"https://mmbiz.qpic.cn/cover/0\""));
        assert!(markdown.starts_with("---\n"));
    }

    #[test]
    fn image_inside_span_starts_its_own_block() {
        let html = r#"<div id="js_content"><p><span>结束。<img data-src="https://mmbiz.qpic.cn/mmbiz_png/abc/640?wx_fmt=png" alt="图"></span></p></div>"#;
        let article = parse_article(html, "https://mp.weixin.qq.com/s/abc").unwrap();
        assert_eq!(
            article.body_markdown,
            "结束。\n\n![图](images/img-001.png)\n"
        );
    }

    #[test]
    fn reads_code_snippet_language() {
        let html = r#"<div id="js_content"><pre class="code-snippet__fix code-snippet__js"><code>const a = 1;</code></pre></div>"#;
        let article = parse_article(html, "https://mp.weixin.qq.com/s/abc").unwrap();
        assert!(article
            .body_markdown
            .starts_with("```javascript\nconst a = 1;\n```"));
        assert_eq!(article.title, "未命名文章");
    }

    #[test]
    fn blocked_page_is_not_an_article() {
        let html = "<html><body>当前环境异常，完成验证后即可继续访问</body></html>";
        assert!(matches!(
            parse_article(html, "https://mp.weixin.qq.com/s/abc"),
            Err(ParseError::Blocked)
        ));
    }

    #[test]
    fn protocol_relative_image_url() {
        let url = normalize_img_url("//mmbiz.qpic.cn/a/0?wx_fmt=jpeg").unwrap();
        assert!(url.starts_with("https://mmbiz.qpic.cn/"));
        assert_eq!(ext_from_url(&url), "jpg");
    }
}
