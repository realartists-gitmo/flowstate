"""Seed channels and discovery queries for competitive debate rounds."""

from __future__ import annotations

# Verified channel entry points (handle or full URL). Enumeration walks
# /videos, /streams, and playlists for each.
SEED_CHANNELS: list[dict[str, str]] = [
    {
        "id": "cedadebate",
        "name": "CEDA Debate",
        "url": "https://www.youtube.com/user/cedadebate",
    },
    {
        "id": "michigan-debate",
        "name": "Michigan Debate",
        "url": "https://www.youtube.com/channel/UC2gnLZUFVVhjTy4UL9MhvRg",
    },
    {
        "id": "georgetown-debate-seminar",
        "name": "Georgetown Debate Seminar",
        "url": "https://www.youtube.com/channel/UCK_L-9TX6z4rMCeh8X-oTyA",
    },
    {
        "id": "nsda",
        "name": "National Speech & Debate Association",
        "url": "https://www.youtube.com/c/NationalSpeechDebateAssociation",
    },
    {
        "id": "policy-debate-central",
        "name": "Policy Debate Central",
        "url": "https://www.youtube.com/@PolicyDebateCentral",
    },
]

# Names to resolve via ytsearch → channel of best matching result.
# `expect` must match channel name (or debate-titled hit). Prefer narrow patterns.
# Optional `url` skips search when already known.
RESOLVE_NAMES: list[dict[str, str]] = [
    {"id": "ddi", "query": "DDI Debate Dartmouth", "expect": r"\bDDI\b|Dartmouth.*Debate"},
    {
        "id": "endi",
        "query": "Emory National Debate Institute",
        "expect": r"Emory.*Debate|ENDI",
    },
    {
        "id": "cndi",
        "query": "\"CNDI\" Berkeley debate",
        "expect": r"\bCNDI\b|Berkeley.*Debate Institute",
    },
    {
        "id": "wake-debate",
        "query": "\"Wake Forest Debate\" channel",
        "expect": r"Wake Forest Debate",
    },
    {
        "id": "nhsi",
        "query": "\"NHSI\" Northwestern debate",
        "expect": r"\bNHSI\b|Northwestern.*Debate",
    },
    {
        "id": "baylor-debate",
        "query": "\"Baylor Debate\"",
        "expect": r"Baylor Debate",
    },
    {
        "id": "utnif",
        "query": "\"UTNIF\" debate",
        "expect": r"\bUTNIF\b",
    },
    {
        "id": "sdi",
        "query": "MSU Debate Spartan Debate Institute",
        "expect": r"MSU Debate|Spartan Debate|\bSDI\b",
    },
    {
        "id": "gdi",
        "query": "Gonzaga Debate Institute",
        "expect": r"Gonzaga Debate",
    },
    {
        "id": "kentucky-debate",
        "query": "\"Kentucky Debate\" UK",
        "expect": r"Kentucky Debate",
    },
    {
        "id": "samford-debate",
        "query": "\"Samford Debate\"",
        "expect": r"Samford.*Debate|Samford Communication",
    },
    {
        "id": "harvard-debate",
        "query": "\"Harvard Debate Council\"",
        "expect": r"Harvard Debate",
    },
    {
        "id": "naudl",
        "query": "NAUDL debate",
        "expect": r"\bNAUDL\b",
    },
    {
        "id": "toc",
        "query": "\"Tournament of Champions\" debate rounds",
        "expect": r"Tournament of Champions|\bTOC\b.*Debate",
    },
    {
        "id": "greenhill-debate",
        "query": "\"Greenhill\" debate tournament",
        "expect": r"Greenhill.*Debate",
    },
    {
        "id": "glenbrooks",
        "query": "\"Glenbrooks\" debate",
        "expect": r"Glenbrooks",
    },
]

# Stage A: search only to find channels (not as the round corpus).
SEARCH_QUERIES: list[str] = [
    # Policy
    "policy debate finals",
    "policy debate round NDT",
    "CEDA debate finals",
    "CX debate round octafinals",
    "policy debate quarterfinals",
    "NDT debate finals",
    "TOC policy debate",
    "Greenhill policy debate",
    "Glenbrooks policy debate",
    "policy debate aff vs neg",
    # PF / LD / WS
    "public forum debate finals",
    "PF debate round TOC",
    "Lincoln Douglas debate finals",
    "LD debate round TOC",
    "World Schools debate round",
    "WSDC debate finals",
    # Camps / institutes
    "DDI debate round",
    "ENDI debate round",
    "UTNIF debate round",
    "Michigan debate round livestream",
]
