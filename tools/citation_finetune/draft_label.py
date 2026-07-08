#!/usr/bin/env python3
"""Generate first-pass citation labels for the stratified sample.

This rewrite models the *structure* of a debate "fullcite" instead of blindly
comma-splitting.  A debate card citation is almost always:

    HEAD:  Surname[s] (+ "et al") + year-shorthand      e.g. `Friedman 11`
    BODY:  <delim> FullName, qualification; qualification, "TITLE",
           Publication, Vol/Issue/Pages, Date, URL/DOI, database ...
    TAIL:  trailing card signatures (`//BPS`, `TDI`, `]AR`) and `*notes`

The head is authoritative for author *surnames* and the shorthand year; the body
carries each author's *given* name and qualification prose.  We enumerate every
named author found in the body (per project decision) and keep each author's
qualifications as a single blob string (per project decision).

Output is schema-shaped.  Labeler bookkeeping (`confidence`, `inferred_fields`)
is kept under `_meta` and is *not* part of the seq2seq training target — see
`make_training_jsonl.py` / `to_target`.
"""

from __future__ import annotations

import argparse
import json
import re
from collections import Counter
from pathlib import Path


DEFAULT_INPUT = Path("datasets/citation_finetune/stratified_sample.jsonl")
DEFAULT_OUTPUT = Path("datasets/citation_finetune/labels_full_draft.jsonl")
DEFAULT_REVIEW = Path("datasets/citation_finetune/labels_full_review_queue.jsonl")
DEFAULT_SUMMARY = Path("datasets/citation_finetune/labels_full_summary.json")

WARNING_ENUM = {
    "incomplete_citation",
    "ambiguous_author",
    "ambiguous_author_qualification",
    "ambiguous_year",
    "conflicting_dates",
    "body_spillover",
    "trailing_debate_annotation",
    "url_only",
    "cross_reference_only",
    "no_date",
    "malformed",
    "possible_card_signature",
    "signature_note_fused",
    "unparsed_tail",
    "multiple_titles",
    "database_only",
    "page_range_ambiguous",
    "source_type_ambiguous",
    "not_a_citation",
    "et_al",
    "multiple_authors_unenumerated",
}

# Warnings that carry semantic signal a downstream consumer would want AND that
# are not already encoded structurally elsewhere in the object.  Only these ride
# along into the training target; the rest are labeler/review bookkeeping.
TARGET_WARNINGS = {
    "incomplete_citation",
    "url_only",
    "conflicting_dates",
    "source_type_ambiguous",
    "et_al",
    "no_date",
    "body_spillover",
}

SOURCE_TYPES = {
    "journal_article",
    "law_review",
    "news_article",
    "web_page",
    "book",
    "book_chapter",
    "report",
    "thesis",
    "legal_source",
    "dictionary_or_reference",
    "interview",
    "unknown",
}

MONTHS = (
    "January|February|March|April|May|June|July|August|September|October|"
    "November|December|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Sept|Oct|Nov|Dec"
)
MONTH_NAMES = {m.lower() for m in MONTHS.split("|")}

# Words that clearly mark the start of a qualification clause (so a comma-segment
# beginning with one of these is NOT a new author name).
QUAL_KEYWORDS = re.compile(
    r"\b(professor|prof\.?|lecturer|ph\.?\s*d|j\.?\s*d|m\.?\s*d|m\.?\s*a|b\.?\s*a|"
    r"b\.?\s*s|m\.?\s*s|m\.?\s*phil|fellow|director|researcher|research|writer|"
    r"reporter|journalist|editor|editor-in-chief|staff|candidate|attorney|lawyer|"
    r"chair|chairman|president|vice president|ceo|cfo|founder|co-founder|analyst|"
    r"columnist|correspondent|scholar|historian|economist|scientist|expert|"
    r"associate|assistant|adjunct|distinguished|senior|visiting|emeritus|"
    r"department|institute|university|college|school|foundation|center|centre|"
    r"faculty|dean|advisor|adviser|consultant|specialist|secretary|minister|"
    r"ambassador|officer|manager|coordinator|activist|professor of|teaches)\b",
    re.I,
)

# Institution / organization words — a segment containing one of these is an
# affiliation, not a person's name, even if it is Title-Cased.
INSTITUTION = re.compile(
    r"\b(University|College|Institute|Foundation|Cent(?:er|re)|Press|Journal|"
    r"Department|School|Society|Association|Committee|Council|Agency|Bureau|"
    r"Office|Ministry|Commission|Corporation|Company|Group|Fund|Magazine|"
    r"Review|Quarterly|Times|Post|Tribune|Gazette|Herald|Network|Program|"
    r"Project|Initiative|Coalition|Alliance|Union|League|Organi[sz]ation|"
    r"Chair|Department|Academy|Laboratory|Observatory|Hospital|Bank)\b",
    re.I,
)

# A qualification keyword that lets us keep scanning a proper-noun-looking run as
# an institution rather than a person (used to end an author-name span).
NAME_TOKEN = re.compile(r"^[A-Z][A-Za-z.'’-]*$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate draft citation labels.")
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--review", type=Path, default=DEFAULT_REVIEW)
    parser.add_argument("--summary", type=Path, default=DEFAULT_SUMMARY)
    return parser.parse_args()


