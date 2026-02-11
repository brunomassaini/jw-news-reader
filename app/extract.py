from __future__ import annotations

import json
import os
import re
from typing import Optional, Tuple
from urllib.parse import urljoin, urlparse

import httpx
import certifi
from bs4 import BeautifulSoup, Tag
from readability import Document
import logging


USER_AGENT = "jw-news-reader-api/1.0"
MIN_TEXT_LEN = 200
KEYWORD_RE = re.compile(r"(article|content|pub|body)", re.IGNORECASE)
PLAYER_CLASS_RE = re.compile(r"(player|audio|video|jwplayer|vjs|media|play)", re.IGNORECASE)
METADATA_CLASS_RE = re.compile(
    r"(publication|issue|magazine|context|related|footer|language|promo|share)",
    re.IGNORECASE,
)
ISSUE_RE = re.compile(r"\bwp\d{2}\b", re.IGNORECASE)
CMS_IMAGE_RE = re.compile(r"https?://cms-imgp\.jw-cdn\.org/img/p/[^\s\"'<>]+", re.IGNORECASE)
AKAMAI_IMAGE_RE = re.compile(
    r"https?://assetsnffrgf-a\.akamaihd\.net/assets/[^\s\"'<>]+", re.IGNORECASE
)
LOGGER = logging.getLogger(__name__)


class ExtractionError(ValueError):
    pass


def validate_url(url: str) -> None:
    parsed = urlparse(url)
    if parsed.scheme != "https":
        raise ExtractionError("Only https URLs are allowed")
    host = (parsed.hostname or "").lower()
    if host == "jw.org" or host.endswith(".jw.org"):
        return
    raise ExtractionError("Only jw.org URLs are allowed")


async def fetch_html(url: str) -> str:
    timeout = httpx.Timeout(10.0, connect=5.0)
    headers = {
        "User-Agent": USER_AGENT,
        "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "Accept-Language": "en-US,en;q=0.9",
    }
    verify = certifi.where()
    if os.getenv("JW_NEWS_READER_INSECURE_SSL") == "1":
        verify = False
    try:
        async with httpx.AsyncClient(
            timeout=timeout,
            follow_redirects=True,
            headers=headers,
            verify=verify,
        ) as client:
            response = await client.get(url)
            response.raise_for_status()
    except httpx.HTTPStatusError as exc:
        raise httpx.HTTPStatusError(
            "Upstream returned an error status",
            request=exc.request,
            response=exc.response,
        )
    except httpx.RequestError as exc:
        LOGGER.exception("Upstream request failed: %s", exc)
        raise httpx.RequestError("Upstream request failed", request=exc.request) from exc

    content_type = response.headers.get("content-type", "").lower()
    if "text/html" not in content_type:
        raise ExtractionError("URL did not return HTML")
    return response.text


def _remove_unwanted_tags(soup: BeautifulSoup) -> None:
    for tag in soup.find_all([
        "script",
        "style",
        "noscript",
        "nav",
        "footer",
        "aside",
        "svg",
        "form",
        "button",
    ]):
        tag.decompose()

    for tag in soup.find_all("header"):
        if tag.find_parent("article") or tag.find_parent("main"):
            continue
        tag.decompose()


def _attr_contains(tag: Tag, attr: str, needles: tuple[str, ...]) -> bool:
    value = tag.get(attr)
    if not value:
        return False
    value = str(value).casefold()
    return any(needle in value for needle in needles)


def _text_is_play(tag: Tag) -> bool:
    text = tag.get_text(" ", strip=True)
    return text.casefold() == "play"


def _contains_node(parent: Tag, node: Optional[Tag]) -> bool:
    if node is None:
        return False
    if parent is node:
        return True
    for descendant in parent.descendants:
        if descendant is node:
            return True
    return False


def _find_title_node(container: Tag, title: Optional[str]) -> Optional[Tag]:
    if not title:
        return None
    for tag in container.find_all(["h1", "h2", "h3", "h4", "h5", "h6", "p", "div", "span"]):
        text = tag.get_text(" ", strip=True)
        if text == title:
            return tag
    return None


