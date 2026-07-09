"""Round classification heuristics for debate video metadata."""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

# Positive: looks like a competitive round
RE_AFF_NEG = re.compile(r"\b(aff|neg)\b", re.I)
RE_VS = re.compile(r"\bvs?\.?\b", re.I)
RE_ROUND_NUM = re.compile(r"\bround\s*[1-8]\b", re.I)
RE_ELIM = re.compile(
    r"\b(octas?|octos?|doubles|triples|quarters|quads|semis?|finals?|elims?)\b",
    re.I,
)
RE_TEAMISH = re.compile(
    r"\b[A-Z][a-z]+(?:\s+[A-Z][a-z]+)?\s+(?:vs?\.?|v\.?)\s+[A-Z][a-z]+",
)

# Format signals
RE_POLICY = re.compile(r"\b(CX|cross.?ex(?:amination)?|policy(?:\s+debate)?)\b", re.I)
RE_PF = re.compile(r"\b(public\s+forum|PF)\b", re.I)
RE_LD = re.compile(r"\b(lincoln.?douglas|LD)\b", re.I)
RE_WS = re.compile(r"\b(world\s+schools|WSDC)\b", re.I)

# Non-round (high confidence drop when stacked)
RE_NON_ROUND = re.compile(
    r"\b(lecture|seminar|how\s+to|intro(?:duction)?\s+to|awards?|"
    r"announcement|promo|promotional|Q\s*&\s*A|trailer|recruiting|"
    r"webinar|workshop\s+series|keynote|topic\s+meeting|"
    r"lecture\s+series|camp\s+promo|disads?\s+on\s+the|"
    r"debating\s+\w+\s+on\s+the\s+\d{4}|"
    r"info\s+session|prepare\s+for|final\s+tips|tips\s+for|"
    r"breakdown|coach(?:es)?|judge\s+training|tabroom|"
    r"registration|welcome\s+ceremony|opening\s+ceremony|"
    r"springboard|highlights?\b|recap\b|montage|"
    r"big\s+questions|"  # BQ is out of mission scope
    r"extemp|oratory|interp|duo\s+interp|humorous|dramatic|"
    r"congress(?:ional)?\s+debate|student\s+congress|"
    r"speech\s+finals?|speech\s+round|"
    r"presidential\s+debate|democratic\s+national\s+convention|"
    r"republican\s+national\s+convention|town\s+hall|"
    r"\bobama\b|\bromney\b|\bbernie\s+sanders\b|"
    r"climate\s+change\s+solutions?|megaprojects)\b",
    re.I,
)

# Strong structural round cues (survive a single weak non-round word)
RE_STRONG_ROUND = re.compile(
    r"(\b(aff|neg)\b.*\bvs?\.?\b|\bvs?\.?\b.*\b(aff|neg)\b|"
    r"\bround\s*[1-8]\b|"
    r"\b(octas?|octos?|doubles|triples|quarters|semis?|finals?)\b.*\bvs?\.?\b|"
    r"\bvs?\.?\b.*\b(octas?|octos?|doubles|triples|quarters|semis?|finals?)\b)",
    re.I,
)

# Explicit in-scope format + competitive framing (helps multi-event orgs like NSDA)
RE_IN_SCOPE_EVENT = re.compile(
    r"\b(policy|CX|public\s+forum|\bPF\b|lincoln.?douglas|\bLD\b|"
    r"world\s+schools|WSDC)\b.{0,40}\b(round|finals?|semis?|quarters?|"
    r"octas?|elim|vs?\.?)\b|"
    r"\b(round|finals?|semis?|quarters?|octas?|elim|vs?\.?).{0,40}"
    r"\b(policy|CX|public\s+forum|\bPF\b|lincoln.?douglas|\bLD\b|"
    r"world\s+schools|WSDC)\b",
    re.I,
)

# Seed / channel names that imply policy when format is otherwise unknown
POLICY_CHANNEL_HINTS = (
    "ceda",
    "policy debate central",
    "michigan debate",
    "georgetown debate",
    "utnif",
    "ndi",
    "nhsi",
    "gdi",
    "sdi",
    "cndi",
    "endi",
    "ddi",
    "wake forest debate",
    "kentucky debate",
    "baylor debate",
    "samford debate",
    "debate stream",
    "ndt stream",
    "ndt debate",
    "exodus files",
    "barkley forum",
    "msu debate",
    "umkc debate",
    "vintage debate",
    "lasa debate",
)