def compact_spaces(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


# --------------------------------------------------------------------------- #
# Years / dates
# --------------------------------------------------------------------------- #
def expand_year(token: str | None) -> tuple[int | None, bool]:
    """Return (year, was_inferred_from_shorthand)."""
    if not token:
        return None, False
    raw = token.strip().strip("‘’'`,.()[]")
    if re.fullmatch(r"2[kK]", raw):
        return 2000, True
    if re.fullmatch(r"\d{4}", raw):
        n = int(raw)
        return (n, False) if 1000 <= n <= 2099 else (None, False)
    if re.fullmatch(r"\d{1,2}", raw):
        n = int(raw)
        if n <= 30:
            return 2000 + n, True
        return 1900 + n, True
    return None, False


# A short year that sits right after an author/institution name in the head.
_SHORT_YEAR_AFTER_NAME = re.compile(
    r"(?<![\d/-])[‘’']?\s*(\d{1,2})\b(?![\d/-])"
)


def find_year(text: str) -> tuple[int | None, bool]:
    """Locate the publication year.  Returns (year, was_inferred)."""
    lead = text[:160]
    m = re.search(r"[‘’']\s*(\d{2})\b(?![\d/-])", lead)
    if m:
        return expand_year(m.group(1))
    m = re.search(r"\bet\.?\s+al\.?\s*[‘’']?\s*(\d{1,2})\b(?![\d/-])", lead, re.I)
    if m:
        return expand_year(m.group(1))
    m = re.search(r"\b2[kK]\b", lead)
    if m:
        return 2000, True
    # Numeric date anywhere: M/D/YY(YY) -> take the year component.
    m = re.search(r"\b\d{1,2}[-/]\d{1,2}[-/](\d{2,4})\b", text)
    if m:
        return expand_year(m.group(1))
    # Explicit 4-digit year.
    m = re.search(r"\b(19\d{2}|20\d{2})\b", text)
    if m:
        return int(m.group(1)), False
    # Short year immediately after a capitalized name near the front.
    for m in re.finditer(
        r"\b([A-Z][A-Za-z.'’&-]{1,40})(?:\s+(?:and|&)\s+[A-Z][A-Za-z.'’&-]{1,40})?"
        r"(?:\s+et\.?\s*al\.?)?\s*,?\s*[‘’']?(\d{1,2})\b(?![\d/-])",
        lead,
    ):
        if m.group(1).lower() in MONTH_NAMES:
            continue
        return expand_year(m.group(2))
    return None, False


def year_from_date(value: str | None) -> int | None:
    if not value:
        return None
    m = re.search(r"\b(19\d{2}|20\d{2})\b", value)
    if m:
        return int(m.group(1))
    m = re.search(r"[-/](\d{2,4})\b$", value)
    if m:
        year, _ = expand_year(m.group(1))
        return year
    return None


NUMERIC_DATE = r"\b\d{1,2}[-/]\d{1,2}[-/]\d{2,4}\b"
MONTH_DATE = rf"\b(?:{MONTHS})\.?\s+\d{{1,2}}(?:st|nd|rd|th)?,?\s+\d{{4}}\b"
DAY_MONTH_DATE = rf"\b\d{{1,2}}\s+(?:{MONTHS})\.?,?\s+\d{{4}}\b"
DATE_PAT = rf"({NUMERIC_DATE}|{MONTH_DATE}|{DAY_MONTH_DATE})"


def extract_dates(
    text: str, title: str | None = None, url: str | None = None
) -> tuple[str | None, str | None, str | None]:
    """Return (published_date, accessed_date, retrieved_date)."""
    accessed = retrieved = published = None
    m = re.search(
        rf"(?:accessed(?:\s+online)?|date\s+accessed|last\s+accessed|accessed\s+on|"
        rf"accessed\s+date|d\.?o\.?a\.?|acc\.?)[:\s-]*{DATE_PAT}",
        text,
        re.I,
    )
    if m:
        accessed = compact_spaces(m.group(1))
    m = re.search(rf"(?:retrieved|web\.)[:\s-]*{DATE_PAT}", text, re.I)
    if m:
        retrieved = compact_spaces(m.group(1))
    url_index = text.find(url) if url and url in text else -1
    title_end = text.find(title) + len(title) if title and title in text else -1
    matches = list(re.finditer(DATE_PAT, text, re.I))
    if not accessed and url_index >= 0:
        for match in matches:
            if match.start() > url_index:
                accessed = compact_spaces(match.group(1))
                break
    for match in matches:
        value = compact_spaces(match.group(1))
        if value in {accessed, retrieved}:
            continue
        if url_index >= 0 and match.start() > url_index:
            continue
        if title_end >= 0 and match.start() < title_end:
            continue
        published = value
        break
    if published is None and not title and not url and matches:
        published = compact_spaces(matches[0].group(1))
    return published, accessed, retrieved


# --------------------------------------------------------------------------- #
# URL / DOI / pages / volume / issue / database
# --------------------------------------------------------------------------- #
def extract_url(text: str) -> str | None:
    m = re.search(r"https?://[^\s)\]>\"'”]+|www\.[^\s)\]>\"'”]+", text)
    if not m:
        return None
    return m.group(0).rstrip(".,;”’")


def extract_doi(text: str) -> str | None:
    m = re.search(r"\b10\.\d{4,9}/[^\s)\]>\"']+", text, re.I)
    if not m:
        return None
    return m.group(0).rstrip(".,;")


def extract_pages(text: str) -> tuple[str | None, bool]:
    patterns = [
        r"\bpp?\.?\s*([0-9]+(?:\s*[-–]\s*[0-9]+)?)\b",
        r"\bpages?\s+([0-9]+(?:\s*[-–]\s*[0-9]+)?)\b",
        r"\bpg\.?\s*([0-9]+(?:\s*[-–]\s*[0-9]+)?)\b",
        # journal-style trailing "112.4 (2013): 589-611" page range
        r"\(\d{4}\):\s*([0-9]+\s*[-–]\s*[0-9]+)\b",
    ]
    matches: list[str] = []
    for pat in patterns:
        matches.extend(re.findall(pat, text, re.I))
    cleaned = [compact_spaces(x.replace("–", "-")) for x in matches]
    if not cleaned:
        return None, False
    return cleaned[0], len(set(cleaned)) > 1


def extract_volume_issue(text: str) -> tuple[str | None, str | None]:
    volume = issue = None
    m = re.search(r"\b(?:Vol\.?|Volume)\s*([0-9]+)", text, re.I)
    if m:
        volume = m.group(1)
    m = re.search(r"\b(?:No\.?|Issue|Iss\.?)\s*([0-9]+)", text, re.I)
    if m:
        issue = m.group(1)
    # journal-style "94.1 (1984)" -> volume 94, issue 1
    if volume is None:
        m = re.search(r"\b(\d{1,3})\.(\d{1,3})\s*\(\d{4}\)", text)
        if m:
            volume, issue = m.group(1), m.group(2)
    return volume, issue


def extract_database(text: str, url: str | None) -> str | None:
    if re.search(r"\bLexis\s*Nexis\b|\bLexisNexis\b|\bLexis\b", text):
        return "LexisNexis"
    if "JSTOR" in text.upper() or (url and "jstor.org" in url):
        return "JSTOR"
    if re.search(r"\bProQuest\b", text, re.I):
        return "ProQuest"
    if re.search(r"\bEBSCO(host)?\b", text, re.I):
        return "EBSCO"
    if re.search(r"\bScienceDirect\b", text, re.I):
        return "ScienceDirect"
    return None


# --------------------------------------------------------------------------- #
# Title / publication
# --------------------------------------------------------------------------- #
def quoted_title(text: str) -> str | None:
    matches = re.findall(
        r"“([^”]{3,240})”|\"([^\"]{3,240})\"|‘([^’]{6,240})’",
        text,
    )
    if matches:
        for grp in matches:
            candidate = grp[0] or grp[1] or grp[2]
            if candidate:
                return compact_spaces(candidate)
    return None


def title_from_url(url: str | None) -> tuple[str | None, str | None, str | None]:
    if not url:
        return None, None, None
    cleaned = url.strip("[]()")
    host = re.sub(r"^https?://", "", cleaned).split("/")[0].lower()
    path = re.sub(r"^https?://[^/]+/?", "", cleaned)
    if "merriam-webster.com" in host and "/dictionary/" in cleaned:
        term = cleaned.rsplit("/dictionary/", 1)[1].split("?", 1)[0].strip("/")
        return term.replace("-", " ").title(), "Merriam-Webster", "dictionary_or_reference"
    if "law.cornell.edu" in host:
        title = path.rsplit("/", 1)[-1].replace("_", " ").replace("-", " ").title()
        return title or None, "Legal Information Institute", "legal_source"
    if "wikipedia.org" in host or "wikiwand.com" in host:
        title = path.rsplit("/", 1)[-1].replace("_", " ").replace("-", " ").title()
        return title or None, None, "dictionary_or_reference"
    if "lexico.com" in host and "/definition/" in cleaned:
        term = cleaned.rsplit("/definition/", 1)[1].split("?", 1)[0].strip("/")
        return term.replace("-", " ").title(), "Lexico", "dictionary_or_reference"
    return None, None, None


def allcaps_title(text: str) -> str | None:
    """A conservative unquoted-title grab: a long run of caps/Title-Case words."""
    m = re.search(r"\b([A-Z][A-Z0-9’'&:;,\s?-]{24,180}[A-Za-z?])\b", text)
    if m:
        chunk = compact_spaces(m.group(1))
        # Avoid grabbing an all-caps author-name-only run.
        if len(chunk.split()) >= 4:
            return chunk.title()
    return None


def infer_publication(text: str, title: str | None, url: str | None) -> str | None:
    if "LexisNexis" in text or "Lexis Nexis" in text:
        before = re.split(r"Lexis\s*Nexis", text)[0]
        parts = [compact_spaces(p) for p in re.split(r"[,;\n]", before) if compact_spaces(p)]
        for part in reversed(parts):
            if any(word in part.lower() for word in ["review", "journal", "law", "studies"]):
                return part
        return None
    if title and title in text:
        after = text.split(title, 1)[1]
        parts = [compact_spaces(p) for p in re.split(r"[,;\n]", after) if compact_spaces(p)]
        for part in parts[:4]:
            part = part.strip(" .,()[]”“\"'")
            if not re.search(r"https?://|accessed|\b\d{1,2}[-/]\d{1,2}", part, re.I):
                if 2 <= len(part.split()) <= 10 and not INSTITUTION.match(part):
                    return part
    if url:
        host = re.sub(r"^https?://", "", url).split("/")[0].lower()
        known = {
            "heritage.org": "The Heritage Foundation",
            "usnews.com": "U.S. News",
            "themoscowtimes.com": "The Moscow Times",
            "brookings.edu": "Brookings Institution",
        }
        for key, name in known.items():
            if key in host:
                return name
    return None


# --------------------------------------------------------------------------- #
# Tail: card signatures + debate annotations
# --------------------------------------------------------------------------- #
def split_tail(text: str) -> tuple[str, list[str], list[str], list[str]]:
    """Peel trailing card signatures and *notes off the end of the cite."""
    warnings: list[str] = []
    signatures: list[str] = []
    annotations: list[str] = []
    body = text.rstrip()

    changed = True
    while changed:
        changed = False
        # A trailing *note.
        m = re.search(r"\s*(\*[^\n]{1,160})$", body)
        if m:
            annotations.append(m.group(1).strip())
            body = body[: m.start()].rstrip()
            changed = True
            continue
        # `// sig` (not part of a URL scheme `http://`).
        m = re.search(r"(?<![:/])//\s*([^\n/]{1,60})$", body)
        if m:
            for piece in re.split(r"[\s,]+", m.group(1).strip()):
                piece = piece.strip(".,;()[]")
                if piece and not piece.startswith("*"):
                    signatures.append(piece)
            body = body[: m.start()].rstrip()
            changed = True
            continue
        # `] SIG` or `) SIG` — short token after a closing bracket at the end.
        m = re.search(r"([)\]])\s*([A-Za-z][A-Za-z0-9_]{1,24})(?:\s+SPRING\d{2})?\s*$", body)
        if m and m.group(2).lower() not in {"the", "in", "and", "et", "al", "no", "vol"}:
            signatures.append(m.group(2).strip(".,;"))
            body = (body[: m.start(2)]).rstrip()
            changed = True
            continue

    m = re.search(r"\brecut\s+([A-Za-z][A-Za-z0-9_-]{1,24})\b", body, re.I)
    if m:
        signatures.append(m.group(1))
        body = (body[: m.start()] + body[m.end():]).strip()

    signatures = list(dict.fromkeys(s for s in signatures if s))
    annotations = list(dict.fromkeys(a for a in annotations if a))
    if signatures and annotations:
        warnings.append("signature_note_fused")
    return body.rstrip(" ,.;"), signatures, annotations, warnings


# --------------------------------------------------------------------------- #
# Spillover
# --------------------------------------------------------------------------- #
_PROSE_MARKERS = re.compile(
    r"\b(the|this|that|these|those|when|whereas|following|instead|because|"
    r"however|therefore|moreover|furthermore|although|despite|according to|"
    r"in the|it is|there is|there are|we|our|they|their)\b",
    re.I,
)


def detect_spillover(raw: str, cite: str) -> tuple[str, int | None, str | None, list[str]]:
    """Split the citation header from article-body spillover.

    `spillover_start_index` is measured against the *raw* input so downstream
    tools can slice the original string.
    """
    warnings: list[str] = []
    # Prefer an explicit newline break.
    if "\n" in cite:
        first, rest = cite.split("\n", 1)
        rest_stripped = rest.strip()
        if rest_stripped and (len(rest_stripped) > 200 or _PROSE_MARKERS.search(rest_stripped[:200])):
            idx = raw.find(first) + len(first)
            warnings.append("body_spillover")
            return first.strip(), (idx if idx >= 0 else len(first)), rest_stripped[:120], warnings
    # No newline: a very long single paragraph whose tail reads like prose.
    if len(cite) > 900:
        # Cut after the first sentence-terminated clause past a plausible header.
        m = re.search(r'["”)\]]\s*[.]\s+[A-Z]', cite[200:])
        if m:
            cut = 200 + m.start() + 2
            head, rest = cite[:cut], cite[cut:].strip()
            if rest and _PROSE_MARKERS.search(rest[:200]) and len(rest) > 200:
                idx = raw.find(head[:60])
                idx = (idx if idx >= 0 else 0) + len(head)
                warnings.append("body_spillover")
                return head.strip(), idx, rest[:120], warnings
    return cite, None, None, warnings


# --------------------------------------------------------------------------- #
# Authors: head/body structural parse
# --------------------------------------------------------------------------- #
def _clean_name_token(tok: str) -> str:
    return tok.strip(".,;:()[]“”‘’\"")


def split_head_names(head: str) -> tuple[list[dict], bool]:
    """Parse the head (before the year anchor) into ordered author skeletons.

    Returns (authors, et_al).  Each author skeleton is {family, given} where
    `given` may be filled if the head carried the full name.
    """
    et_al = False
    h = head
    if re.search(r"\bet\.?\s*al\.?", h, re.I):
        et_al = True
        h = re.sub(r"\bet\.?\s*al\.?", "", h, flags=re.I)
    h = h.strip(" ,–—-[](){}")
    if not h:
        return [], et_al
    # Split multiple authors on conjunctions only (commas in the head are usually
    # `Surname, YY`, not author separators).
    chunks = re.split(r"\s*,?\s+(?:and|&)\s+", h)
    authors: list[dict] = []
    for chunk in chunks:
        chunk = compact_spaces(chunk).strip(" ,")
        # Drop a trailing bare number if it slipped through.
        chunk = re.sub(r"[\s,]+\d{1,4}$", "", chunk).strip(" ,")
        words = [w for w in chunk.split() if w]
        if not words:
            continue
        if len(words) == 1:
            authors.append({"family": _clean_name_token(words[0]), "given": None})
        else:
            family = _clean_name_token(words[-1])
            given = compact_spaces(" ".join(words[:-1]).strip(" ,"))
            authors.append({"family": family or None, "given": given or None})
    return authors, et_al


def _looks_like_name_segment(seg: str) -> bool:
    seg = seg.strip().strip(".")
    if not seg or QUAL_KEYWORDS.search(seg) or INSTITUTION.search(seg):
        return False
    words = seg.split()
    if not (1 <= len(words) <= 4):
        return False
    # Reject ALL-CAPS runs of 3+ words (those are unquoted titles, not names).
    if len(words) >= 3 and seg.isupper():
        return False
    capish = sum(1 for w in words if NAME_TOKEN.match(w))
    return capish >= max(1, len(words) - 1)


def _split_name_conjunctions(seg: str) -> list[str]:
    """`Eve Tuck and K. Wayne Yang` -> [`Eve Tuck`, `K. Wayne Yang`] when both
    sides look like names; otherwise keep the segment whole."""
    parts = re.split(r"\s+(?:and|&)\s+", seg)
    if len(parts) >= 2 and all(_looks_like_name_segment(p) for p in parts):
        return [p.strip() for p in parts]
    return [seg]


def enumerate_body_authors(body_lead: str, title: str | None = None) -> list[dict]:
    """Best-effort enumeration of `Name, quals; Name, quals` in the body.

    Returns [{given, family, literal, qualifications:[blob]}] in order.
    """
    # Cut off at the title (quoted or the detected unquoted title) — title prose
    # must not bleed into an author's qualification blob.
    cuts = [len(body_lead)]
    q = re.search(r"[“\"‘]", body_lead)
    if q:
        cuts.append(q.start())
    if title:
        needle = title[:24].lower()
        tpos = body_lead.lower().find(needle)
        if tpos >= 0:
            cuts.append(tpos)
    scan = body_lead[: min(cuts)].strip(" ,;([—–-")
    if not scan:
        return []

    raw_segments = [s.strip() for s in re.split(r"\s*[;,]\s*", scan) if s.strip()]
    segments: list[str] = []
    for seg in raw_segments:
        segments.extend(_split_name_conjunctions(seg))

    authors: list[dict] = []
    current: dict | None = None
    for seg in segments:
        if _looks_like_name_segment(seg):
            if current:
                authors.append(current)
            words = seg.strip(".").split()
            family = _clean_name_token(words[-1]) if words else None
            given = compact_spaces(" ".join(words[:-1])) if len(words) > 1 else None
            current = {
                "given": given or None,
                "family": family or None,
                "literal": compact_spaces(seg.strip(".")),
                "single": len(words) == 1,
                "quals": [],
            }
        else:
            # Drop bare years/numbers/dates — they are not qualification prose.
            if re.fullmatch(r"[‘’'\"]?\d{1,4}[‘’'\".]?|\d{1,2}[-/]\d{1,2}[-/]\d{2,4}", seg.strip()):
                continue
            if current is None:
                current = {"given": None, "family": None, "literal": None, "single": False, "quals": []}
            current["quals"].append(seg)
    if current:
        authors.append(current)

    out = []
    for a in authors:
        quals = compact_spaces(", ".join(a["quals"])).strip(" ,;.") if a["quals"] else ""
        out.append(
            {
                "given": a["given"],
                "family": a["family"],
                "literal": a["literal"],
                "single": a["single"],
                "qualifications": [quals] if quals else [],
            }
        )
    return out


_GIVEN_LEAD = re.compile(r"^([A-Z][A-Za-z'’-]{1,14})[.,]\s+(.*)$", re.S)


def _peel_given_from_qual(quals: list[str]) -> tuple[str | None, list[str]]:
    """`Vladimir. Russian analyst...` -> given `Vladimir`, quals `Russian ...`."""
    if not quals:
        return None, quals
    m = _GIVEN_LEAD.match(quals[0])
    if not m:
        return None, quals
    token, rest = m.group(1), m.group(2).strip()
    if QUAL_KEYWORDS.search(token) or INSTITUTION.search(token) or token.lower() in MONTH_NAMES:
        return None, quals
    new = [rest] + quals[1:] if rest else quals[1:]
    return token, [q for q in new if q]


def merge_authors(head_authors: list[dict], body_authors: list[dict]) -> list[dict]:
    """Combine head surnames (authoritative family) with body names + quals."""
    result: list[dict] = []
    used_body = [False] * len(body_authors)

    def find_body_match(family: str | None) -> int | None:
        if not family:
            return None
        fam = family.lower()
        for i, b in enumerate(body_authors):
            if used_body[i]:
                continue
            if b.get("family") and b["family"].lower() == fam:
                return i
            if b.get("literal") and fam in b["literal"].lower().split():
                return i
        return None

    for idx, ha in enumerate(head_authors):
        family = ha.get("family")
        given = ha.get("given")
        literal = None
        quals: list[str] = []
        bi = find_body_match(family)
        positional = False
        if bi is None and idx < len(body_authors) and not used_body[idx]:
            b = body_authors[idx]
            if not b.get("family") or _looks_like_name_segment(b.get("literal") or ""):
                bi = idx
                positional = True
        if bi is not None:
            used_body[bi] = True
            b = body_authors[bi]
            quals = b.get("qualifications", [])
            if b.get("single") and family and b.get("family") and b["family"].lower() != family.lower():
                # Reversed order: head gave the surname, body gave a lone given.
                given = b["family"] or given
            else:
                given = b.get("given") or given
                if positional and b.get("family") and not find_body_match(family):
                    # keep head family authoritative; body family becomes given if it looks like one
                    pass
        if not given and quals:
            peeled, quals = _peel_given_from_qual(quals)
            given = peeled or given
        if literal is None:
            literal = compact_spaces(" ".join(x for x in [given, family] if x)) or None
        result.append(
            {
                "index": len(result),
                "family": family,
                "given": given,
                "literal": literal,
                "qualifications": quals,
            }
        )

    # Any body authors not matched to a head surname (et-al expansion), but never
    # promote an institution/qualifier-only fragment to an author.
    for i, b in enumerate(body_authors):
        if used_body[i]:
            continue
        if not (b.get("family") or b.get("given")):
            continue
        result.append(
            {
                "index": len(result),
                "family": b.get("family"),
                "given": b.get("given"),
                "literal": b.get("literal"),
                "qualifications": b.get("qualifications", []),
            }
        )

    for i, a in enumerate(result):
        a["index"] = i
    return result


def find_head_body_split(cite: str, year_span: tuple[int, int] | None) -> tuple[str, str]:
    """Return (head, body).  Head = author+year shorthand; body = the rest."""
    # If there's a clear opening delimiter early on, split there.
    delim = re.search(r"^(.{0,90}?)\s*([(\[—]|--|–\s)", cite)
    if delim and (year_span is None or delim.start(2) >= year_span[0]):
        return cite[: delim.start(2)].strip(), cite[delim.end(2):].strip()
    if year_span is not None:
        # Head ends just after the year token; body is whatever follows.
        head = cite[: year_span[1]].strip()
        body = cite[year_span[1]:].lstrip(" ,—–-([")
        return head, body
    # No year, no delimiter: treat leading name-ish run as head.
    m = re.match(r"([A-Z][A-Za-z.'’-]+(?:\s+(?:and|&)\s+[A-Z][A-Za-z.'’-]+)*)", cite)
    if m:
        return m.group(1).strip(), cite[m.end():].strip(" ,—–-([")
    return cite, ""


def locate_year_span(cite: str) -> tuple[int, int] | None:
    lead = cite[:160]
    for pat in (
        r"[‘’']\s*\d{2}\b(?![\d/-])",
        r"\bet\.?\s*al\.?\s*[‘’']?\s*\d{1,2}\b(?![\d/-])",
        r"\b\d{1,2}[-/]\d{1,2}[-/]\d{2,4}\b",
        r"\b2[kK]\b",
    ):
        m = re.search(pat, lead, re.I)
        if m:
            return m.start(), m.end()
    # name + short year
    for m in re.finditer(
        r"\b[A-Z][A-Za-z.'’&-]{1,40}(?:\s+(?:and|&)\s+[A-Z][A-Za-z.'’&-]{1,40})?"
        r"(?:\s+et\.?\s*al\.?)?\s*,?\s*[‘’']?(\d{1,2})\b(?![\d/-])",
        lead,
    ):
        return m.start(1), m.end(1)
    m = re.search(r"\b(19\d{2}|20\d{2})\b", lead)
    if m:
        return m.start(), m.end()
    return None


# --------------------------------------------------------------------------- #
# Source type
# --------------------------------------------------------------------------- #
def source_type(text: str, publication: str | None, url: str | None) -> str:
    lower = text.lower()
    if "law review" in lower or "l. rev" in lower:
        return "law_review"
    if "dissertation" in lower or "thesis" in lower:
        return "thesis"
    if "podcast" in lower or "interview" in lower:
        return "interview"
    if "dictionary" in lower or "merriam" in lower or "lexico" in lower:
        return "dictionary_or_reference"
    if " v. " in lower or re.search(r"\b\d+\s+U\.?S\.?\s+\d+\b", text) or "supreme court" in lower:
        return "legal_source"
    if (
        "journal" in lower
        or "vol." in lower
        or "volume" in lower
        or "jstor" in lower
        or "proceedings" in lower
        or re.search(r"\b\d+\.\d+\s*\(\d{4}\)", text)
        or re.search(r"\b\d+\s*\(\d{4}\)\s*\d+", text)
    ):
        return "journal_article"
    if "dissertation" in lower or "ph.d" in lower and "university" in lower and not url:
        return "thesis"
    if "report" in lower or "foundation" in lower or "institute" in lower:
        return "report"
    if "book" in lower or "press" in lower:
        return "book"
    if url:
        host = re.sub(r"^https?://", "", url).split("/")[0].lower()
        if any(x in host for x in ["merriam-webster", "dictionary", "britannica", "wikipedia", "wikiwand", "lexico"]):
            return "dictionary_or_reference"
        news = ["news", "politico", "times", "post", "cnn", "forbes", "fortune",
                "vox", "reuters", "apnews", "bloomberg", "theatlantic", "newyorker",
                "guardian", "npr", "bbc", "hill", "slate"]
        if any(x in (lower + " " + host) for x in news):
            return "news_article"
        return "web_page"
    return "unknown"


# --------------------------------------------------------------------------- #
# Reject / assembly
# --------------------------------------------------------------------------- #
PLACEHOLDERS = {"insert", "ibid", "id", "id.", ".", "---", "///marked///", "n/a", "tbd", "xx"}
TAG_PREFIX = re.compile(
    r"^\s*(at\b|a2\b|answers? to\b|2ac\b|1ar\b|2nr\b|1nc\b|2nc\b|overview\b|"
    r"impact\b|uniqueness\b|link\b|internal link\b|solvency\b|turn\b|case\b|"
    r"framework\b|fw\b|perm\b|cp\b|da\b|kritik\b|\bk\b)",
    re.I,
)


def reject(reason: str, evidence: str, warnings: list[str], meta: dict) -> dict:
    return {
        "status": "reject",
        "reject_reason": reason,
        "evidence": evidence[:240],
        "warnings": list(dict.fromkeys(warnings)),
        "_meta": meta,
    }


def parsed_base() -> dict:
    return {
        "status": "parsed",
        "authors": [],
        "year": None,
        "no_date": False,
        "published_date": None,
        "accessed_date": None,
        "retrieved_date": None,
        "title": None,
        "container_title": None,
        "publication": None,
        "publisher": None,
        "volume": None,
        "issue": None,
        "pages": None,
        "url": None,
        "doi": None,
        "database": None,
        "source_type": "unknown",
        "card_signatures": [],
        "debate_annotations": [],
        "raw_tail": None,
        "spillover_start_index": None,
        "spillover_start_text": None,
        "warnings": [],
        "_meta": {"confidence": "medium", "inferred_fields": []},
    }


def _no_date(cite: str) -> bool:
    # Real no-date markers, excluding legal districts like `N.D. Cal.`.
    if re.search(r"\bno date\b", cite, re.I):
        return True
    for m in re.finditer(r"(?<![A-Za-z])[nN]\.?\s?[dD]\.?(?![A-Za-z])", cite):
        after = cite[m.end():m.end() + 6]
        if re.match(r"\s*(Cal|Tex|N\.?Y|Ill|Pa|Fla|Va|Ohio|Mich|Ga|Wash|Mo|La|Ind|Ala|Miss|Okla)\b", after):
            continue
        return True
    return False


def label_row(row: dict) -> dict:
    raw = row["fullcite"]
    text = compact_spaces(raw) if "\n" not in raw else raw.strip()
    stripped_lower = text.strip().lower()

    # ---- reject gate ----
    if stripped_lower in PLACEHOLDERS or len(text.strip()) <= 1:
        reason = "cross_reference_only" if stripped_lower.startswith(("ibid", "id")) else "empty_or_placeholder"
        return reject(reason, text, ["not_a_citation", "malformed"], {"confidence": "high", "inferred_fields": []})
    if stripped_lower.startswith("note:") and not re.search(r"\b(19\d{2}|20\d{2})\b|[‘’']\d{2}\b|\b2[kK]\b", text):
        return reject("analytic_or_tag", text, ["not_a_citation"], {"confidence": "medium", "inferred_fields": []})
    has_source_signal = bool(re.search(r"https?://|www\.|[“\"].+[”\"]|\bpress\b|\bjournal\b", text, re.I))
    if (row.get("is_likely_reject") or TAG_PREFIX.match(text)) and not has_source_signal and not re.search(r"\b(19\d{2}|20\d{2})\b", text):
        return reject("analytic_or_tag", text, ["not_a_citation"], {"confidence": "medium", "inferred_fields": []})

    label = parsed_base()
    meta = label["_meta"]

    # ---- tail then spillover ----
    cite, signatures, annotations, tail_warn = split_tail(text)
    cite, spill_idx, spill_text, spill_warn = detect_spillover(raw, cite)
    label["card_signatures"] = signatures
    label["debate_annotations"] = annotations
    label["spillover_start_index"] = spill_idx
    label["spillover_start_text"] = spill_text
    label["warnings"].extend(tail_warn + spill_warn)

    # ---- structured fields ----
    label["url"] = extract_url(cite)
    url_title, url_pub, url_stype = title_from_url(label["url"])
    label["doi"] = extract_doi(cite)
    label["pages"], page_ambiguous = extract_pages(cite)
    if page_ambiguous:
        label["warnings"].append("page_range_ambiguous")
    label["volume"], label["issue"] = extract_volume_issue(cite)
    label["database"] = extract_database(cite, label["url"])

    # ---- dates / year ----
    label["no_date"] = _no_date(cite)
    if label["no_date"]:
        label["warnings"].append("no_date")
    year, inferred = find_year(cite)
    if label["no_date"] and not year:
        label["year"] = None
    else:
        label["year"] = year
        if inferred and year:
            meta["inferred_fields"].append("year")
    if not year and not label["no_date"] and not (label["url"] and compact_spaces(cite) == label["url"]):
        label["warnings"].append("ambiguous_year")

    # ---- title ----
    label["title"] = quoted_title(cite) or url_title or allcaps_title(cite)

    published, accessed, retrieved = extract_dates(cite, label["title"], label["url"])
    label["published_date"] = published
    label["accessed_date"] = accessed
    label["retrieved_date"] = retrieved
    pub_year = year_from_date(published)
    year_span = locate_year_span(cite)
    if pub_year and year_span is None:
        label["year"] = pub_year
        meta["inferred_fields"] = [f for f in meta["inferred_fields"] if f != "year"]

    # ---- publication ----
    label["publication"] = infer_publication(cite, label["title"], label["url"]) or url_pub

    # ---- authors (head/body structural) ----
    et_al = False
    mla = re.match(
        r"^([A-Z][A-Za-z'’-]+),\s+([A-Z][A-Za-z.'’ -]{1,40}?)\.?\s*[\"“]",
        cite,
    )
    if mla and year_span is not None and mla.end() <= year_span[0] + 4:
        family = _clean_name_token(mla.group(1))
        given = compact_spaces(mla.group(2))
        authors = [{
            "index": 0, "family": family, "given": given,
            "literal": compact_spaces(f"{given} {family}"), "qualifications": [],
        }]
    else:
        head, body = find_head_body_split(cite, year_span)
        head_authors, et_al = split_head_names(head)
        body_authors = enumerate_body_authors(body, label["title"])
        authors = merge_authors(head_authors, body_authors)
    if not authors and label["publication"]:
        authors = [{"index": 0, "family": None, "given": None, "literal": label["publication"], "qualifications": []}]
    label["authors"] = authors
    if et_al:
        label["warnings"].append("et_al")
    if not authors:
        label["warnings"].append("ambiguous_author")
    elif any(a["qualifications"] for a in authors):
        pass

    # ---- source type ----
    label["source_type"] = url_stype or source_type(cite, label["publication"], label["url"])
    if label["source_type"] == "unknown":
        label["warnings"].append("source_type_ambiguous")

    # ---- url-only / incomplete ----
    if label["url"] and compact_spaces(cite).strip("[]() .") == label["url"].strip("[]() ."):
        label["warnings"].append("url_only")
        label["warnings"] = [w for w in label["warnings"] if w not in {"ambiguous_author", "ambiguous_year", "source_type_ambiguous"}]
    if len(cite) < 50 or (not label["title"] and not label["url"]):
        label["warnings"].append("incomplete_citation")
        meta["confidence"] = "low"

    # ---- date conflict (materially different pub years only) ----
    if label["published_date"] and label["published_date"] == label["accessed_date"]:
        label["published_date"] = None
        pub_year = None
    pub_year = year_from_date(label["published_date"])
    if pub_year and label["year"] and pub_year != label["year"]:
        label["warnings"].append("conflicting_dates")

    # ---- confidence ----
    if meta["confidence"] != "low":
        hard = row.get("primary_stratum") in {"very_long_spillover", "long", "et_al", "author_qualifications", "multiple_date_signals"}
        many_authors = len(label["authors"]) >= 2
        if (hard and label["warnings"]) or (et_al and many_authors and any(not a["qualifications"] for a in label["authors"])):
            meta["confidence"] = "low"
        elif label["title"] and label["year"] and label["authors"]:
            meta["confidence"] = "high"

    label["warnings"] = [w for w in dict.fromkeys(label["warnings"]) if w in WARNING_ENUM]
    meta["inferred_fields"] = list(dict.fromkeys(meta["inferred_fields"]))
    return label


# --------------------------------------------------------------------------- #
# Sparse training target
# --------------------------------------------------------------------------- #
TARGET_KEY_ORDER = [
    "status", "authors", "year", "no_date", "published_date", "accessed_date",
    "retrieved_date", "title", "container_title", "publication", "publisher",
    "volume", "issue", "pages", "url", "doi", "database", "source_type",
    "card_signatures", "debate_annotations", "raw_tail",
    "spillover_start_index", "spillover_start_text", "warnings",
    "reject_reason", "evidence",
]
AUTHOR_KEY_ORDER = ["family", "given", "literal", "qualifications"]
MAX_TARGET_AUTHORS = 12
MAX_QUAL_CHARS = 400


def _nonempty(value) -> bool:
    return value not in (None, "", [], {}, False)


def to_target(label: dict) -> dict:
    """Produce the sparse seq2seq target: only populated fields, canonical order.

    Drops labeler bookkeeping (`_meta`: confidence/inferred_fields) and the
    per-author `index` (recoverable from array position).  Warnings are filtered
    to the semantically-useful, non-redundant set.
    """
    out: dict = {"status": label["status"]}
    if label["status"] == "reject":
        out["reject_reason"] = label["reject_reason"]
        if _nonempty(label.get("evidence")):
            out["evidence"] = label["evidence"]
        warns = [w for w in label.get("warnings", []) if w in TARGET_WARNINGS or w in {"not_a_citation", "cross_reference_only", "malformed"}]
        if warns:
            out["warnings"] = warns
        return _ordered(out)

    for key in TARGET_KEY_ORDER:
        if key in ("status", "authors", "warnings", "no_date"):
            continue
        if key in label and _nonempty(label[key]):
            out[key] = label[key]
    # source_type is always meaningful for a parsed cite (incl. "unknown").
    out["source_type"] = label.get("source_type", "unknown")
    if label.get("no_date"):
        out["no_date"] = True
    # authors (sparse per author). Target-side sanity caps keep pathological
    # mega-cites (dozens of signatories, runaway qual blobs) from producing
    # unlearnable multi-kilobyte targets; the full label is left untouched.
    authors = []
    for a in label.get("authors", [])[:MAX_TARGET_AUTHORS]:
        ao = {}
        for k in AUTHOR_KEY_ORDER:
            if not _nonempty(a.get(k)):
                continue
            if k == "qualifications":
                ao[k] = [q[:MAX_QUAL_CHARS].rstrip() for q in a[k]]
            else:
                ao[k] = a[k]
        authors.append(ao)
    if authors:
        out["authors"] = authors
    warns = [w for w in label.get("warnings", []) if w in TARGET_WARNINGS]
    if warns:
        out["warnings"] = warns
    return _ordered(out)


def _ordered(out: dict) -> dict:
    order = {k: i for i, k in enumerate(TARGET_KEY_ORDER)}
    return {k: out[k] for k in sorted(out, key=lambda k: order.get(k, 999))}


def normalize_target(t: dict) -> dict:
    """Canonicalize an externally-produced (LLM) target into the exact sparse
    shape ``to_target`` emits: populated fields only, canonical order, single
    qualification blob per author, target-side caps applied."""
    status = t.get("status", "parsed")
    if status == "reject":
        out = {"status": "reject", "reject_reason": t.get("reject_reason", "not_a_citation")}
        if _nonempty(t.get("evidence")):
            out["evidence"] = str(t["evidence"])[:240]
        warns = [w for w in t.get("warnings", []) if w in TARGET_WARNINGS or w in {"not_a_citation", "cross_reference_only", "malformed"}]
        if warns:
            out["warnings"] = warns
        return _ordered(out)

    out = {"status": "parsed"}
    for key in TARGET_KEY_ORDER:
        if key in ("status", "authors", "warnings", "no_date", "source_type"):
            continue
        if _nonempty(t.get(key)):
            out[key] = t[key]
    out["source_type"] = t.get("source_type", "unknown") if t.get("source_type") in SOURCE_TYPES else "unknown"
    if t.get("no_date"):
        out["no_date"] = True
    authors = []
    for a in t.get("authors", [])[:MAX_TARGET_AUTHORS]:
        ao = {}
        for k in AUTHOR_KEY_ORDER:
            if not _nonempty(a.get(k)):
                continue
            if k == "qualifications":
                q = a[k] if isinstance(a[k], list) else [a[k]]
                blob = compact_spaces(" ".join(str(x) for x in q)).strip()
                if blob:
                    ao[k] = [blob[:MAX_QUAL_CHARS].rstrip()]
            else:
                ao[k] = a[k]
        if ao:
            authors.append(ao)
    if authors:
        out["authors"] = authors
    warns = [w for w in t.get("warnings", []) if w in TARGET_WARNINGS]
    if warns:
        out["warnings"] = warns
    return _ordered(out)


# --------------------------------------------------------------------------- #
# Validation
# --------------------------------------------------------------------------- #
def validate_label(label: dict) -> None:
    if label["status"] == "reject":
        for key in ["status", "reject_reason", "evidence", "warnings"]:
            if key not in label:
                raise ValueError(f"reject label missing {key}")
    elif label["status"] == "parsed":
        required = set(parsed_base()) - {"_meta"}
        missing = required - set(label)
        if missing:
            raise ValueError(f"parsed label missing {sorted(missing)}")
        if label["source_type"] not in SOURCE_TYPES:
            raise ValueError(f"bad source_type {label['source_type']}")
        for a in label["authors"]:
            if set(a) - {"index", "family", "given", "literal", "qualifications"}:
                raise ValueError(f"bad author keys {sorted(a)}")
            if not isinstance(a.get("qualifications"), list):
                raise ValueError("qualifications must be a list")
    else:
        raise ValueError(f"bad status {label.get('status')}")
    bad = set(label.get("warnings", [])) - WARNING_ENUM
    if bad:
        raise ValueError(f"bad warnings {sorted(bad)}")


def needs_review(row: dict, label: dict) -> bool:
    meta = label.get("_meta", {})
    if label["status"] == "reject":
        return False
    if meta.get("confidence") == "low":
        return True
    review_warnings = {
        "conflicting_dates", "malformed", "ambiguous_author",
        "signature_note_fused", "unparsed_tail", "multiple_titles",
        "multiple_authors_unenumerated",
    }
    return bool(set(label.get("warnings", [])) & review_warnings)


def main() -> None:
    args = parse_args()
    with args.input.open(encoding="utf-8") as src:
        rows = [json.loads(line) for line in src]
    args.output.parent.mkdir(parents=True, exist_ok=True)

    by_stratum: Counter[str] = Counter()
    by_status: Counter[str] = Counter()
    by_warning: Counter[str] = Counter()
    by_confidence: Counter[str] = Counter()
    review_count = 0

    with args.output.open("w", encoding="utf-8") as out, args.review.open("w", encoding="utf-8") as review:
        for row in rows:
            label = label_row(row)
            validate_label(label)
            record = {
                "id": row["id"],
                "primary_stratum": row["primary_stratum"],
                "fullcite": row["fullcite"],
                "label_json": label,
            }
            out.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
            by_stratum[row["primary_stratum"]] += 1
            by_status[label["status"]] += 1
            by_warning.update(label.get("warnings", []))
            by_confidence[label.get("_meta", {}).get("confidence", "n/a")] += 1
            if needs_review(row, label):
                review.write(json.dumps(record, ensure_ascii=False, separators=(",", ":")) + "\n")
                review_count += 1

    summary = {
        "input": str(args.input),
        "output": str(args.output),
        "review": str(args.review),
        "row_count": len(rows),
        "review_count": review_count,
        "by_stratum": dict(by_stratum),
        "by_status": dict(by_status),
        "by_confidence": dict(by_confidence),
        "by_warning": dict(by_warning),
    }
    args.summary.write_text(json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