def _strip_player_controls(container: Tag) -> None:
    for tag in list(container.find_all(["audio", "video", "source", "track"])):
        tag.decompose()

    control_needles = ("play", "audio", "video")
    for tag in list(container.find_all(attrs={"aria-label": True})):
        if _attr_contains(tag, "aria-label", control_needles):
            tag.decompose()
    for tag in list(container.find_all(attrs={"title": True})):
        if _attr_contains(tag, "title", control_needles):
            tag.decompose()

    for tag in list(container.find_all(attrs={"role": True})):
        role = str(tag.get("role", "")).casefold()
        if role in {"button", "link"} and _text_is_play(tag):
            tag.decompose()

    for tag in list(container.find_all(True)):
        if not isinstance(tag, Tag) or tag.attrs is None:
            continue
        class_text = " ".join(tag.get("class", []))
        id_text = tag.get("id", "")
        if not (PLAYER_CLASS_RE.search(class_text) or PLAYER_CLASS_RE.search(id_text)):
            continue
        if tag.find("img") is not None or tag.find("picture") is not None:
            continue
        text_len = len(tag.get_text(" ", strip=True))
        if text_len <= 20:
            tag.decompose()


def _strip_metadata_blocks(container: Tag, title: Optional[str], title_node: Optional[Tag]) -> None:
    candidates = list(container.find_all([
        "section",
        "div",
        "p",
        "ul",
        "ol",
        "li",
        "footer",
        "aside",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
    ]))
    for tag in candidates:
        if tag is container:
            continue
        if tag.parent is None:
            continue
        if _contains_node(tag, title_node):
            continue
        text = tag.get_text(" ", strip=True)
        if not text:
            continue
        normalized = " ".join(text.split())
        lowered = normalized.casefold()
        is_short = len(normalized) <= 250

        class_id = " ".join(filter(None, [tag.get("id", ""), " ".join(tag.get("class", []))]))
        if class_id and METADATA_CLASS_RE.search(class_id) and is_short:
            tag.decompose()
            continue

        upper = normalized.upper()
        if ("THE WATCHTOWER" in upper or "AWAKE!" in upper) and is_short:
            tag.decompose()
            continue

        if ISSUE_RE.search(normalized) and ("No." in normalized or "pp." in normalized or "pp " in normalized):
            if is_short:
                tag.decompose()
                continue

        if title and "english" in lowered and title.casefold() in lowered and is_short:
            tag.decompose()


def _ensure_markdown_title(markdown: str, title: Optional[str]) -> str:
    if not title:
        return markdown
    lines = markdown.splitlines()
    for idx, line in enumerate(lines):
        if line.strip():
            stripped = line.strip()
            if stripped == f"# {title}":
                return markdown
            if stripped == title:
                lines[idx] = f"# {title}"
                return "\n".join(lines)
            return markdown
    return markdown


def _select_container(soup: BeautifulSoup) -> Optional[Tag]:
    article = soup.find("article")
    if article:
        return article
    main = soup.find("main")
    if main:
        return main

    best: Optional[Tag] = None
    best_len = 0
    for div in soup.find_all("div"):
        attrs = " ".join(filter(None, [div.get("id", ""), " ".join(div.get("class", []))]))
        if not KEYWORD_RE.search(attrs):
            continue
        text_len = len(div.get_text(" ", strip=True))
        if text_len > best_len:
            best = div
            best_len = text_len

    if best and best_len >= MIN_TEXT_LEN:
        return best
    return None


def _readability_fallback(html: str) -> Tuple[Tag, Optional[str]]:
    document = Document(html)
    summary_html = document.summary(html_partial=True)
    summary_soup = BeautifulSoup(summary_html, "lxml")
    container = summary_soup.body or summary_soup
    return container, document.title()


def _best_src_from_srcset(srcset: str) -> Optional[str]:
    candidates = []
    for index, part in enumerate(srcset.split(",")):
        part = part.strip()
        if not part:
            continue
        pieces = part.split()
        url = pieces[0]
        score = 0.0
        if len(pieces) > 1:
            descriptor = pieces[1]
            if descriptor.endswith("w") or descriptor.endswith("x"):
                try:
                    score = float(descriptor[:-1])
                except ValueError:
                    score = 0.0
        candidates.append((score, index, url))
    if not candidates:
        return None
    candidates.sort(key=lambda item: (item[0], item[1]))
    return candidates[-1][2]


def _score_image_url(url: str) -> int:
    match = re.search(r"_(xs|s|m|l|xl)(?:\b|\.|_)", url, re.IGNORECASE)
    if not match:
        return 0
    size = match.group(1).lower()
    return {"xs": 1, "s": 2, "m": 3, "l": 4, "xl": 5}.get(size, 0)


