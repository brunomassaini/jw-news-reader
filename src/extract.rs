use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use url::Url;

use crate::models::ImageInfo;

// ── Constants ────────────────────────────────────────────────────────────────

const USER_AGENT: &str = "jw-news-reader-api/1.0";
const MIN_TEXT_LEN: usize = 200;
const CONTROL_NEEDLES: &[&str] = &["play", "audio", "video"];

// ── Lazy static regexes ──────────────────────────────────────────────────────

static KEYWORD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(article|content|pub|body)").unwrap());

static PLAYER_CLASS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(player|audio|video|jwplayer|vjs|media|play)").unwrap());

static METADATA_CLASS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(publication|issue|magazine|context|related|footer|language|promo|share)")
        .unwrap()
});

static ISSUE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\bwp\d{2}\b").unwrap());

static CMS_IMAGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)https?://cms-imgp\.jw-cdn\.org/img/p/[^\s"'<>]+"#).unwrap()
});

static AKAMAI_IMAGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)https?://assetsnffrgf-a\.akamaihd\.net/assets/[^\s"'<>]+"#).unwrap()
});

static IMAGE_SIZE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)_(xs|s|m|l|xl)(?:\b|\.|_)").unwrap());

static MULTI_NEWLINE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("{0}")]
    InvalidUrl(String),
    #[error("URL did not return HTML")]
    NotHtml,
    #[error("Upstream returned an error")]
    Upstream,
    #[error("{0}")]
    Request(String),
}

// ── Public result type ───────────────────────────────────────────────────────

pub struct ExtractResult {
    pub markdown: String,
    pub title: Option<String>,
    pub source_url: String,
    pub images: Vec<ImageInfo>,
}

// ── Walk context ─────────────────────────────────────────────────────────────

struct WalkCtx<'a> {
    in_article_or_main: bool,
    title: Option<&'a str>,
}

// ── Public API ───────────────────────────────────────────────────────────────

pub async fn extract_article(url: &str) -> Result<ExtractResult, ExtractionError> {
    validate_url(url)?;
    let html = fetch_html(url).await?;
    Ok(extract_from_html(&html, url))
}

// ── URL validation ───────────────────────────────────────────────────────────

fn validate_url(url: &str) -> Result<(), ExtractionError> {
    let parsed = Url::parse(url)
        .map_err(|_| ExtractionError::InvalidUrl("Invalid URL".to_string()))?;
    if parsed.scheme() != "https" {
        return Err(ExtractionError::InvalidUrl(
            "Only https URLs are allowed".to_string(),
        ));
    }
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    if host == "jw.org" || host.ends_with(".jw.org") {
        return Ok(());
    }
    Err(ExtractionError::InvalidUrl(
        "Only jw.org URLs are allowed".to_string(),
    ))
}

// ── HTTP fetch ───────────────────────────────────────────────────────────────

async fn fetch_html(url: &str) -> Result<String, ExtractionError> {
    let insecure = std::env::var("JW_NEWS_READER_INSECURE_SSL").as_deref() == Ok("1");

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
            .parse()
            .unwrap(),
    );
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        "en-US,en;q=0.9".parse().unwrap(),
    );

    let mut builder = reqwest::ClientBuilder::new()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(USER_AGENT)
        .default_headers(headers);

    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    let client = builder
        .build()
        .map_err(|e| ExtractionError::Request(e.to_string()))?;

    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            ExtractionError::Request(format!("TimeoutError: {}", e))
        } else if e.is_connect() {
            ExtractionError::Request(format!("ConnectError: {}", e))
        } else {
            ExtractionError::Request(format!("RequestError: {}", e))
        }
    })?;

    if !response.status().is_success() {
        return Err(ExtractionError::Upstream);
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if !content_type.contains("text/html") {
        return Err(ExtractionError::NotHtml);
    }

    response
        .text()
        .await
        .map_err(|e| ExtractionError::Request(e.to_string()))
}

// ── Main extraction pipeline ─────────────────────────────────────────────────

