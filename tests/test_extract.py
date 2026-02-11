import re

import pytest

from app.extract import ExtractionError, extract_from_html, validate_url


def test_validate_url_accepts_jw_domains():
    validate_url("https://www.jw.org/en/")
    validate_url("https://wol.jw.org/en/")


def test_validate_url_rejects_non_https_or_non_jw():
    with pytest.raises(ExtractionError):
        validate_url("http://www.jw.org/en/")
    with pytest.raises(ExtractionError):
        validate_url("https://example.com/")


def test_extract_preserves_order_and_captions():
    html = """
    <html>
      <head><title>Sample Title</title></head>
      <body>
        <main>
          <h1>Headline</h1>
          <p>First paragraph.</p>
          <figure>
            <img src="/images/cover.jpg" alt="Cover">
            <figcaption>Cover caption</figcaption>
          </figure>
          <p>Second paragraph.</p>
          <img data-src="/images/inline.jpg" alt="Inline">
        </main>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    markdown = result["markdown"]

    assert "First paragraph." in markdown
    assert "Second paragraph." in markdown
    assert "![Cover](https://www.jw.org/images/cover.jpg)" in markdown
    assert "![Inline](https://www.jw.org/images/inline.jpg)" in markdown
    assert re.search(r"^[*_]Cover caption[*_]$", markdown, re.MULTILINE)

    first_idx = markdown.index("First paragraph.")
    img_idx = markdown.index("![Cover](https://www.jw.org/images/cover.jpg)")
    caption_idx = markdown.index("Cover caption")
    second_idx = markdown.index("Second paragraph.")
    inline_idx = markdown.index("![Inline](https://www.jw.org/images/inline.jpg)")

    assert first_idx < img_idx < caption_idx < second_idx < inline_idx

    assert result["images"] == [
        {"url": "https://www.jw.org/images/cover.jpg", "alt": "Cover", "caption": "Cover caption"},
        {"url": "https://www.jw.org/images/inline.jpg", "alt": "Inline", "caption": None},
    ]


def test_article_header_image_kept():
    html = """
    <html>
      <body>
        <article>
          <header>
            <figure>
              <img src="/images/hero.jpg" alt="Hero">
            </figure>
          </header>
          <p>Body content.</p>
        </article>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    markdown = result["markdown"]

    assert "Body content." in markdown
    assert "![Hero](https://www.jw.org/images/hero.jpg)" in markdown
    assert result["images"] == [
        {"url": "https://www.jw.org/images/hero.jpg", "alt": "Hero", "caption": None},
    ]


def test_data_largest_image_resolution():
    html = """
    <html>
      <body>
        <article>
          <img data-largest="/images/hero.jpg" alt="Hero">
        </article>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    assert "![Hero](https://www.jw.org/images/hero.jpg)" in result["markdown"]
    assert result["images"] == [
        {"url": "https://www.jw.org/images/hero.jpg", "alt": "Hero", "caption": None},
    ]


def test_fallback_image_from_cms_url():
    html = """
    <html>
      <head><title>Sample Title</title></head>
      <body>
        <main>
          <p>Body content.</p>
        </main>
        <div class="context">
          https://cms-imgp.jw-cdn.org/img/p/504000002/univ/art/504000002_univ_sqr_xl.jpg
        </div>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    assert "![Sample Title](https://cms-imgp.jw-cdn.org/img/p/504000002/univ/art/504000002_univ_sqr_xl.jpg)" in result["markdown"]
    assert result["images"] == [
        {
            "url": "https://cms-imgp.jw-cdn.org/img/p/504000002/univ/art/504000002_univ_sqr_xl.jpg",
            "alt": "Sample Title",
            "caption": None,
        },
    ]


def test_fallback_image_from_image_link():
    html = """
    <html>
      <body>
        <main>
          <p>Body content.</p>
        </main>
        <div class="context">
          <a href="https://cms-imgp.jw-cdn.org/img/p/123/univ/art/123_univ_sqr_xl.jpg">
            Image: Hero image alt text
          </a>
        </div>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    assert "![Hero image alt text](https://cms-imgp.jw-cdn.org/img/p/123/univ/art/123_univ_sqr_xl.jpg)" in result["markdown"]
    assert result["images"] == [
        {
            "url": "https://cms-imgp.jw-cdn.org/img/p/123/univ/art/123_univ_sqr_xl.jpg",
            "alt": "Hero image alt text",
            "caption": None,
        },
    ]


def test_relative_image_resolution():
    html = """
    <html><body><main><img src="/media/pic.jpg" alt="Pic"></main></body></html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    assert "https://www.jw.org/media/pic.jpg" in result["markdown"]


def test_readability_fallback():
    html = """
    <html>
      <body>
        <div>Short content that should still be captured.</div>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    assert "Short content that should still be captured." in result["markdown"]


def test_strip_play_control():
    html = """
    <html>
      <body>
        <main>
          <div class="audio-player"><button role="button">PLAY</button></div>
          <p>Body content.</p>
        </main>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    markdown = result["markdown"]
    assert "Body content." in markdown
    assert "PLAY" not in markdown


def test_title_h1_conversion():
    html = """
    <html>
      <head><title>Right and Wrong: A Choice You Must Make</title></head>
      <body>
        <main>
          <p>Right and Wrong: A Choice You Must Make</p>
          <p>Body text.</p>
        </main>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    markdown = result["markdown"].splitlines()
    first_non_empty = next(line for line in markdown if line.strip())
    assert first_non_empty == "# Right and Wrong: A Choice You Must Make"


def test_metadata_block_removed():
    html = """
    <html>
      <body>
        <main>
          <p>Body text.</p>
          <div class="publication-info">
            THE WATCHTOWER
            <br>
            wp24 No. 1 pp. 14-15
          </div>
        </main>
      </body>
    </html>
    """
    result = extract_from_html(html, "https://www.jw.org/en/")
    markdown = result["markdown"]
    assert "Body text." in markdown
    assert "THE WATCHTOWER" not in markdown
    assert "wp24" not in markdown