def _pick_best_image_url(urls: list[str]) -> Optional[str]:
    if not urls:
        return None
    scored = [(idx, _score_image_url(url), url) for idx, url in enumerate(urls)]
    scored.sort(key=lambda item: (item[1], -item[0]))
    return scored[-1][2]


def _extract_meta_image(soup: BeautifulSoup) -> Optional[str]:
    keys = [
        ("property", "og:image"),
        ("property", "og:image:secure_url"),
        ("name", "twitter:image"),
        ("name", "twitter:image:src"),
        ("itemprop", "image"),
    ]
    for attr, key in keys:
        tag = soup.find("meta", attrs={attr: key})
        if tag and tag.get("content"):
            return tag["content"].strip()
    return None


def _extract_jsonld_image(soup: BeautifulSoup) -> Optional[str]:
    for script in soup.find_all("script", attrs={"type": "application/ld+json"}):
        if not script.string:
            continue
        try:
            payload = json.loads(script.string)
        except json.JSONDecodeError:
            continue
        url = _extract_jsonld_image_value(payload)
        if url:
            return url
    return None


def _extract_jsonld_image_value(payload: object) -> Optional[str]:
    if isinstance(payload, dict):
        for key in ("image", "thumbnailUrl"):
            if key in payload:
                value = payload[key]
                if isinstance(value, str):
                    return value
                if isinstance(value, list):
                    for item in value:
                        if isinstance(item, str):
                            return item
                        if isinstance(item, dict) and "url" in item:
                            url = item.get("url")
                            if isinstance(url, str):
                                return url
                if isinstance(value, dict) and "url" in value:
                    url = value.get("url")
                    if isinstance(url, str):
                        return url
        for nested in payload.values():
            url = _extract_jsonld_image_value(nested)
            if url:
                return url
    elif isinstance(payload, list):
        for item in payload:
            url = _extract_jsonld_image_value(item)
            if url:
                return url
    return None


def _extract_image_link(soup: BeautifulSoup) -> tuple[Optional[str], Optional[str]]:
    for anchor in soup.find_all("a"):
        text = anchor.get_text(" ", strip=True)
        if not text:
            continue
        if text.startswith("Image:"):
            href = anchor.get("href")
            if href:
                alt = text.split("Image:", 1)[1].strip() or None
                return href, alt
    return None, None


def _extract_fallback_image(
    html: str,
    soup: BeautifulSoup,
    base_url: str,
) -> Optional[dict]:
    meta_image = _extract_meta_image(soup)
    if meta_image:
        return {"url": urljoin(base_url, meta_image), "alt": None, "caption": None}

    jsonld_image = _extract_jsonld_image(soup)
    if jsonld_image:
        return {"url": urljoin(base_url, jsonld_image), "alt": None, "caption": None}

    link_url, link_alt = _extract_image_link(soup)
    if link_url:
        return {"url": urljoin(base_url, link_url), "alt": link_alt, "caption": None}

    cms_urls = CMS_IMAGE_RE.findall(html)
    if cms_urls:
        best = _pick_best_image_url(cms_urls)
        if best:
            return {"url": best, "alt": None, "caption": None}

    akamai_urls = AKAMAI_IMAGE_RE.findall(html)
    if akamai_urls:
        best = _pick_best_image_url(akamai_urls)
        if best:
            return {"url": best, "alt": None, "caption": None}

    return None


def _insert_fallback_image(markdown: str, image: dict) -> str:
    alt = image.get("alt") or ""
    url = image["url"]
    image_md = f"![{alt}]({url})"
    if not markdown.strip():
        return image_md

    lines = markdown.splitlines()
    for idx, line in enumerate(lines):
        if line.strip():
            if line.startswith("# "):
                head = "\n".join(lines[: idx + 1])
                tail = "\n".join(lines[idx + 1 :]).strip()
                if tail:
                    return f"{head}\n\n{image_md}\n\n{tail}"
                return f"{head}\n\n{image_md}"
            return f"{image_md}\n\n{markdown}"
    return f"{image_md}\n\n{markdown}"