pub fn extract_from_html(html: &str, base_url: &str) -> ExtractResult {
    let document = Html::parse_document(html);
    let base = Url::parse(base_url).unwrap_or_else(|_| Url::parse("https://jw.org").unwrap());

    // Extract fallback image from the full HTML before any filtering.
    let fallback_image = extract_fallback_image(html, &document, &base);

    // Find the best content container element.
    let (container, fallback_title) = find_container(&document);

    // Resolve title: h1 in container → <title> tag → readability title.
    let title: Option<String> = container
        .and_then(|c| {
            let h1_sel = Selector::parse("h1").unwrap();
            c.select(&h1_sel)
                .next()
                .map(|el| normalize_text(collect_text(el)))
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            let title_sel = Selector::parse("title").unwrap();
            document
                .select(&title_sel)
                .next()
                .map(|el| collect_text(el).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or(fallback_title);

    let ctx = WalkCtx {
        in_article_or_main: true,
        title: title.as_deref(),
    };

    let mut images: Vec<ImageInfo> = Vec::new();
    let mut markdown = String::new();

    if let Some(container) = container {
        markdown = walk_element(container, &base, &mut images, &ctx);
    }

    // Collapse runs of 3+ newlines and trim.
    let markdown = MULTI_NEWLINE_RE
        .replace_all(&markdown, "\n\n")
        .trim()
        .to_string();

    let markdown = match title.as_deref() {
        Some(t) => ensure_markdown_title(&markdown, t),
        None => markdown,
    };

    // Use fallback image if we found no images in the content.
    let (images, markdown) = if images.is_empty() {
        if let Some(mut fb) = fallback_image {
            if fb.alt.is_none() {
                fb.alt = title.clone();
            }
            let md = insert_fallback_image(&markdown, &fb);
            (vec![fb], md)
        } else {
            (images, markdown)
        }
    } else {
        (images, markdown)
    };

    ExtractResult {
        markdown,
        title,
        source_url: base_url.to_string(),
        images,
    }
}

// ── Container selection ──────────────────────────────────────────────────────

fn find_container<'doc>(document: &'doc Html) -> (Option<ElementRef<'doc>>, Option<String>) {
    // 1. Prefer <article>
    let article_sel = Selector::parse("article").unwrap();
    if let Some(el) = document.select(&article_sel).next() {
        return (Some(el), None);
    }

    // 2. Fall back to <main>
    let main_sel = Selector::parse("main").unwrap();
    if let Some(el) = document.select(&main_sel).next() {
        return (Some(el), None);
    }

    // 3. Best <div> with a content-like class/id and sufficient text.
    let div_sel = Selector::parse("div").unwrap();
    let mut best: Option<ElementRef<'doc>> = None;
    let mut best_len: usize = 0;

    for div in document.select(&div_sel) {
        let id = div.value().id().unwrap_or("").to_string();
        let classes = div.value().classes().collect::<Vec<_>>().join(" ");
        let combined = format!("{} {}", id, classes);

        if !KEYWORD_RE.is_match(&combined) {
            continue;
        }

        let text_len = collect_text(div).len();
        if text_len > best_len {
            best_len = text_len;
            best = Some(div);
        }
    }

    if best_len >= MIN_TEXT_LEN {
        return (best, None);
    }

    // 4. Readability fallback: use the <body> element.
    let title_sel = Selector::parse("title").unwrap();
    let fallback_title = document
        .select(&title_sel)
        .next()
        .map(|el| collect_text(el).trim().to_string())
        .filter(|s| !s.is_empty());

    let body_sel = Selector::parse("body").unwrap();
    (document.select(&body_sel).next(), fallback_title)
}

// ── DOM tree walker → Markdown ───────────────────────────────────────────────

fn walk_element(el: ElementRef<'_>, base_url: &Url, images: &mut Vec<ImageInfo>, ctx: &WalkCtx) -> String {
    let name = el.value().name();

    // Hard-skip tags.
    if matches!(
        name,
        "script"
            | "style"
            | "noscript"
            | "svg"
            | "form"
            | "button"
            | "audio"
            | "video"
            | "source"
            | "track"
    ) {
        return String::new();
    }

    // Skip layout / navigation chrome.
    if matches!(name, "nav" | "footer" | "aside") {
        return String::new();
    }

    // Skip <header> unless we are already inside article/main.
    if name == "header" && !ctx.in_article_or_main {
        return String::new();
    }

    // Skip player-control elements by aria-label.
    if let Some(aria) = el.value().attr("aria-label") {
        let lower = aria.to_lowercase();
        if CONTROL_NEEDLES.iter().any(|n| lower.contains(n)) {
            return String::new();
        }
    }

    // Skip player-control elements by title attribute.
    if let Some(title_attr) = el.value().attr("title") {
        let lower = title_attr.to_lowercase();
        if CONTROL_NEEDLES.iter().any(|n| lower.contains(n)) {
            return String::new();
        }
    }

    // Skip role=button|link elements whose only text is "play".
    if let Some(role) = el.value().attr("role") {
        let role_lower = role.to_lowercase();
        if role_lower == "button" || role_lower == "link" {
            let text = normalize_text(collect_text(el)).to_lowercase();
            if text == "play" {
                return String::new();
            }
        }
    }

    // Class/id combined string used in several checks below.
    let id = el.value().id().unwrap_or("").to_string();
    let classes = el.value().classes().collect::<Vec<_>>().join(" ");
    let class_id = format!("{} {}", id, classes);

    // Skip elements with player-like class/id that have no image and little text.
    if PLAYER_CLASS_RE.is_match(&class_id)
        && !has_descendant_of_tag(el, "img")
        && !has_descendant_of_tag(el, "picture")
    {
        let text_len = normalize_text(collect_text(el)).len();
        if text_len <= 20 {
            return String::new();
        }
    }

    // Metadata block filtering for block-level elements.
    if matches!(
        name,
        "section" | "div" | "p" | "ul" | "ol" | "li" | "footer" | "aside"
            | "h2" | "h3" | "h4" | "h5" | "h6"
    ) {
        let text = collect_text(el);
        let normalized = normalize_text(text);
        let is_short = normalized.len() <= 250;

        if is_short {
            // Preserve elements that are/contain the article title.
            let contains_title = ctx
                .title
                .map(|t| element_has_exact_text(el, t))
                .unwrap_or(false);

            if !contains_title {
                if !class_id.trim().is_empty() && METADATA_CLASS_RE.is_match(&class_id) {
                    return String::new();
                }

                let upper = normalized.to_uppercase();
                if upper.contains("THE WATCHTOWER") || upper.contains("AWAKE!") {
                    return String::new();
                }

                if ISSUE_RE.is_match(&normalized)
                    && (normalized.contains("No.")
                        || normalized.contains("pp.")
                        || normalized.contains("pp "))
                {
                    return String::new();
                }

                if let Some(t) = ctx.title {
                    let lower = normalized.to_lowercase();
                    if lower.contains("english") && lower.contains(&t.to_lowercase()) {
                        return String::new();
                    }
                }
            }
        }
    }

    // Context for children: mark if we enter an article or main element.
    let child_ctx = WalkCtx {
        in_article_or_main: ctx.in_article_or_main || matches!(name, "article" | "main"),
        title: ctx.title,
    };

    // Tag-specific markdown rendering.
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name[1..].parse::<usize>().unwrap_or(1);
            let text = normalize_text(collect_text(el));
            if text.is_empty() {
                return String::new();
            }
            format!("{} {}\n\n", "#".repeat(level), text)
        }

        "figure" => handle_figure(el, base_url, images, &child_ctx),

        "img" => {
            if let Some(src) = resolve_img_src(el, base_url) {
                let alt = el
                    .value()
                    .attr("alt")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                images.push(ImageInfo {
                    url: src.clone(),
                    alt: alt.clone(),
                    caption: None,
                });
                format!("![{}]({})\n\n", alt.as_deref().unwrap_or(""), src)
            } else {
                String::new()
            }
        }

        "picture" => {
            if let Some(img) = find_first_tag(el, "img") {
                if let Some(src) = resolve_img_src(img, base_url) {
                    let alt = img
                        .value()
                        .attr("alt")
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    images.push(ImageInfo {
                        url: src.clone(),
                        alt: alt.clone(),
                        caption: None,
                    });
                    return format!("![{}]({})\n\n", alt.as_deref().unwrap_or(""), src);
                }
            }
            String::new()
        }

        "a" => {
            let href = el.value().attr("href").map(|h| {
                base_url
                    .join(h)
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| h.to_string())
            });
            let content = walk_children(el, base_url, images, &child_ctx);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return String::new();
            }
            match href {
                Some(href) => format!("[{}]({})", trimmed, href),
                None => trimmed,
            }
        }

        "p" => {
            let content = walk_children(el, base_url, images, &child_ctx);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return String::new();
            }
            format!("{}\n\n", trimmed)
        }

        "br" => "\n".to_string(),
        "hr" => "\n---\n\n".to_string(),

        "ul" => handle_list(el, base_url, images, &child_ctx, false),
        "ol" => handle_list(el, base_url, images, &child_ctx, true),

        "li" => {
            let content = walk_children(el, base_url, images, &child_ctx);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return String::new();
            }
            format!("- {}\n", trimmed)
        }

        "strong" | "b" => {
            let content = walk_children(el, base_url, images, &child_ctx);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return String::new();
            }
            format!("**{}**", trimmed)
        }

        "em" | "i" => {
            let content = walk_children(el, base_url, images, &child_ctx);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                return String::new();
            }
            format!("*{}*", trimmed)
        }

        "blockquote" => {
            let content = walk_children(el, base_url, images, &child_ctx);
            let quoted = content
                .lines()
                .map(|l| format!("> {}", l))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n\n", quoted)
        }

        "pre" => {
            let text = collect_text(el);
            format!("```\n{}\n```\n\n", text)
        }

        "code" => {
            let text = collect_text(el);
            format!("`{}`", text)
        }

        _ => walk_children(el, base_url, images, &child_ctx),
    }
}

