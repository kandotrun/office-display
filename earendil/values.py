"""The values: our bundled summaries (values.json) and the original page text.

values.json holds Earendil's English titles plus the team's own Japanese
summaries; /values.json serves exactly those. The original page text is only
reference material for the model: fetched at runtime, cached on disk (as
extracted plain text) for a day, and never part of the repository.
"""
import html.parser
import json
import re

MAX_VALUES = 32
MAX_TITLE = 64
MAX_JA = 40
MAX_GLOSS = 120
KINDS = ("value", "principle")
HEADINGS = {"value": "Values:", "principle": "Operating Principles:"}

SOURCE_KEY = "values-page.txt"
SOURCE_MAX_AGE = 86400
SOURCE_MAX_BYTES = 2 << 20
SOURCE_MAX_CHARS = 16000


def _text(entry, key, limit, empty=False):
    value = entry.get(key)
    if not isinstance(value, str) or len(value) > limit or (not value and not empty):
        raise ValueError("values: %s must be a string of 1..%d characters" % (key, limit))
    return value


def validate(doc):
    """The /values.json document from a parsed values.json (ValueError when invalid)."""
    if not isinstance(doc, dict) or not isinstance(doc.get("source"), str):
        raise ValueError("values: expected an object with a source URL")
    entries = doc.get("values")
    if not isinstance(entries, list) or not 1 <= len(entries) <= MAX_VALUES:
        raise ValueError("values: expected 1..%d values" % MAX_VALUES)
    out, seen = [], set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("values: every value must be an object")
        clean = {
            "title": _text(entry, "title", MAX_TITLE),
            "kind": entry.get("kind"),
            "ja": _text(entry, "ja", MAX_JA),
            "gloss": _text(entry, "gloss", MAX_GLOSS),
        }
        if clean["kind"] not in KINDS:
            raise ValueError("values: kind must be one of %s" % ", ".join(KINDS))
        if clean["title"].casefold() in seen:
            # the model cites values by title
            raise ValueError("values: duplicate title %r" % clean["title"])
        seen.add(clean["title"].casefold())
        out.append(clean)
    return {"source": doc["source"], "values": out}


def load(path):
    with open(path, encoding="utf-8") as f:
        return validate(json.load(f))


def render_block(entries):
    """The values for the system prompt: one line each, grouped by kind."""
    lines = []
    for kind in KINDS:
        group = [e for e in entries if e["kind"] == kind]
        if not group:
            continue
        if lines:
            lines.append("")
        lines.append(HEADINGS[kind])
        lines.extend("- %s（%s）: %s" % (e["title"], e["ja"], e["gloss"]) for e in group)
    return "\n".join(lines)


# -- the original page -------------------------------------------------------

SKIP = {"script", "style", "noscript", "template", "svg", "head", "nav", "header", "footer", "form", "button", "iframe"}
BLOCKS = {
    "address", "article", "aside", "blockquote", "br", "dd", "details", "div", "dl", "dt", "figcaption",
    "figure", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "li", "main", "ol", "p", "section", "summary",
    "table", "td", "th", "tr", "ul",
}  # fmt: skip
_SPACE = re.compile(r"\s+")


class _TextParser(html.parser.HTMLParser):
    """Text lines of a page, separately for everything and for <main>."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.skip = 0
        self.main = 0
        self.lines = {"all": [""], "main": [""]}

    def _break(self, prefix=""):
        for name, lines in self.lines.items():
            if name == "all" or self.main:
                lines.append(prefix)

    def handle_starttag(self, tag, attrs):
        if tag in SKIP:
            self.skip += 1
        elif tag == "main":
            self.main += 1
        if tag in BLOCKS and not self.skip:
            self._break("- " if tag == "li" else "")

    def handle_startendtag(self, tag, attrs):
        if tag in BLOCKS and not self.skip:
            self._break()

    def handle_endtag(self, tag):
        if tag in SKIP:
            self.skip = max(0, self.skip - 1)
        elif tag == "main":
            self.main = max(0, self.main - 1)
        if tag in BLOCKS and not self.skip:
            self._break()

    def handle_data(self, data):
        if self.skip:
            return
        for name, lines in self.lines.items():
            if name == "all" or self.main:
                lines[-1] += data

    def text(self, name):
        out = []
        for line in self.lines[name]:
            line = _SPACE.sub(" ", line).strip()
            if line and line != "-":
                out.append(line)
        return out


def html_to_text(page, max_chars=SOURCE_MAX_CHARS):
    """Plain text of the main content (<main>, else the body), cut at a line boundary."""
    parser = _TextParser()
    parser.feed(page)
    parser.close()
    lines = parser.text("main") or parser.text("all")
    out, size = [], 0
    for line in lines:
        size += len(line) + (1 if out else 0)
        if size > max_chars:
            break
        out.append(line)
    return "\n".join(out)


class SourceText:
    """The original values page as plain text: refreshed daily, cached, or ""."""

    def __init__(self, url, disk, fetch):
        self.url = url
        self.cache = disk
        self.fetch = fetch

    def _download(self):
        page = self.fetch(self.url, max_bytes=SOURCE_MAX_BYTES)
        return html_to_text(page.decode("utf-8", "replace")).encode("utf-8")

    @staticmethod
    def _validate(data):
        if not data.strip():
            raise ValueError("values page has no text")

    def refresh(self):
        try:
            data = self.cache.fetch(SOURCE_KEY, SOURCE_MAX_AGE, self._download, self._validate)
        except Exception:
            return ""
        return data.decode("utf-8", "replace")
