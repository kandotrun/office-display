"""Hearing the wake phrase in rough Vosk captions.

vosk-model-small-ja hears エンデルヒール as 「エンゼル ヒール」 and worse, so
matching is fuzzy and on kana: captions are normalised (NFKC, katakana to
hiragana, no spacing or punctuation) and compared with aliases by the best
substring similarity, 1 - edit distance / alias length.

* Strong aliases wake on their own at a similarity of 0.8 or more.
* Weak aliases are observed mis-hearings that are also ordinary words
  (エネルギー, ベルギービール, ...); they wake only when they match exactly
  and a cue such as なら or さん follows within 4 characters. (One edit away
  from the short ones is everyday talk: 「リーダー で いる の は」 and
  「メンバー で いる なら」 are 0.8 from えーでいる. Every measured weak caption
  matches exactly.)

The question is what follows the name, in the caption's own script, with a
leading cue removed; fewer than 3 characters left means "wake, then listen".
"""
import collections
import re
import unicodedata

STRONG = ("えんでるひーる", "えんぜるひーる", "えんぜるびーる", "えあれんでぃる", "えあれんでる")
WEAK = (
    "えねるぎーいる",
    "えーでいる",
    "えーられんでいる",
    "やれでいる",
    "えーあれんじいる",
    "でれるびーる",
    "べるぎーびーる",
)
CUES = ("なら", "だったら", "たら", "さん", "てきに", "的に", "って", "は")
# removed from the start of the question; 、 never gets here (normalise drops it)
LEADING_CUES = ("だったら", "てきには", "的には", "なら", "さん")
THRESHOLD = 0.8
WEAK_THRESHOLD = 1.0  # exact matches only
CUE_WINDOW = 4
MIN_QUESTION = 3
MAX_QUESTION = 200

Wake = collections.namedtuple("Wake", "question alias score strong")

_SPACES = re.compile(r"\s+")


def _fold(ch):
    """One NFKC character as matched: katakana as hiragana, Latin case-folded."""
    code = ord(ch)
    if 0x30A1 <= code <= 0x30F6 or code in (0x30FD, 0x30FE):  # ァ..ヶ, ヽ ヾ
        return chr(code - 0x60)
    return ch.casefold()


def _dropped(ch):
    # separators, punctuation, symbols and control characters; ー is a letter (Lm)
    return unicodedata.category(ch)[0] in "ZPSC"


def _normalised(text):
    """(NFKC text, normalised text, index in the NFKC text of each normalised char)."""
    base = unicodedata.normalize("NFKC", text or "")
    out, src = [], []
    for i, ch in enumerate(base):
        if _dropped(ch):
            continue
        for folded in _fold(ch):
            out.append(folded)
            src.append(i)
    return base, "".join(out), src


def normalise(text):
    return _normalised(text)[1]


def tidy(text):
    """A transcript for display: NFKC, and no spaces between Japanese words.

    Vosk separates every word with a space; spaces next to a wide character
    are dropped, spaces between Latin words are kept.
    """
    text = _SPACES.sub(" ", unicodedata.normalize("NFKC", text or "")).strip()
    out = []
    for i, ch in enumerate(text):
        if ch == " " and (_wide(text[i - 1]) or _wide(text[i + 1])):
            continue
        out.append(ch)
    return "".join(out)


def _wide(ch):
    return unicodedata.east_asian_width(ch) in ("W", "F")


def _distances(alias, norm):
    """Edit distance of alias to the best substring of norm ending at each index."""
    prev = list(range(len(alias) + 1))  # an empty substring: delete the alias
    ends = [prev[-1]]
    for c in norm:
        cur = [0]  # a match may start anywhere for free
        for i, a in enumerate(alias, 1):
            cur.append(min(prev[i] + 1, cur[i - 1] + 1, prev[i - 1] + (a != c)))
        ends.append(cur[-1])
        prev = cur
    return ends


def _has_cue(norm, end):
    window = norm[end : end + CUE_WINDOW]
    return any(cue in window for cue in CUES)


def similarity(alias, norm, need_cue=False):
    """(1 - edit distance / len(alias), end index) of the best match in norm.

    Of equally good ends, the first one followed by a cue wins, else the first:
    「えんでるひーなら」 ends before な, not after it. With need_cue only ends
    followed by a cue count; (0.0, 0) when there is none.
    """
    ends = _distances(alias, norm)
    candidates = [j for j in range(len(ends)) if not need_cue or _has_cue(norm, j)]
    if not candidates:
        return 0.0, 0
    best = min(ends[j] for j in candidates)
    ties = [j for j in candidates if ends[j] == best]
    end = next((j for j in ties if _has_cue(norm, j)), ties[0])
    return 1.0 - best / len(alias), end


def _best(aliases, norm, need_cue, threshold):
    hits = []
    for alias in aliases:
        score, end = similarity(alias, norm, need_cue)
        if score >= threshold:
            hits.append((score, len(alias), -end, alias, end))
    return max(hits) if hits else None


def detect(text):
    """A Wake for a caption that holds the wake phrase, else None."""
    base, norm, src = _normalised(text)
    if not norm:
        return None
    strong = True
    hit = _best(STRONG, norm, need_cue=False, threshold=THRESHOLD)
    if hit is None:
        strong = False
        hit = _best(WEAK, norm, need_cue=True, threshold=WEAK_THRESHOLD)
    if hit is None:
        return None
    score, _, _, alias, end = hit
    for cue in LEADING_CUES:
        if norm.startswith(cue, end):
            end += len(cue)
            break
    question = ""
    if len(norm) - end >= MIN_QUESTION:
        question = tidy(base[src[end] :])[:MAX_QUESTION]
    return Wake(question, alias, score, strong)