fn walk_children(
    el: ElementRef<'_>,
    base_url: &Url,
    images: &mut Vec<ImageInfo>,
    ctx: &WalkCtx,
) -> String {
    use scraper::node::Node;
    let mut result = String::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => {
                result.push_str(&*text.text);
            }
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    result.push_str(&walk_element(child_el, base_url, images, ctx));
                }
            }
            _ => {}
        }
    }
    result
}

// ── Element-specific handlers ────────────────────────────────────────────────

fn handle_figure(
    el: ElementRef<'_>,
    base_url: &Url,
    images: &mut Vec<ImageInfo>,
    _ctx: &WalkCtx,
) -> String {
    let img = match find_first_tag(el, "img") {
        Some(i) => i,
        None => return String::new(),
    };
    let src = match resolve_img_src(img, base_url) {
        Some(s) => s,
        None => return String::new(),
    };

    let alt = img
        .value()
        .attr("alt")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let caption = find_first_tag(el, "figcaption")
        .map(|fc| normalize_text(collect_text(fc)))
        .filter(|s| !s.is_empty());

    images.push(ImageInfo {
        url: src.clone(),
        alt: alt.clone(),
        caption: caption.clone(),
    });

    let alt_str = alt.as_deref().unwrap_or("");
    let mut result = format!("![{}]({})\n\n", alt_str, src);
    if let Some(cap) = caption {
        result.push_str(&format!("*{}*\n\n", cap));
    }
    result
}

