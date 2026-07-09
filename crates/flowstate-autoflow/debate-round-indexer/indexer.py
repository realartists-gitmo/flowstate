#!/usr/bin/env python3
"""Competitive debate round URL indexer (YouTube metadata only).

See ../policy-debate-round-indexer-spec.md
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from classify import classify_row
from seeds import RESOLVE_NAMES, SEARCH_QUERIES, SEED_CHANNELS
from store import Store

HERE = Path(__file__).resolve().parent
DEFAULT_DB = HERE.parent / "rounds.sqlite"
DEFAULT_JSONL = HERE.parent / "rounds.jsonl"

PRINT_SPEC = "\t".join(
    [
        "%(id)s",
        "%(webpage_url)s",
        "%(title)s",
        "%(channel)s",
        "%(channel_url)s",
        "%(upload_date)s",
        "%(duration)s",
        "%(view_count)s",
        "%(playlist_title)s",
        "%(description)s",
    ]
)

ENRICH_PRINT = "\t".join(
    [
        "%(id)s",
        "%(title)s",
        "%(channel)s",
        "%(channel_url)s",
        "%(upload_date)s",
        "%(duration)s",
        "%(view_count)s",
        "%(description)s",
    ]
)

# Discovery denylist (substring match on channel name)
CHANNEL_DENY_SUBSTRINGS = (
    "vevo",
    "music",
    "official artist",
    "movieclips",
    "tedx",
    "wired",
    "big think",
    "inside edition",
    "the boston globe",
    "cbs",
    "nbc",
    "cnn",
    "fox news",
    "msnbc",
    "espn",
    "netflix",
    "disney",
    "sony",
    "peter schiff",
)


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def find_yt_dlp() -> str:
    path = shutil.which("yt-dlp")
    if not path:
        raise SystemExit(
            "yt-dlp not found on PATH. Install with: uv tool install yt-dlp"
        )
    return path


def check_yt_dlp(yt_dlp: str, update: bool) -> str:
    if update:
        subprocess.run([yt_dlp, "-U"], check=False)
    ver = subprocess.check_output([yt_dlp, "--version"], text=True).strip()
    print(f"yt-dlp {ver} ({yt_dlp})", flush=True)
    return ver


def cookie_args(
    cookies_from_browser: str | None, cookies_file: Path | None
) -> list[str]:
    if cookies_file is not None:
        return ["--cookies", str(cookies_file)]
    if cookies_from_browser:
        return ["--cookies-from-browser", cookies_from_browser]
    return []


def _clean_na(x: str) -> str:
    if not x or x == "NA":
        return ""
    return x


def _int_field(x: str) -> int | None:
    if not x or x == "NA":
        return None
    try:
        return int(float(x))
    except ValueError:
        return None


def channel_key_from_url(url: str) -> str:
    """Stable key from a channel URL."""
    u = url.rstrip("/")
    m = re.search(r"youtube\.com/@([^/?#]+)", u, re.I)
    if m:
        return m.group(1).lower()
    m = re.search(r"youtube\.com/channel/([^/?#]+)", u, re.I)
    if m:
        return m.group(1)
    m = re.search(r"youtube\.com/user/([^/?#]+)", u, re.I)
    if m:
        return m.group(1).lower()
    m = re.search(r"youtube\.com/c/([^/?#]+)", u, re.I)
    if m:
        return m.group(1).lower()
    # fallback
    path = urlparse(u).path.strip("/").replace("/", "-")
    return path.lower() or "unknown"


def normalize_channel_url(url: str) -> str:
    u = url.rstrip("/")
    for suffix in ("/videos", "/streams", "/shorts", "/playlists", "/featured", "/about"):
        if u.endswith(suffix):
            u = u[: -len(suffix)]
            break
    return u


def channel_surfaces(base_url: str) -> list[str]:
    u = normalize_channel_url(base_url)
    return [f"{u}/videos", f"{u}/streams"]


def run_flat_enumerate(
    yt_dlp: str,
    urls: list[str],
    *,
    sleep_requests: float,
    cookie: list[str],
) -> list[dict[str, Any]]:
    cmd = [
        yt_dlp,
        "--flat-playlist",
        "--ignore-errors",
        "--no-warnings",
        "--extractor-args",
        "youtubetab:approximate_date",
        "--sleep-requests",
        str(sleep_requests),
        "--print",
        PRINT_SPEC,
        *cookie,
        *urls,
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0 and not proc.stdout.strip():
        err = (proc.stderr or "").strip() or f"exit {proc.returncode}"
        print(f"  warn: yt-dlp failed: {err[:500]}", file=sys.stderr, flush=True)
        return []

    rows: list[dict[str, Any]] = []
    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        while len(parts) < 10:
            parts.append("")
        (
            vid,
            url,
            title,
            channel,
            channel_url,
            upload_date,
            duration,
            view_count,
            playlist_title,
            description,
        ) = parts[:10]
        if not vid or vid == "NA" or len(vid) > 20:
            continue
        if not url or url == "NA":
            url = f"https://www.youtube.com/watch?v={vid}"
        rows.append(
            {
                "video_id": vid,
                "url": url,
                "title": _clean_na(title),
                "channel": _clean_na(channel),
                "channel_url": _clean_na(channel_url),
                "upload_date": _clean_na(upload_date),
                "duration_sec": _int_field(duration),
                "view_count": _int_field(view_count),
                "playlist_title": _clean_na(playlist_title),
                "description": _clean_na(description)[:2000],
            }
        )
    return rows


def enumerate_playlists(
    yt_dlp: str,
    channel_url: str,
    *,
    sleep_requests: float,
    cookie: list[str],
    max_playlists: int,
) -> list[tuple[str, list[dict[str, Any]]]]:
    base = normalize_channel_url(channel_url)
    playlists_url = f"{base}/playlists"

    cmd = [
        yt_dlp,
        "--flat-playlist",
        "--ignore-errors",
        "--no-warnings",
        "--sleep-requests",
        str(sleep_requests),
        "--print",
        "%(id)s\t%(url)s\t%(title)s",
        *cookie,
        playlists_url,
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0 and not proc.stdout.strip():
        return []

    playlists: list[tuple[str, str]] = []
    for line in proc.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        pid, purl = parts[0], parts[1]
        ptitle = parts[2] if len(parts) > 2 else ""
        if not purl or purl == "NA":
            if pid and pid != "NA":
                purl = f"https://www.youtube.com/playlist?list={pid}"
            else:
                continue
        playlists.append((_clean_na(ptitle), purl))

    # Prefer debate-ish playlist titles first
    def pl_score(item: tuple[str, str]) -> tuple[int, str]:
        t = item[0].lower()
        score = 0
        for kw in (
            "debate",
            "round",
            "final",
            "elim",
            "ndt",
            "ceda",
            "toc",
            "policy",
            "pf",
            "ld",
            "octa",
            "semi",
            "prelim",
        ):
            if kw in t:
                score -= 1
        return (score, t)

    playlists.sort(key=pl_score)
    playlists = playlists[:max_playlists]
    out: list[tuple[str, list[dict[str, Any]]]] = []
    for ptitle, purl in playlists:
        print(f"    playlist: {ptitle or purl}", flush=True)
        rows = run_flat_enumerate(
            yt_dlp, [purl], sleep_requests=sleep_requests, cookie=cookie
        )
        for r in rows:
            if ptitle and not r.get("playlist_title"):
                r["playlist_title"] = ptitle
        out.append((ptitle, rows))
    return out


def ingest_rows(
    store: Store,
    rows: list[dict[str, Any]],
    *,
    discovered_via: str,
    channel_fallback: str = "",
    channel_url_fallback: str = "",
) -> tuple[int, int]:
    kept = dropped = 0
    now = utc_now()
    for row in rows:
        row = dict(row)
        if not row.get("channel") and channel_fallback:
            row["channel"] = channel_fallback
        if not row.get("channel_url") and channel_url_fallback:
            row["channel_url"] = channel_url_fallback
        row["discovered_via"] = [discovered_via]
        clf = classify_row(row)
        row["is_round"] = clf.is_round
        row["round_label"] = clf.round_label
        row["format"] = clf.format
        row["confidence"] = clf.confidence
        row["dropped"] = clf.drop
        row["updated_at"] = now
        store.upsert_video(row, commit=False)
        if clf.drop:
            dropped += 1
        else:
            kept += 1
    store.commit()
    return kept, dropped


def channel_name_map(store: Store) -> dict[str, str]:
    m = {s["id"]: s["name"] for s in SEED_CHANNELS}
    for ch in store.list_channels():
        if ch.get("name"):
            m[ch["channel_key"]] = ch["name"]
    return m


PERMANENT_UNAVAILABLE = frozenset(
    {
        "private",
        "unavailable",
        "live_unavailable",
        "upcoming_live",
        "copyright",
        # Without cookies these aren't downloadable for us either
        "login_required",
    }
)


def reclassify_all(store: Store) -> dict[str, int]:
    now = utc_now()
    names = channel_name_map(store)
    rows = list(store.iter_videos())
    kept = dropped = 0
    for raw in rows:
        discovered = json.loads(raw["discovered_via"] or "[]")
        channel = raw["channel"] or ""
        if not channel:
            for d in discovered:
                if d.startswith("seed:") or d.startswith("resolved:") or d.startswith(
                    "search:"
                ):
                    key = d.split(":")[1]
                    channel = names.get(key, channel)
                    if channel:
                        break
        row = {
            "video_id": raw["video_id"],
            "url": raw["url"],
            "title": raw["title"] or "",
            "channel": channel,
            "channel_url": raw["channel_url"] or "",
            "playlist_title": raw["playlist_title"] or "",
            "upload_date": raw["upload_date"],
            "duration_sec": raw["duration_sec"],
            "view_count": raw["view_count"],
            "description": raw["description"] or "",
            "discovered_via": discovered,
            "updated_at": now,
        }
        clf = classify_row(row)
        row["is_round"] = clf.is_round
        row["round_label"] = clf.round_label
        row["format"] = clf.format
        row["confidence"] = clf.confidence
        # Preserve enrich permanent unavailability (don't resurrect dead links)
        avail = None
        try:
            avail = raw["availability"]
        except (KeyError, IndexError):
            avail = None
        if avail in PERMANENT_UNAVAILABLE:
            row["dropped"] = True
        else:
            row["dropped"] = clf.drop
        store.upsert_video(row, commit=False)
        if row["dropped"]:
            dropped += 1
        else:
            kept += 1
    store.commit()
    return {"reclassified": len(rows), "kept": kept, "dropped": dropped}


def harvest_channel(
    store: Store,
    yt_dlp: str,
    *,
    key: str,
    name: str,
    url: str,
    source_tag: str,
    refresh: bool,
    sleep_requests: float,
    cookie: list[str],
    max_playlists: int,
    do_videos: bool,
    do_playlists: bool,
) -> None:
    if do_videos and (refresh or not store.channel_videos_done(key)):
        print(f"enumerate videos+streams: {name} ({url})", flush=True)
        surfaces = channel_surfaces(url)
        rows = run_flat_enumerate(
            yt_dlp, surfaces, sleep_requests=sleep_requests, cookie=cookie
        )
        print(f"  videos+streams: {len(rows)}", flush=True)
        k, d = ingest_rows(
            store,
            rows,
            discovered_via=source_tag,
            channel_fallback=name,
            channel_url_fallback=url,
        )
        print(f"  ingest videos+streams: kept={k} dropped={d}", flush=True)
        store.mark_channel_videos_done(key, utc_now())
    elif do_videos:
        print(f"skip videos (done): {name}", flush=True)

    if do_playlists and max_playlists > 0 and (
        refresh or not store.channel_playlists_done(key)
    ):
        print(f"enumerate playlists: {name}", flush=True)
        pl = enumerate_playlists(
            yt_dlp,
            url,
            sleep_requests=sleep_requests,
            cookie=cookie,
            max_playlists=max_playlists,
        )
        for ptitle, prows in pl:
            tag = f"{source_tag}:playlist:{ptitle or 'unknown'}"
            k, d = ingest_rows(
                store,
                prows,
                discovered_via=tag,
                channel_fallback=name,
                channel_url_fallback=url,
            )
            print(f"  ingest playlist {ptitle!r}: kept={k} dropped={d}", flush=True)
        store.mark_channel_playlists_done(key)
    elif do_playlists and max_playlists > 0:
        print(f"skip playlists (done): {name}", flush=True)


def ensure_seed_channels(store: Store) -> None:
    for seed in SEED_CHANNELS:
        store.upsert_channel(
            seed["id"], seed["name"], seed["url"], source="seed"
        )


def resolve_name_channels(
    store: Store,
    yt_dlp: str,
    *,
    sleep_requests: float,
    cookie: list[str],
) -> int:
    """Resolve RESOLVE_NAMES to channel URLs via ytsearch; register pending channels."""
    added = 0
    known = {c["channel_key"] for c in store.list_channels()}
    for item in RESOLVE_NAMES:
        key = item["id"]
        if key in known:
            print(f"resolve skip (known): {key}", flush=True)
            continue

        if item.get("url"):
            store.upsert_channel(
                key, item.get("name") or key, item["url"], source="resolved"
            )
            print(f"resolve pinned: {key} → {item['url']}", flush=True)
            known.add(key)
            added += 1
            continue

        query = item["query"]
        expect = re.compile(item["expect"], re.I)
        print(f"resolve: {query}", flush=True)
        cmd = [
            yt_dlp,
            "--flat-playlist",
            "--ignore-errors",
            "--no-warnings",
            "--sleep-requests",
            str(sleep_requests),
            "--print",
            "%(channel)s\t%(channel_url)s\t%(channel_id)s\t%(title)s",
            *cookie,
            f"ytsearch10:{query}",
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        best: tuple[str, str] | None = None
        for line in proc.stdout.splitlines():
            parts = line.split("\t")
            while len(parts) < 4:
                parts.append("")
            ch_name, ch_url, ch_id, title = parts[:4]
            ch_name = _clean_na(ch_name)
            ch_url = _clean_na(ch_url)
            blob = f"{ch_name} {title} {ch_url}"
            if not ch_url or ch_url == "NA":
                if ch_id and ch_id != "NA":
                    ch_url = f"https://www.youtube.com/channel/{ch_id}"
                else:
                    continue
            # Require expect pattern on the *channel name* only.
            # Matching titles pulls news orgs that covered a tournament once.
            if ch_name and expect.search(ch_name):
                best = (ch_name, normalize_channel_url(ch_url))
                break

        if not best:
            print(f"  failed to resolve {key} (no expect match)", flush=True)
            continue
        name, url = best
        print(f"  resolved {key} → {name} {url}", flush=True)
        store.upsert_channel(key, name, url, source="resolved")
        known.add(key)
        added += 1
        time.sleep(0.5)
    return added


def discover_channels_from_search(
    store: Store,
    yt_dlp: str,
    *,
    sleep_requests: float,
    cookie: list[str],
    search_size: int,
    max_new: int | None = None,
) -> int:
    """Stage A: search queries → unique channel URLs."""
    known_urls = {
        normalize_channel_url(c["url"]).lower() for c in store.list_channels()
    }
    known_keys = {c["channel_key"] for c in store.list_channels()}
    added = 0

    for q in SEARCH_QUERIES:
        if max_new is not None and added >= max_new:
            break
        print(f"search: {q}", flush=True)
        cmd = [
            yt_dlp,
            "--flat-playlist",
            "--ignore-errors",
            "--no-warnings",
            "--sleep-requests",
            str(sleep_requests),
            "--print",
            "%(channel)s\t%(channel_url)s\t%(channel_id)s\t%(title)s",
            *cookie,
            f"ytsearch{search_size}:{q}",
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        seen_this_q: set[str] = set()
        for line in proc.stdout.splitlines():
            parts = line.split("\t")
            while len(parts) < 4:
                parts.append("")
            ch_name, ch_url, ch_id, _title = parts[:4]
            ch_name = _clean_na(ch_name)
            ch_url = _clean_na(ch_url)
            if not ch_url or ch_url == "NA":
                if ch_id and ch_id != "NA":
                    ch_url = f"https://www.youtube.com/channel/{ch_id}"
                else:
                    continue
            ch_url = normalize_channel_url(ch_url)
            if ch_url.lower() in known_urls or ch_url in seen_this_q:
                continue
            # Skip obvious non-debate mega / news / unrelated channels
            name_l = ch_name.lower()
            if any(bad in name_l for bad in CHANNEL_DENY_SUBSTRINGS):
                continue
            # Prefer debate-adjacent channel names for search discovery
            if not re.search(r"debate|ndt|ceda|forensic|speech", name_l):
                # Allow short personal uploaders only if query was format-specific
                # (still skip obvious junk names)
                if len(name_l) < 2:
                    continue
            key = channel_key_from_url(ch_url)
            if key in known_keys:
                continue
            # Dedupe against already-known channel IDs embedded in seed URLs
            if any(key in normalize_channel_url(u).lower() for u in known_urls):
                continue
            store.upsert_channel(
                key, ch_name or key, ch_url, source=f"search:{q[:40]}"
            )
            known_urls.add(ch_url.lower())
            known_keys.add(key)
            seen_this_q.add(ch_url)
            added += 1
            print(f"  + channel {ch_name or key} ({ch_url})", flush=True)
            if max_new is not None and added >= max_new:
                break
        time.sleep(0.8)
    return added


def harvest_all_channels(
    store: Store,
    yt_dlp: str,
    *,
    refresh: bool,
    sleep_requests: float,
    sleep_between_channels: float,
    cookie: list[str],
    max_playlists: int,
    do_videos: bool,
    do_playlists: bool,
    only_keys: set[str] | None = None,
) -> None:
    channels = store.list_channels()
    # Prefer seeds / resolved first
    def sort_key(c: dict[str, Any]) -> tuple[int, str]:
        src = c.get("source") or ""
        pri = 0 if src == "seed" else 1 if src == "resolved" else 2
        return (pri, c.get("name") or c["channel_key"])

    channels.sort(key=sort_key)
    for ch in channels:
        key = ch["channel_key"]
        if only_keys is not None and key not in only_keys:
            continue
        src = ch.get("source") or "seed"
        if src == "seed":
            tag = f"seed:{key}"
        elif src == "resolved":
            tag = f"resolved:{key}"
        else:
            tag = f"search:{key}"
        harvest_channel(
            store,
            yt_dlp,
            key=key,
            name=ch.get("name") or key,
            url=ch["url"],
            source_tag=tag,
            refresh=refresh,
            sleep_requests=sleep_requests,
            cookie=cookie,
            max_playlists=max_playlists,
            do_videos=do_videos,
            do_playlists=do_playlists,
        )
        if sleep_between_channels > 0:
            time.sleep(sleep_between_channels)


def classify_enrich_error(stderr: str) -> str:
    s = (stderr or "").lower()
    if "private video" in s:
        return "private"
    if "live stream recording is not available" in s or (
        "livestream" in s and "not available" in s
    ):
        return "live_unavailable"
    if "live event will begin" in s:
        return "upcoming_live"
    if (
        "video unavailable" in s
        or "no longer available" in s
        or "account associated" in s
        or "community guidelines" in s
        or "terms of service" in s
        or "has been removed" in s
    ):
        return "unavailable"
    if (
        "sign in to confirm" in s
        or "confirm you're not a bot" in s
        or "please sign in" in s
    ):
        return "login_required"
    if "copyright" in s:
        return "copyright"
    return "error"


def enrich_durations(
    store: Store,
    yt_dlp: str,
    *,
    sleep_requests: float,
    cookie: list[str],
    limit: int,
    batch_size: int,
) -> dict[str, Any]:
    """Pull duration/view_count/description via non-flat yt-dlp for missing rows.

    Permanently dead/private videos are marked and excluded from the export so
    the retained index is download-ready.
    """
    targets = store.videos_missing_duration(limit=limit)
    print(f"enrich: {len(targets)} videos missing duration (limit={limit})", flush=True)
    enriched = 0
    counts: dict[str, int] = {}

    # Prefer node JS runtime when present (yt-dlp YouTube extractor)
    js_args: list[str] = []
    if shutil.which("node") or shutil.which("nodejs"):
        js_args = ["--js-runtimes", "node"]

    for i in range(0, len(targets), batch_size):
        batch = targets[i : i + batch_size]
        ok = 0
        for t in batch:
            vid = t["video_id"]
            url = t["url"] or f"https://www.youtube.com/watch?v={vid}"
            cmd = [
                yt_dlp,
                "--skip-download",
                "--no-playlist",
                *js_args,
                "--sleep-requests",
                str(max(sleep_requests, 0.5)),
                "--print",
                ENRICH_PRINT,
                *cookie,
                url,
            ]
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
            line = (proc.stdout or "").strip().splitlines()
            if line and proc.returncode == 0:
                parts = line[0].split("\t")
                while len(parts) < 8:
                    parts.append("")
                (
                    out_id,
                    title,
                    channel,
                    channel_url,
                    upload_date,
                    duration,
                    view_count,
                    description,
                ) = parts[:8]
                if not out_id or out_id == "NA":
                    out_id = vid
                dur = _int_field(duration)
                store.patch_metadata(
                    out_id,
                    duration_sec=dur,
                    view_count=_int_field(view_count),
                    description=_clean_na(description)[:2000] or None,
                    upload_date=_clean_na(upload_date) or None,
                    title=_clean_na(title) or None,
                    channel=_clean_na(channel) or None,
                    channel_url=_clean_na(channel_url) or None,
                    availability="public",
                    enrich_error="",  # clear prior error
                    dropped=False,
                    commit=False,
                )
                ok += 1
                if dur is not None:
                    enriched += 1
                counts["public"] = counts.get("public", 0) + 1
            else:
                err = (proc.stderr or "").strip()
                kind = classify_enrich_error(err)
                # Permanent failures leave the index; soft errors stay retryable
                permanent = kind in PERMANENT_UNAVAILABLE
                short_err = err.splitlines()[-1][:240] if err else f"exit {proc.returncode}"
                store.patch_metadata(
                    vid,
                    availability=kind,
                    enrich_error=short_err,
                    dropped=True if permanent else None,
                    commit=False,
                )
                counts[kind] = counts.get(kind, 0) + 1

        store.commit()
        print(
            f"  batch {i // batch_size + 1}: ok={ok}/{len(batch)} running={counts}",
            flush=True,
        )
        time.sleep(0.2)

    # Reclassify after enrichment so duration signals apply to survivors
    rc = reclassify_all(store)
    return {
        "targets": len(targets),
        "enriched": enriched,
        "by_status": counts,
        **rc,
    }


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description="Index competitive debate round URLs from YouTube"
    )
    p.add_argument("--db", type=Path, default=DEFAULT_DB)
    p.add_argument("--jsonl", type=Path, default=DEFAULT_JSONL)
    p.add_argument("--refresh", action="store_true")
    p.add_argument("--no-update", action="store_true")
    p.add_argument("--sleep-requests", type=float, default=1.5)
    p.add_argument("--sleep-between-channels", type=float, default=2.0)
    p.add_argument("--cookies-from-browser", default=None)
    p.add_argument("--cookies", type=Path, default=None)
    p.add_argument("--max-playlists", type=int, default=30)
    p.add_argument("--skip-playlists", action="store_true")
    p.add_argument("--videos-only", action="store_true", help="Skip playlist walk")
    p.add_argument(
        "--playlists-only",
        action="store_true",
        help="Only walk playlists for known channels",
    )
    p.add_argument(
        "--resolve-names",
        action="store_true",
        help="Resolve RESOLVE_NAMES to channels",
    )
    p.add_argument(
        "--discover",
        action="store_true",
        help="Stage A search → new channels",
    )
    p.add_argument("--search-size", type=int, default=50, help="ytsearchN size")
    p.add_argument(
        "--max-discover",
        type=int,
        default=80,
        help="Cap new channels from search this run",
    )
    p.add_argument(
        "--enrich",
        action="store_true",
        help="Fill missing duration/metadata via yt-dlp",
    )
    p.add_argument("--enrich-limit", type=int, default=400)
    p.add_argument("--enrich-batch", type=int, default=15)
    p.add_argument("--export-only", action="store_true")
    p.add_argument("--reclassify", action="store_true")
    p.add_argument(
        "--full",
        action="store_true",
        help="resolve + discover + harvest videos/playlists + enrich",
    )
    p.add_argument(
        "--harvest",
        action="store_true",
        help="Harvest all known channels (default if no other mode flag)",
    )
    args = p.parse_args(argv)

    # Default action: harvest if nothing else specified
    mode_flags = [
        args.export_only,
        args.reclassify,
        args.resolve_names,
        args.discover,
        args.enrich,
        args.playlists_only,
        args.harvest,
        args.full,
    ]
    if not any(mode_flags):
        args.harvest = True
    if args.full:
        args.resolve_names = True
        args.discover = True
        args.harvest = True
        args.enrich = True

    store = Store(args.db)
    try:
        if args.export_only and not args.reclassify:
            n = store.export_jsonl(args.jsonl)
            print(json.dumps({"exported": n, "jsonl": str(args.jsonl), **store.stats()}, indent=2))
            return 0

        if args.reclassify and not any(
            [args.harvest, args.full, args.resolve_names, args.discover, args.enrich, args.playlists_only]
        ):
            print(json.dumps(reclassify_all(store)), flush=True)
            n = store.export_jsonl(args.jsonl)
            print(json.dumps({"exported": n, **store.stats()}, indent=2))
            return 0

        yt_dlp = find_yt_dlp()
        check_yt_dlp(yt_dlp, update=not args.no_update)
        cookie = cookie_args(args.cookies_from_browser, args.cookies)

        ensure_seed_channels(store)

        if args.resolve_names:
            n = resolve_name_channels(
                store,
                yt_dlp,
                sleep_requests=args.sleep_requests,
                cookie=cookie,
            )
            print(json.dumps({"resolved_new": n}), flush=True)

        if args.discover:
            n = discover_channels_from_search(
                store,
                yt_dlp,
                sleep_requests=args.sleep_requests,
                cookie=cookie,
                search_size=args.search_size,
                max_new=args.max_discover,
            )
            print(json.dumps({"discovered_new": n}), flush=True)

        do_playlists = not args.skip_playlists and not args.videos_only
        do_videos = not args.playlists_only
        if args.playlists_only:
            do_playlists = True
            do_videos = False

        if args.harvest or args.playlists_only or args.full:
            harvest_all_channels(
                store,
                yt_dlp,
                refresh=args.refresh,
                sleep_requests=args.sleep_requests,
                sleep_between_channels=args.sleep_between_channels,
                cookie=cookie,
                max_playlists=0 if not do_playlists else args.max_playlists,
                do_videos=do_videos,
                do_playlists=do_playlists,
            )

        if args.enrich:
            er = enrich_durations(
                store,
                yt_dlp,
                sleep_requests=args.sleep_requests,
                cookie=cookie,
                limit=args.enrich_limit,
                batch_size=args.enrich_batch,
            )
            print(json.dumps({"enrich": er}, indent=2), flush=True)
        elif args.reclassify or args.harvest or args.playlists_only:
            # Fresh classification after harvest
            print(json.dumps(reclassify_all(store)), flush=True)

        n = store.export_jsonl(args.jsonl)
        print(
            json.dumps(
                {
                    "exported": n,
                    "db": str(args.db),
                    "jsonl": str(args.jsonl),
                    **store.stats(),
                },
                indent=2,
            ),
            flush=True,
        )
    finally:
        store.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
