import copy
import json
import os

import pytest

import cache
import net
import values

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VALUES_JSON = os.path.join(HERE, "values.json")
DAY = 86400


def doc(**over):
    entry = {"title": "We not I", "kind": "value", "ja": "「私」ではなく「私たち」", "gloss": "チームで。"}
    entry.update(over)
    return {"source": "https://earendil.com/values/", "values": [entry]}


def test_the_bundled_values_load_in_page_order():
    loaded = values.load(VALUES_JSON)
    assert loaded["source"] == "https://earendil.com/values/"
    titles = [v["title"] for v in loaded["values"]]
    assert len(titles) == 14
    assert titles[0] == "Be Compassionate" and titles[-1] == "Radical Responsibility, Generous Recognition"
    kinds = [v["kind"] for v in loaded["values"]]
    assert kinds == ["value"] * 7 + ["principle"] * 7


def test_the_document_has_only_the_contract_fields():
    loaded = values.load(VALUES_JSON)
    assert set(loaded) == {"source", "values"}  # the file's "note" is not served
    for entry in loaded["values"]:
        assert set(entry) == {"title", "kind", "ja", "gloss"}
        assert len(entry["title"]) <= 64 and len(entry["ja"]) <= 40 and len(entry["gloss"]) <= 120


def test_the_values_file_is_plain_json():
    with open(VALUES_JSON, encoding="utf-8") as f:
        assert json.load(f)["values"]


@pytest.mark.parametrize(
    "bad",
    [
        doc(title="x" * 65),
        doc(title=""),
        doc(ja="あ" * 41),
        doc(gloss="あ" * 121),
        doc(kind="rule"),
        doc(title=3),
        {"source": "https://earendil.com/values/", "values": []},
        {"source": "https://earendil.com/values/", "values": [doc()["values"][0]] * 2},  # duplicate title
        {"source": "https://earendil.com/values/", "values": "We not I"},
        {"values": doc()["values"]},
        [],
    ],
)
def test_invalid_documents_are_rejected(bad):
    with pytest.raises(ValueError):
        values.validate(bad)


def test_at_most_32_entries():
    many = {"source": "s", "values": [doc(title="v%d" % i)["values"][0] for i in range(33)]}
    with pytest.raises(ValueError):
        values.validate(many)
    many["values"].pop()
    assert len(values.validate(many)["values"]) == 32


def test_validate_does_not_touch_its_input():
    original = doc(extra="ignored")
    kept = copy.deepcopy(original)
    clean = values.validate(original)
    assert original == kept and "extra" not in clean["values"][0]


def test_render_block_lists_values_then_principles():
    block = values.render_block(values.load(VALUES_JSON)["values"])
    lines = block.splitlines()
    assert lines[0] == "Values:"
    assert "- Be Compassionate（思いやりを持つ）: 人は仕事の前に人。誰にでも調子の悪い日がある。まず寛大さを、そして相手の善意を信じよう。" in lines
    assert "Operating Principles:" in lines
    assert lines.index("Operating Principles:") > lines.index("- Positive Intentions & Outcomes（良い意図と、良い結果）: " + values.load(VALUES_JSON)["values"][6]["gloss"])
    assert "{" not in block and "}" not in block


PAGE = """<!doctype html><html><head><title>T</title><style>p{color:red}</style>
<script>var secret = "not text";</script></head>
<body><nav><a href="/">Home</a><a href="/values/">Values</a></nav>
<main class="content-page">
  <article>
    <h1 class="visually-hidden">Principles</h1>
    <p class="section-heading"><em><strong>Our Things</strong></em></p>
    <details><summary><em><span>Be Tidy</span></em></summary>
      <ul><li>Put things back   where they
        belong</li><li>Leave the kitchen better &amp; cleaner</li></ul>
    </details>
    <details><summary>Ship It</summary><ul><li>Done beats perfect</li></ul></details>
    <svg><path d="M0 0"/><text>icon</text></svg>
  </article>
</main>
<footer>© somebody</footer></body></html>"""


def test_html_to_text_keeps_the_main_content_only():
    assert values.html_to_text(PAGE) == "\n".join(
        [
            "Principles",
            "Our Things",
            "Be Tidy",
            "- Put things back where they belong",
            "- Leave the kitchen better & cleaner",
            "Ship It",
            "- Done beats perfect",
        ]
    )


def test_html_to_text_without_main_uses_the_body():
    html = "<html><body><header>menu</header><p>One</p><p>Two<br>Three</p><script>x</script></body></html>"
    assert values.html_to_text(html) == "One\nTwo\nThree"


def test_html_to_text_is_capped_on_a_line_boundary():
    html = "<main>" + "".join("<p>line %03d</p>" % i for i in range(100)) + "</main>"
    text = values.html_to_text(html, max_chars=40)
    assert text == "line 000\nline 001\nline 002\nline 003"


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


class FakeFetch:
    def __init__(self, body=PAGE.encode()):
        self.body = body
        self.calls = []
        self.fail = False

    def __call__(self, url, **kw):
        self.calls.append((url, kw))
        if self.fail:
            raise net.FetchError(url, 503, "down")
        return self.body


def source(tmp_path, fetch, t=1_800_000_000.0):
    return values.SourceText("https://earendil.com/values/", cache.DiskCache(tmp_path, clock=Clock(t)), fetch)


def test_source_text_is_fetched_size_capped_and_cached(tmp_path):
    fetch = FakeFetch()
    text = source(tmp_path, fetch).refresh()
    assert text.startswith("Principles\nOur Things\nBe Tidy")
    url, kw = fetch.calls[0]
    assert url == "https://earendil.com/values/" and kw["max_bytes"] == values.SOURCE_MAX_BYTES
    # the cache holds the extracted text, not the page
    cached = (tmp_path / values.SOURCE_KEY).read_text(encoding="utf-8")
    assert cached == text and "<" not in cached


def test_source_text_is_refreshed_daily(tmp_path):
    fetch = FakeFetch()
    src = source(tmp_path, fetch)
    src.refresh()
    mtime = os.stat(tmp_path / values.SOURCE_KEY).st_mtime
    src.cache.clock = Clock(mtime + DAY - 60)
    src.refresh()
    assert len(fetch.calls) == 1
    src.cache.clock = Clock(mtime + DAY + 1)
    fetch.body = b"<main><p>New text</p></main>"
    assert src.refresh() == "New text"
    assert len(fetch.calls) == 2


def test_source_text_falls_back_to_the_cache_then_to_nothing(tmp_path):
    fetch = FakeFetch()
    src = source(tmp_path, fetch)
    first = src.refresh()
    src.cache.clock = Clock(4_000_000_000.0)
    fetch.fail = True
    assert src.refresh() == first
    empty = source(tmp_path / "empty", fetch)
    assert empty.refresh() == ""


def test_a_page_without_text_is_not_cached(tmp_path):
    fetch = FakeFetch(b"<html><body><script>only()</script></body></html>")
    src = source(tmp_path, fetch)
    assert src.refresh() == ""
    assert not (tmp_path / values.SOURCE_KEY).exists()