fn handle_list(
    el: ElementRef<'_>,
    base_url: &Url,
    images: &mut Vec<ImageInfo>,
    ctx: &WalkCtx,
    ordered: bool,
) -> String {
    use scraper::node::Node;
    let mut result = String::new();
    let mut idx = 1usize;

    for child in el.children() {
        if let Node::Element(_) = child.value() {
            if let Some(child_el) = ElementRef::wrap(child) {
                if child_el.value().name() == "li" {
                    let content = walk_children(child_el, base_url, images, ctx);
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        if ordered {
                            result.push_str(&format!("{}. {}\n", idx, trimmed));
                            idx += 1;
                        } else {
                            result.push_str(&format!("- {}\n", trimmed));
                        }
                    }
                }
            }
        }
    }

    if !result.is_empty() {
        result.push('\n');
    }
    result
}

// ── Image helpers ────────────────────────────────────────────────────────────

fn resolve_img_src(el: ElementRef<'_>, base_url: &Url) -> Option<String> {
    let v = el.value();

    // Priority: data-src → src → data-original|largest|large|medium|small|smallest → srcset.
    let src: Option<String> = v
        .attr("data-src")
        .or_else(|| v.attr("src"))
        .map(|s| s.to_string())
        .or_else(|| {
            [
                "data-original",
                "data-largest",
                "data-large",
                "data-medium",
                "data-small",
                "data-smallest",
            ]
            .iter()
            .find_map(|a| v.attr(a).map(|s| s.to_string()))
        })
        .or_else(|| {
            v.attr("srcset")
                .or_else(|| v.attr("data-srcset"))
                .and_then(best_src_from_srcset)
        });

    let src = src?;
    base_url.join(&src).ok().map(|u| u.to_string())
}

