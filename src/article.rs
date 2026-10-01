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
  body {{ margin: 0; background: #f4f4f4; color: #222; font: 16px/1.6 "PingFang SC", "Hiragino Sans GB", "Noto Sans SC", sans-serif; }}
  main {{ max-width: 677px; margin: 0 auto; padding: 28px 16px 80px; background: #fff; }}
  .title {{ font-size: 1.55rem; line-height: 1.35; margin: 0 0 0.6rem; }}
  .meta {{ color: #888; font-size: 14px; line-height: 1.6; margin: 0 0 1.5rem; }}
  .meta a {{ color: #576b95; }}
  .article {{ overflow-wrap: break-word; }}
  .article img {{ max-width: 100%; }}
</style>
</head>
<body>
<main>
  <h1 class="title">{title}</h1>
  <p class="meta">{meta}<br><a href="{source}">原文</a></p>
  <div class="article">{body}</div>
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
    let body_html = serialize_preview(&mut builder, content);
    let blocks = render_children_blocks(&mut builder, content);
    let body_markdown = join_markdown(&blocks);
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
    let mut blocks = background_blocks(builder, el);
    blocks.extend(render_element_body(builder, el));
    blocks
}

fn render_element_body(builder: &mut Builder, el: ElementRef) -> Vec<Block> {
    let name = el.value().name();
    if is_horizontal_row(el) {
        return render_row_children(builder, el);
    }
    match name {
        "script" | "style" | "noscript" | "button" | "svg" | "form" | "input" | "textarea"
        | "canvas" | "iframe" => embed_or_empty(el),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name.as_bytes()[1] - b'0';
            let inlines = hoist_images(flatten_spans(render_inline_children(builder, el)));
            let mut blocks = Vec::new();
            let mut buf = Vec::new();
            for inline in inlines {
                if matches!(inline, Inline::Image { .. }) {
                    if !inline_is_empty(&buf) {
                        blocks.push(Block::Heading {
                            level,
                            inlines: std::mem::take(&mut buf),
                        });
                    } else {
                        buf.clear();
                    }
                    blocks.push(Block::Paragraph(vec![inline]));
                } else {
                    buf.push(inline);
                }
            }
            if !inline_is_empty(&buf) {
                blocks.push(Block::Heading {
                    level,
                    inlines: buf,
                });
            }
            blocks
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

fn render_row_children(builder: &mut Builder, el: ElementRef) -> Vec<Block> {
    let mut blocks = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => {
                let inlines = text_inlines(text);
                if inline_is_empty(&inlines) {
                    continue;
                }
                blocks.extend(blocks_from_inlines(inlines));
            }
            Node::Element(_) => {
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                if is_hidden(child_el) {
                    continue;
                }
                let name = child_el.value().name();
                if is_horizontal_row(child_el) || !is_inline_tag(name) {
                    blocks.extend(render_element(builder, child_el));
                } else if let Some(inline) = render_inline_element(builder, child_el) {
                    blocks.extend(blocks_from_inlines(vec![inline]));
                }
            }
            _ => {}
        }
    }
    blocks
}

fn paragraph(inlines: Vec<Inline>) -> Vec<Block> {
    blocks_from_inlines(inlines)
}

fn blocks_from_inlines(inlines: Vec<Inline>) -> Vec<Block> {
    let inlines = hoist_images(flatten_spans(inlines));
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

fn hoist_images(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for inline in inlines {
        out.extend(hoist_one(inline));
    }
    out
}

fn hoist_one(inline: Inline) -> Vec<Inline> {
    match inline {
        Inline::Bold(children) => split_marked(children, Inline::Bold),
        Inline::Italic(children) => split_marked(children, Inline::Italic),
        Inline::Span(children) => hoist_images(children),
        Inline::Link { href, children } => split_marked(children, |inner| Inline::Link {
            href: href.clone(),
            children: inner,
        }),
        other => vec![other],
    }
}

fn split_marked(children: Vec<Inline>, wrap: impl Fn(Vec<Inline>) -> Inline) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for child in hoist_images(children) {
        if matches!(child, Inline::Image { .. }) {
            if !inline_is_empty(&buf) {
                out.push(wrap(std::mem::take(&mut buf)));
            } else {
                buf.clear();
            }
            out.push(child);
        } else {
            buf.push(child);
        }
    }
    if !inline_is_empty(&buf) {
        out.push(wrap(buf));
    }
    out
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
    split_declarations(style).into_iter().any(|(key, value)| {
        let value = value.split('!').next().unwrap_or("").trim();
        (key.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
            || (key.eq_ignore_ascii_case("visibility") && value.eq_ignore_ascii_case("hidden"))
    })
}

fn is_horizontal_row(el: ElementRef) -> bool {
    if !matches!(
        el.value().name(),
        "section" | "div" | "p" | "article" | "span" | "figure"
    ) {
        return false;
    }
    let Some(style) = el.attr("style") else {
        return false;
    };
    split_declarations(style).into_iter().any(|(name, value)| {
        if !name.eq_ignore_ascii_case("display") {
            return false;
        }
        let value = value
            .split('!')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        value.split_whitespace().any(|token| {
            matches!(
                token,
                "flex" | "inline-flex" | "-webkit-flex" | "-webkit-box"
            )
        })
    })
}

fn background_blocks(builder: &mut Builder, el: ElementRef) -> Vec<Block> {
    let Some(style) = el.attr("style") else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    for (name, value) in split_declarations(style) {
        if !is_background_prop(&name) {
            continue;
        }
        for url in wechat_urls_in_css(&value) {
            blocks.push(Block::Paragraph(vec![
                builder.push_image(url, String::new())
            ]));
        }
    }
    blocks
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

fn serialize_preview(builder: &mut Builder, root: ElementRef) -> String {
    let mut out = String::new();
    serialize_children(builder, root, &mut out);
    out
}

fn serialize_children(builder: &mut Builder, el: ElementRef, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&escape_html(text)),
            Node::Element(_) => {
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                serialize_element(builder, child_el, out);
            }
            _ => {}
        }
    }
}

fn serialize_element(builder: &mut Builder, el: ElementRef, out: &mut String) {
    if is_hidden(el) {
        return;
    }
    let name = el.value().name();
    if is_preview_dropped(name) {
        return;
    }
    if !is_preview_tag(name) {
        serialize_children(builder, el, out);
        return;
    }
    if name == "img" {
        serialize_img(builder, el, out);
        return;
    }
    out.push('<');
    out.push_str(name);
    write_preview_attrs(builder, el, out);
    out.push('>');
    if !matches!(name, "br" | "hr") {
        serialize_children(builder, el, out);
        out.push_str("</");
        out.push_str(name);
        out.push('>');
    }
}

fn serialize_img(builder: &mut Builder, el: ElementRef, out: &mut String) {
    let Some(Inline::Image { alt, path }) = image_inline(builder, el) else {
        return;
    };
    out.push_str("<img src=\"");
    out.push_str(&escape_html(&path));
    out.push('"');
    if !alt.is_empty() {
        out.push_str(" alt=\"");
        out.push_str(&escape_html(&alt));
        out.push('"');
    }
    if let Some(style) = sanitize_style(builder, el.attr("style").unwrap_or("")) {
        out.push_str(" style=\"");
        out.push_str(&escape_html(&style));
        out.push('"');
    }
    out.push('>');
}

fn write_preview_attrs(builder: &mut Builder, el: ElementRef, out: &mut String) {
    let name = el.value().name();
    if let Some(style) = sanitize_style(builder, el.attr("style").unwrap_or("")) {
        out.push_str(" style=\"");
        out.push_str(&escape_html(&style));
        out.push('"');
    }
    if name == "a" {
        if let Some(href) = el.attr("href").and_then(normalize_href) {
            out.push_str(" href=\"");
            out.push_str(&escape_html(&href));
            out.push('"');
        }
    }
    if matches!(name, "td" | "th") {
        for key in ["colspan", "rowspan"] {
            if let Some(value) = el.attr(key) {
                if value.bytes().all(|byte| byte.is_ascii_digit())
                    && !value.is_empty()
                    && value.len() <= 3
                {
                    out.push(' ');
                    out.push_str(key);
                    out.push_str("=\"");
                    out.push_str(value);
                    out.push('"');
                }
            }
        }
    }
    if name == "font" {
        if let Some(color) = el.attr("color") {
            if color.len() <= 32 && !color.contains(['"', '\'', '<', '>', '(', ')', ';']) {
                out.push_str(" color=\"");
                out.push_str(&escape_html(color));
                out.push('"');
            }
        }
    }
    if is_svg_tag(name) {
        for (key, value) in el.value().attrs() {
            if let Some(attr) = svg_attr_name(key) {
                out.push(' ');
                out.push_str(attr);
                out.push_str("=\"");
                out.push_str(&escape_html(value));
                out.push('"');
            }
        }
    }
}

fn is_preview_dropped(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "noscript"
            | "button"
            | "form"
            | "input"
            | "textarea"
            | "canvas"
            | "iframe"
    )
}

fn is_preview_tag(name: &str) -> bool {
    matches!(
        name,
        "section"
            | "div"
            | "p"
            | "span"
            | "strong"
            | "b"
            | "em"
            | "i"
            | "u"
            | "s"
            | "del"
            | "sub"
            | "sup"
            | "font"
            | "a"
            | "img"
            | "br"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
            | "code"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "hr"
            | "figure"
            | "figcaption"
    ) || is_svg_tag(name)
}

fn is_svg_tag(name: &str) -> bool {
    matches!(
        name,
        "svg"
            | "g"
            | "path"
            | "circle"
            | "rect"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "defs"
            | "title"
            | "lineargradient"
            | "radialgradient"
            | "stop"
    )
}

fn svg_attr_name(name: &str) -> Option<&'static str> {
    match name {
        "viewbox" => Some("viewBox"),
        "preserveaspectratio" => Some("preserveAspectRatio"),
        "stroke-width" => Some("stroke-width"),
        "fill" => Some("fill"),
        "stroke" => Some("stroke"),
        "d" => Some("d"),
        "cx" => Some("cx"),
        "cy" => Some("cy"),
        "r" => Some("r"),
        "rx" => Some("rx"),
        "ry" => Some("ry"),
        "x" => Some("x"),
        "y" => Some("y"),
        "x1" => Some("x1"),
        "y1" => Some("y1"),
        "x2" => Some("x2"),
        "y2" => Some("y2"),
        "points" => Some("points"),
        "transform" => Some("transform"),
        "opacity" => Some("opacity"),
        "fill-opacity" => Some("fill-opacity"),
        "stroke-opacity" => Some("stroke-opacity"),
        "font-size" => Some("font-size"),
        "text-anchor" => Some("text-anchor"),
        "dx" => Some("dx"),
        "dy" => Some("dy"),
        "offset" => Some("offset"),
        "stop-color" => Some("stop-color"),
        "width" => Some("width"),
        "height" => Some("height"),
        "xmlns" => Some("xmlns"),
        "role" => Some("role"),
        _ => None,
    }
}

fn sanitize_style(builder: &mut Builder, style: &str) -> Option<String> {
    let mut kept = Vec::new();
    for (name, value) in split_declarations(style) {
        if !style_prop_allowed(&name) {
            continue;
        }
        let Some(value) = rewrite_css_value(&value, builder) else {
            continue;
        };
        kept.push(format!("{}:{}", name.to_ascii_lowercase(), value));
    }
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(";"))
    }
}

fn style_prop_allowed(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "color"
            | "letter-spacing"
            | "line-height"
            | "text-align"
            | "text-decoration"
            | "text-indent"
            | "vertical-align"
            | "white-space"
            | "word-break"
            | "word-wrap"
            | "overflow"
            | "overflow-wrap"
            | "box-shadow"
            | "box-sizing"
            | "clear"
            | "float"
            | "opacity"
            | "visibility"
            | "display"
            | "position"
            | "top"
            | "right"
            | "bottom"
            | "left"
            | "z-index"
            | "outline"
            | "gap"
            | "row-gap"
            | "column-gap"
            | "align-items"
            | "align-self"
            | "align-content"
            | "justify-content"
            | "justify-items"
            | "flex"
            | "flex-basis"
            | "flex-grow"
            | "flex-shrink"
            | "flex-direction"
            | "flex-wrap"
            | "flex-flow"
            | "width"
            | "height"
            | "max-width"
            | "min-width"
            | "max-height"
            | "min-height"
            | "font"
            | "font-size"
            | "font-family"
            | "font-weight"
            | "font-style"
            | "border"
            | "border-radius"
            | "background"
            | "background-color"
            | "background-image"
            | "background-size"
            | "background-repeat"
            | "background-position"
            | "background-origin"
            | "margin"
            | "padding"
            | "-webkit-box-flex"
            | "-webkit-box-orient"
            | "-webkit-box-align"
            | "-webkit-box-pack"
            | "-webkit-box-direction"
            | "-webkit-flex"
            | "-webkit-flex-direction"
            | "-webkit-justify-content"
            | "-webkit-align-items"
            | "-webkit-tap-highlight-color"
    ) {
        return true;
    }
    let bare = name.strip_prefix("-webkit-").unwrap_or(&name);
    bare.starts_with("margin-")
        || bare.starts_with("padding-")
        || bare.starts_with("border-")
        || bare.starts_with("background-")
        || bare.starts_with("flex-")
        || bare.starts_with("font-")
}

fn is_background_prop(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name == "background" || name.starts_with("background-")
}

fn rewrite_css_value(value: &str, builder: &mut Builder) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("expression(")
        || lower.contains("javascript:")
        || lower.contains("behavior:")
        || lower.contains("@import")
    {
        return None;
    }
    if !lower.contains("url(") {
        let trimmed = value.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    let mut out = String::new();
    let mut rest = value;
    loop {
        let lower_rest = rest.to_ascii_lowercase();
        let Some(index) = lower_rest.find("url(") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..index]);
        rest = &rest[index + 4..];
        let (raw, next) = split_css_url(rest)?;
        rest = next;
        let Some(url) = normalize_img_url(raw.trim()) else {
            return None;
        };
        if !is_wechat_cdn(&url) {
            return None;
        }
        let Inline::Image { path, .. } = builder.push_image(url, String::new()) else {
            return None;
        };
        out.push_str("url('");
        out.push_str(&path);
        out.push_str("')");
    }
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn wechat_urls_in_css(value: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = value;
    loop {
        let lower_rest = rest.to_ascii_lowercase();
        let Some(index) = lower_rest.find("url(") else {
            break;
        };
        rest = &rest[index + 4..];
        let Some((raw, next)) = split_css_url(rest) else {
            break;
        };
        rest = next;
        if let Some(url) = normalize_img_url(raw.trim()) {
            if is_wechat_cdn(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

fn split_css_url(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim_start();
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first == '"' || first == '\'' {
        let mut end = None;
        for (index, ch) in chars {
            if ch == first {
                end = Some(index);
                break;
            }
        }
        let end = end?;
        let body = &rest[first.len_utf8()..end];
        let after = rest[end + first.len_utf8()..].trim_start();
        let after = after.strip_prefix(')')?;
        return Some((body, after));
    }
    let end = rest.find(')')?;
    Some((&rest[..end], &rest[end + 1..]))
}

fn split_declarations(style: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut quote = None;
    for ch in style.chars() {
        match ch {
            '\'' | '"' if quote == Some(ch) => {
                quote = None;
                current.push(ch);
            }
            '\'' | '"' if quote.is_none() => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' if quote.is_none() => {
                depth += 1;
                current.push(ch);
            }
            ')' if quote.is_none() && depth > 0 => {
                depth -= 1;
                current.push(ch);
            }
            ';' if quote.is_none() && depth == 0 => {
                push_declaration(&mut out, &current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    push_declaration(&mut out, &current);
    out
}

fn push_declaration(out: &mut Vec<(String, String)>, raw: &str) {
    let Some((name, value)) = raw.split_once(':') else {
        return;
    };
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() {
        return;
    }
    out.push((name.to_string(), value.to_string()));
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
    fn image_inside_bold_is_not_wrapped_in_markers() {
        let html = r#"<div id="js_content"><p><strong><strong><img data-src="https://mmbiz.qpic.cn/mmbiz_png/abc/640?wx_fmt=png" alt="图"></strong>来了来了！</strong></p></div>"#;
        let article = parse_article(html, "https://mp.weixin.qq.com/s/abc").unwrap();
        assert_eq!(
            article.body_markdown,
            "![图](images/img-001.png)\n\n**来了来了！**\n"
        );
        assert!(!article.body_markdown.contains("**!["));
    }

    #[test]
    fn keeps_flex_card_and_background_image() {
        let html = r#"<div id="js_content">
<section style="display:flex;color:rgb(34, 34, 34)">
  <span style="flex:0 0 2cm">1</span>
  <span style="flex:1 1 auto" onclick="alert(1)">来了来了！</span>
  <img data-src="https://mmbiz.qpic.cn/mmbiz_jpg/card/0?wx_fmt=jpeg" alt="图">
</section>
<section style="display:flex;background-image:url(https://mmbiz.qpic.cn/mmbiz_png/bg/0?wx_fmt=png)">
  <span>2</span>
  <span>困</span>
</section>
<script>alert(1)</script>
<p style="display:none">隐藏</p>
<p style="visibility:hidden">看不见</p>
<p style="background-image:url(https://example.com/track.png)">外来背景</p>
</div>"#;
        let article = parse_article(html, "https://mp.weixin.qq.com/s/abc").unwrap();
        assert_eq!(
            article.body_markdown,
            "1\n\n来了来了！\n\n![图](images/img-001.jpg)\n\n![](images/img-002.png)\n\n2\n\n困\n\n外来背景\n"
        );
        assert!(!article.body_markdown.contains("隐藏"));
        assert!(!article.body_markdown.contains("看不见"));
        assert_eq!(article.images.len(), 2);
        assert!(article.images[0].url.contains("wx_fmt=jpeg"));
        assert_eq!(article.images[0].relative_path, "images/img-001.jpg");
        assert!(article.images[1].url.contains("wx_fmt=png"));
        assert_eq!(article.images[1].relative_path, "images/img-002.png");

        let preview = &article.body_html;
        let number = preview.find(">1</span>").unwrap();
        let caption = preview.find("来了来了！").unwrap();
        let photo = preview.find("images/img-001.jpg").unwrap();
        let background = preview.find("images/img-002.png").unwrap();
        let second = preview.find(">2</span>").unwrap();
        let third = preview.find("困").unwrap();
        assert!(
            number < caption
                && caption < photo
                && photo < background
                && background < second
                && second < third
        );
        assert!(preview.contains("display:flex"));
        assert!(preview.contains("flex:0 0 2cm"));
        assert!(preview.contains("color:rgb(34, 34, 34)"));
        assert!(!preview.contains("隐藏"));
        assert!(!preview.contains("看不见"));
        assert!(!preview.contains("onclick"));
        assert!(!preview.contains("alert"));
        assert!(!preview.contains("example.com"));
        let page = article.to_preview_html();
        assert!(page.contains("class=\"article\""));
        assert!(page.contains("max-width: 677px"));
        assert!(!page.contains("p img"));
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