def _normalize_img_tag(img: Tag, base_url: str) -> Optional[str]:
    src = img.get("data-src") or img.get("src")
    if not src:
        for attr in [
            "data-original",
            "data-largest",
            "data-large",
            "data-medium",
            "data-small",
            "data-smallest",
        ]:
            src = img.get(attr)
            if src:
                break
    if not src:
        srcset = img.get("srcset") or img.get("data-srcset")
        if srcset:
            src = _best_src_from_srcset(srcset)
    if not src:
        img.decompose()
        return None

    abs_src = urljoin(base_url, src)
    img["src"] = abs_src
    for attr in [
        "data-src",
        "data-srcset",
        "data-original",
        "data-largest",
        "data-large",
        "data-medium",
        "data-small",
        "data-smallest",
        "srcset",
    ]:
        if attr in img.attrs:
            del img.attrs[attr]
    return abs_src


def _get_soup(node: Tag) -> BeautifulSoup:
    if isinstance(node, BeautifulSoup):
        return node
    return node.soup


def _normalize_figures(container: Tag, base_url: str) -> None:
    figures = list(container.find_all("figure"))
    for figure in figures:
        img = figure.find("img")
        if not img:
            figure.decompose()
            continue
        if _normalize_img_tag(img, base_url) is None:
            figure.decompose()
            continue
        caption_text = None
        figcaption = figure.find("figcaption")
        if figcaption:
            caption_text = figcaption.get_text(" ", strip=True) or None

        figure.insert_after(img)
        if caption_text:
            soup = _get_soup(figure)
            p_tag = soup.new_tag("p")
            em_tag = soup.new_tag("em")
            em_tag.string = caption_text
            p_tag.append(em_tag)
            img.insert_after(p_tag)
        figure.decompose()


def _normalize_links(container: Tag, base_url: str) -> None:
    for anchor in container.find_all("a"):
        href = anchor.get("href")
        if not href:
            continue
        anchor["href"] = urljoin(base_url, href)


def _normalize_images(container: Tag, base_url: str) -> None:
    for img in container.find_all("img"):
        _normalize_img_tag(img, base_url)


def _extract_title(container: Tag, soup: BeautifulSoup, fallback: Optional[str]) -> Optional[str]:
    h1 = container.find("h1")
    if h1:
        text = h1.get_text(" ", strip=True)
        if text:
            return text
    if soup.title and soup.title.string:
        text = soup.title.string.strip()
        if text:
            return text
    if fallback:
        text = fallback.strip()
        if text:
            return text
    return None


def _collect_images(container: Tag) -> list[dict]:
    images = []
    for img in container.find_all("img"):
        src = img.get("src")
        if not src:
            continue
        alt = img.get("alt")
        if alt is not None:
            alt = alt.strip() or None
        caption = None
        next_sibling = img.find_next_sibling()
        if next_sibling and next_sibling.name == "p":
            em = next_sibling.find("em")
            if em:
                em_text = em.get_text(" ", strip=True)
                p_text = next_sibling.get_text(" ", strip=True)
                if em_text and em_text == p_text:
                    caption = em_text
        images.append({"url": src, "alt": alt, "caption": caption})
    return images


def _html_to_markdown(container: Tag) -> str:
    from markdownify import markdownify

    markdown = markdownify(str(container), heading_style="ATX", bullets="-")
    markdown = re.sub(r"\n{3,}", "\n\n", markdown)
    return markdown.strip()


def extract_from_html(html: str, base_url: str) -> dict:
    soup = BeautifulSoup(html, "lxml")
    fallback_image = _extract_fallback_image(html, soup, base_url)
    _remove_unwanted_tags(soup)

    container = _select_container(soup)
    fallback_title = None
    if container is None:
        container, fallback_title = _readability_fallback(html)

    title = _extract_title(container, soup, fallback_title)
    title_node = _find_title_node(container, title)

    _strip_player_controls(container)
    _strip_metadata_blocks(container, title, title_node)

    _normalize_links(container, base_url)
    _normalize_figures(container, base_url)
    _normalize_images(container, base_url)

    images = _collect_images(container)
    markdown = _html_to_markdown(container)
    markdown = _ensure_markdown_title(markdown, title)
    if not images and fallback_image:
        if not fallback_image.get("alt"):
            fallback_image["alt"] = title
        images = [fallback_image]
        markdown = _insert_fallback_image(markdown, fallback_image)

    return {
        "markdown": markdown,
        "title": title,
        "source_url": base_url,
        "images": images,
    }


async def extract_article(url: str) -> dict:
    validate_url(url)
    html = await fetch_html(url)
    return extract_from_html(html, url)