fn best_src_from_srcset(srcset: &str) -> Option<String> {
    let mut candidates: Vec<(f64, usize, String)> = Vec::new();

    for (index, part) in srcset.split(',').enumerate() {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let pieces: Vec<&str> = part.split_whitespace().collect();
        let url = pieces[0].to_string();
        let score: f64 = if pieces.len() > 1 {
            let desc = pieces[1];
            if desc.ends_with('w') || desc.ends_with('x') {
                desc[..desc.len() - 1].parse().unwrap_or(0.0)
            } else {
                0.0
            }
        } else {
            0.0
        };
        candidates.push((score, index, url));
    }

    candidates.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    candidates.last().map(|(_, _, url)| url.clone())
}

fn score_image_url(url: &str) -> i32 {
    IMAGE_SIZE_RE
        .captures(url)
        .map(|cap| match cap[1].to_lowercase().as_str() {
            "xs" => 1,
            "s" => 2,
            "m" => 3,
            "l" => 4,
            "xl" => 5,
            _ => 0,
        })
        .unwrap_or(0)
}

fn pick_best_image_url(urls: &[String]) -> Option<String> {
    if urls.is_empty() {
        return None;
    }
    // Higher score wins; for equal scores prefer later index (like Python's sort on (-idx)).
    urls.iter()
        .enumerate()
        .max_by_key(|(idx, url)| (score_image_url(url), *idx))
        .map(|(_, url)| url.clone())
}

// ── Fallback image extraction (from full HTML / document) ────────────────────

fn extract_fallback_image(html: &str, document: &Html, base_url: &Url) -> Option<ImageInfo> {
    if let Some(url) = extract_meta_image(document) {
        let abs = base_url.join(&url).ok().map(|u| u.to_string()).unwrap_or(url);
        return Some(ImageInfo { url: abs, alt: None, caption: None });
    }

    if let Some(url) = extract_jsonld_image(document) {
        let abs = base_url.join(&url).ok().map(|u| u.to_string()).unwrap_or(url);
        return Some(ImageInfo { url: abs, alt: None, caption: None });
    }

    if let Some((url, alt)) = extract_image_link(document, base_url) {
        return Some(ImageInfo { url, alt, caption: None });
    }

    let cms: Vec<String> = CMS_IMAGE_RE
        .find_iter(html)
        .map(|m| m.as_str().to_string())
        .collect();
    if let Some(best) = pick_best_image_url(&cms) {
        return Some(ImageInfo { url: best, alt: None, caption: None });
    }

    let akamai: Vec<String> = AKAMAI_IMAGE_RE
        .find_iter(html)
        .map(|m| m.as_str().to_string())
        .collect();
    if let Some(best) = pick_best_image_url(&akamai) {
        return Some(ImageInfo { url: best, alt: None, caption: None });
    }

    None
}