# Multi-event orgs: require stronger evidence before counting as a round
STRICT_CHANNEL_HINTS = (
    "national speech",
    "speech & debate association",
    "speech and debate association",
    "nsda",
)

TOURNAMENT_HINTS = (
    "ndt",
    "ceda",
    "toc",
    "greenhill",
    "glenbrooks",
    "berkeley",
    "emory",
    "wake forest",
    "harvard",
    "northwestern",
    "michigan",
    "kentucky",
    "baylor",
    "samford",
    "gonzaga",
    "utnif",
    "cndi",
    "endi",
    "ddi",
    "nhsi",
    "naudl",
)


@dataclass
class Classification:
    is_round: bool | None  # None = uncertain
    format: str  # policy | pf | ld | world_schools | unknown
    round_label: str
    confidence: float
    drop: bool  # high-confidence non-round


def _blob(title: str, description: str, channel: str, playlist: str) -> str:
    return " | ".join(x for x in (title, description, channel, playlist) if x)


def _format_scores(text: str) -> dict[str, float]:
    scores = {
        "policy": 0.0,
        "pf": 0.0,
        "ld": 0.0,
        "world_schools": 0.0,
    }
    if RE_POLICY.search(text):
        scores["policy"] += 0.45
    if RE_PF.search(text):
        scores["pf"] += 0.55
    if RE_LD.search(text):
        scores["ld"] += 0.55
    if RE_WS.search(text):
        scores["world_schools"] += 0.55
    lower = text.lower()
    if any(k in lower for k in ("ceda", "policy debate", "cx debate", "ndt")):
        scores["policy"] += 0.15
    return scores


def _round_label(text: str) -> str:
    lower = text.lower()
    if re.search(r"\bfinals?\b", lower) and not re.search(
        r"\b(semi|quarter|octa|octo|double|triple)", lower
    ):
        return "elim/finals"
    if re.search(r"\bsemi", lower):
        return "elim/semis"
    if re.search(r"\bquarter", lower):
        return "elim/quarters"
    if re.search(r"\b(octas?|octos?)\b", lower):
        return "elim/octas"
    if re.search(r"\bdoubles?\b", lower):
        return "elim/doubles"
    if re.search(r"\btriples?\b", lower):
        return "elim/triples"
    if re.search(r"\belims?\b", lower):
        return "elim/unknown"
    m = re.search(r"\bround\s*([1-8])\b", lower)
    if m:
        return f"prelim/r{m.group(1)}"
    if re.search(r"\b(practice|lab|scrimmage)\b", lower):
        return "practice"
    return "unknown"


def _is_strict_channel(channel: str) -> bool:
    ch = channel.lower()
    return any(h in ch for h in STRICT_CHANNEL_HINTS)