fn extract_meta_image(document: &Html) -> Option<String> {
    let checks = [
        ("property", "og:image"),
        ("property", "og:image:secure_url"),
        ("name", "twitter:image"),
        ("name", "twitter:image:src"),
        ("itemprop", "image"),
    ];
    for (attr, value) in &checks {
        let sel_str = format!("meta[{}=\"{}\"]", attr, value);
        // Use .ok() immediately to drop SelectorErrorKind<'_> before sel_str is dropped.
        let sel = Selector::parse(&sel_str).ok();
        if let Some(sel) = sel {
            if let Some(el) = document.select(&sel).next() {
                if let Some(content) = el.value().attr("content") {
                    let trimmed = content.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }
    None
}

fn extract_jsonld_image(document: &Html) -> Option<String> {
    let sel = Selector::parse("script[type=\"application/ld+json\"]").unwrap();
    for script in document.select(&sel) {
        let text = collect_text(script);
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(url) = jsonld_image_value(&value) {
                return Some(url);
            }
        }
    }
    None
}

fn jsonld_image_value(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in &["image", "thumbnailUrl"] {
                if let Some(v) = map.get(*key) {
                    match v {
                        Value::String(s) => return Some(s.clone()),
                        Value::Array(arr) => {
                            for item in arr {
                                match item {
                                    Value::String(s) => return Some(s.clone()),
                                    Value::Object(obj) => {
                                        if let Some(Value::String(u)) = obj.get("url") {
                                            return Some(u.clone());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Value::Object(obj) => {
                            if let Some(Value::String(u)) = obj.get("url") {
                                return Some(u.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            for nested in map.values() {
                if let Some(url) = jsonld_image_value(nested) {
                    return Some(url);
                }
            }
            None
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(url) = jsonld_image_value(item) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_image_link(document: &Html, base_url: &Url) -> Option<(String, Option<String>)> {
    let sel = Selector::parse("a").unwrap();
    for anchor in document.select(&sel) {
        let text = normalize_text(collect_text(anchor));
        if text.starts_with("Image:") {
            if let Some(href) = anchor.value().attr("href") {
                let abs = base_url.join(href).ok()?.to_string();
                let alt_text = text["Image:".len()..].trim().to_string();
                let alt = if alt_text.is_empty() { None } else { Some(alt_text) };
                return Some((abs, alt));
            }
        }
    }
    None
}

// ── Markdown post-processing ─────────────────────────────────────────────────

fn ensure_markdown_title(markdown: &str, title: &str) -> String {
    let expected = format!("# {}", title);
    let lines: Vec<&str> = markdown.lines().collect();
    for (idx, &line) in lines.iter().enumerate() {
        if !line.trim().is_empty() {
            let stripped = line.trim();
            if stripped == expected {
                return markdown.to_string();
            }
            if stripped == title {
                let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
                result[idx] = expected;
                return result.join("\n");
            }
            return markdown.to_string();
        }
    }
    markdown.to_string()
}

fn insert_fallback_image(markdown: &str, image: &ImageInfo) -> String {
    let alt = image.alt.as_deref().unwrap_or("");
    let image_md = format!("![{}]({})", alt, image.url);

    if markdown.trim().is_empty() {
        return image_md;
    }

    let lines: Vec<&str> = markdown.lines().collect();
    for (idx, &line) in lines.iter().enumerate() {
        if !line.trim().is_empty() {
            if line.starts_with("# ") {
                let head = lines[..=idx].join("\n");
                let tail = lines[idx + 1..].join("\n");
                let tail = tail.trim();
                if !tail.is_empty() {
                    return format!("{}\n\n{}\n\n{}", head, image_md, tail);
                }
                return format!("{}\n\n{}", head, image_md);
            }
            return format!("{}\n\n{}", image_md, markdown);
        }
    }
    format!("{}\n\n{}", image_md, markdown)
}

// ── DOM utility helpers ──────────────────────────────────────────────────────

/// Recursively collect all text from an element and its descendants.
fn collect_text(el: ElementRef<'_>) -> String {
    use scraper::node::Node;
    let mut parts = Vec::new();
    for child in el.children() {
        match child.value() {
            Node::Text(text) => parts.push((&*text.text).to_string()),
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    parts.push(collect_text(child_el));
                }
            }
            _ => {}
        }
    }
    parts.join("")
}

/// Collapse whitespace and trim — equivalent to Python's `" ".join(text.split())`.
fn normalize_text(text: String) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Return true if any descendant element (or the element itself) has
/// normalized text that exactly matches `target`.
fn element_has_exact_text(el: ElementRef<'_>, target: &str) -> bool {
    use scraper::node::Node;
    let text = normalize_text(collect_text(el));
    if text.trim() == target {
        return true;
    }
    for child in el.children() {
        if let Node::Element(_) = child.value() {
            if let Some(child_el) = ElementRef::wrap(child) {
                if element_has_exact_text(child_el, target) {
                    return true;
                }
            }
        }
    }
    false
}

/// Depth-first search for the first element with the given tag name.
fn find_first_tag<'a>(el: ElementRef<'a>, tag: &str) -> Option<ElementRef<'a>> {
    use scraper::node::Node;
    for child in el.children() {
        if let Node::Element(_) = child.value() {
            if let Some(child_el) = ElementRef::wrap(child) {
                if child_el.value().name() == tag {
                    return Some(child_el);
                }
                if let Some(found) = find_first_tag(child_el, tag) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Return true if the element has any descendant with the given tag name.
fn has_descendant_of_tag(el: ElementRef<'_>, tag: &str) -> bool {
    find_first_tag(el, tag).is_some()
}