def classify(
    *,
    title: str = "",
    description: str = "",
    channel: str = "",
    playlist_title: str = "",
    duration_sec: int | None = None,
) -> Classification:
    text = _blob(title, description, channel, playlist_title)
    lower = text.lower()
    title_l = (title or "").lower()
    strict_ch = _is_strict_channel(channel)

    pos = 0.0
    if RE_AFF_NEG.search(text):
        pos += 0.25
    if RE_VS.search(text):
        pos += 0.2
    if RE_ROUND_NUM.search(text):
        pos += 0.25
    if RE_ELIM.search(text):
        pos += 0.3
    if RE_TEAMISH.search(title or ""):
        pos += 0.15
    if any(t in lower for t in TOURNAMENT_HINTS):
        pos += 0.1
    if re.search(r"\bdebate\b", lower):
        pos += 0.05
    if RE_IN_SCOPE_EVENT.search(text):
        pos += 0.25

    non = 0.0
    non_hits = RE_NON_ROUND.findall(text)
    if non_hits:
        non += 0.35 * min(len(non_hits), 3)

    # Soft title-only promo / meta content (common on NSDA)
    promo_bits = (
        "prepare for",
        "final tips",
        "info session",
        "for coaches",
        "for judges",
        "welcome to",
        "what is ",
        "why join",
        "registration",
        "highlight reel",
        "season preview",
    )
    if any(b in title_l for b in promo_bits):
        non += 0.4

    # Duration soft signal
    if duration_sec is not None:
        if duration_sec < 12 * 60:
            non += 0.25
            pos -= 0.1
        elif duration_sec < 20 * 60:
            non += 0.12
            pos -= 0.05
        elif 45 * 60 <= duration_sec <= 180 * 60:
            pos += 0.2
        elif duration_sec > 20 * 60:
            pos += 0.05

    fmt_scores = _format_scores(text)
    best_fmt = max(fmt_scores, key=fmt_scores.get)
    best_fmt_score = fmt_scores[best_fmt]
    if best_fmt_score < 0.2:
        fmt = "unknown"
    else:
        fmt = best_fmt
        pos += min(best_fmt_score, 0.2)

    strong = bool(RE_STRONG_ROUND.search(text))
    if strong:
        pos += 0.15

    ch_lower = channel.lower()
    if fmt == "unknown" and any(h in ch_lower for h in POLICY_CHANNEL_HINTS):
        if not RE_PF.search(text) and not RE_LD.search(text) and not RE_WS.search(text):
            fmt = "policy"

    # Strict channels (NSDA etc.): drop unless clear in-scope competitive round
    if strict_ch and not strong and not RE_IN_SCOPE_EVENT.search(text):
        # Keep only if very strong round-ish signal + in-scope format word
        has_fmt = best_fmt_score >= 0.4 or fmt in ("policy", "pf", "ld", "world_schools")
        if not (has_fmt and pos >= 0.55 and non < 0.25):
            return Classification(
                is_round=False,
                format=fmt,
                round_label="non_round",
                confidence=0.7,
                drop=True,
            )

    # High-confidence non-round: non-round language without strong round structure
    if non_hits and not strong and (non >= 0.3 or pos < 0.4):
        return Classification(
            is_round=False,
            format=fmt,
            round_label="non_round",
            confidence=min(0.95, 0.55 + non - min(pos, 0.3)),
            drop=True,
        )

    if non >= 0.5 and pos < 0.35 and not strong:
        return Classification(
            is_round=False,
            format=fmt,
            round_label="non_round",
            confidence=min(0.95, 0.55 + non - pos),
            drop=True,
        )

    # Short unknown promos without structure
    if (
        duration_sec is not None
        and duration_sec < 15 * 60
        and not strong
        and fmt == "unknown"
        and pos < 0.5
    ):
        return Classification(
            is_round=False,
            format=fmt,
            round_label="non_round",
            confidence=0.65,
            drop=True,
        )

    if pos >= 0.45 and non < 0.35:
        conf = min(0.98, 0.4 + pos - non * 0.5)
        if fmt == "unknown" and strong:
            fmt = "policy" if any(h in ch_lower for h in POLICY_CHANNEL_HINTS) else fmt
        return Classification(
            is_round=True,
            format=fmt,
            round_label=_round_label(text),
            confidence=conf,
            drop=False,
        )

    if (pos >= 0.25 and non < pos) or strong:
        if fmt == "unknown" and any(h in ch_lower for h in POLICY_CHANNEL_HINTS):
            fmt = "policy"
        return Classification(
            is_round=True,
            format=fmt,
            round_label=_round_label(text),
            confidence=max(0.35, pos - non + (0.2 if strong else 0)),
            drop=False,
        )

    # Uncertain: keep (except strict channels already handled)
    if non > pos and non >= 0.35 and not strong:
        return Classification(
            is_round=False,
            format=fmt,
            round_label="non_round",
            confidence=min(0.75, non),
            drop=non >= 0.45 and pos < 0.25,
        )

    return Classification(
        is_round=None,
        format=fmt,
        round_label=_round_label(text),
        confidence=0.3,
        drop=False,
    )


def classify_row(row: dict[str, Any]) -> Classification:
    dur = row.get("duration_sec")
    if dur is not None and dur != "":
        try:
            dur_i = int(float(dur))
        except (TypeError, ValueError):
            dur_i = None
    else:
        dur_i = None
    return classify(
        title=row.get("title") or "",
        description=row.get("description") or "",
        channel=row.get("channel") or "",
        playlist_title=row.get("playlist_title") or "",
        duration_sec=dur_i,
    )
