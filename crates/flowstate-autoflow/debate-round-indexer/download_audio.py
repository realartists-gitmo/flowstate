#!/usr/bin/env python3
"""Download bestaudio for policy rounds from the URL index.

Stores native YouTube audio (usually Opus/webm or m4a) — no re-encode.
Resumable via yt-dlp download archive + existing files.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_DB = HERE.parent / "rounds.sqlite"
DEFAULT_STASH = HERE.parent / "audio-stash" / "policy"
DEFAULT_ARCHIVE = DEFAULT_STASH / ".yt-dlp-archive.txt"
DEFAULT_MANIFEST = DEFAULT_STASH / "manifest.jsonl"
DEFAULT_LOG = DEFAULT_STASH / "download.log"


def find_yt_dlp() -> str:
    path = shutil.which("yt-dlp")
    if not path:
        raise SystemExit("yt-dlp not on PATH")
    return path


def policy_rounds(db: Path) -> list[dict]:
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT video_id, url, title, channel, duration_sec, confidence, format
        FROM videos
        WHERE dropped = 0
          AND is_round = 1
          AND format = 'policy'
          AND duration_sec IS NOT NULL
          AND duration_sec > 0
        ORDER BY confidence DESC, duration_sec DESC
        """
    ).fetchall()
    conn.close()
    return [dict(r) for r in rows]


def already_have(stash: Path, video_id: str) -> Path | None:
    """Return existing media path if present (any common audio/video-audio ext)."""
    for ext in (
        ".opus",
        ".webm",
        ".m4a",
        ".mp3",
        ".ogg",
        ".wav",
        ".flac",
        ".mp4",
        ".mkv",
    ):
        p = stash / f"{video_id}{ext}"
        if p.exists() and p.stat().st_size > 0:
            return p
    # yt-dlp sometimes uses title-ish names if template fails — scan prefix
    for p in stash.glob(f"{video_id}.*"):
        if p.is_file() and p.stat().st_size > 0 and p.suffix not in {".part", ".ytdl"}:
            return p
    return None


def download_one(
    yt_dlp: str,
    *,
    video_id: str,
    url: str,
    stash: Path,
    archive: Path,
    sleep_requests: float,
    cookie: list[str],
) -> tuple[bool, str]:
    out_tmpl = str(stash / f"{video_id}.%(ext)s")
    js_args: list[str] = []
    if shutil.which("node") or shutil.which("nodejs"):
        js_args = ["--js-runtimes", "node"]

    cmd = [
        yt_dlp,
        "--ignore-errors",
        "--no-playlist",
        "--no-overwrites",
        "--continue",
        *js_args,
        "-f",
        "ba/bestaudio/best",
        # Keep native stream; no forced re-encode
        "--download-archive",
        str(archive),
        "--sleep-requests",
        str(sleep_requests),
        "-o",
        out_tmpl,
        "--print",
        "after_move:%(filepath)s",
        *cookie,
        url or f"https://www.youtube.com/watch?v={video_id}",
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    path = already_have(stash, video_id)
    if path:
        return True, str(path)
    err = (proc.stderr or proc.stdout or "").strip()
    short = err.splitlines()[-1][:300] if err else f"exit {proc.returncode}"
    return False, short


def cookie_args(
    cookies_from_browser: str | None, cookies_file: Path | None
) -> list[str]:
    if cookies_file is not None:
        return ["--cookies", str(cookies_file)]
    if cookies_from_browser:
        return ["--cookies-from-browser", cookies_from_browser]
    return []


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Download bestaudio for policy rounds")
    p.add_argument("--db", type=Path, default=DEFAULT_DB)
    p.add_argument("--stash", type=Path, default=DEFAULT_STASH)
    p.add_argument("--archive", type=Path, default=None)
    p.add_argument("--limit", type=int, default=0, help="Max downloads this run (0=all)")
    p.add_argument("--sleep-requests", type=float, default=1.0)
    p.add_argument("--sleep-between", type=float, default=1.5)
    p.add_argument("--cookies-from-browser", default=None)
    p.add_argument("--cookies", type=Path, default=None)
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="List targets only",
    )
    args = p.parse_args(argv)

    stash: Path = args.stash
    stash.mkdir(parents=True, exist_ok=True)
    archive = args.archive or (stash / ".yt-dlp-archive.txt")
    manifest = stash / "manifest.jsonl"
    log_path = stash / "download.log"

    rounds = policy_rounds(args.db)
    print(f"policy rounds in index: {len(rounds)}", flush=True)
    print(f"stash: {stash}", flush=True)

    if args.dry_run:
        for r in rounds[:20]:
            print(f"  {r['video_id']}  {r['duration_sec']}s  {(r['title'] or '')[:70]}")
        if len(rounds) > 20:
            print(f"  ... +{len(rounds)-20} more")
        return 0

    yt_dlp = find_yt_dlp()
    cookie = cookie_args(args.cookies_from_browser, args.cookies)

    ok = skip = fail = 0
    targets = rounds if not args.limit else rounds[: args.limit]

    with log_path.open("a", encoding="utf-8") as log, manifest.open(
        "a", encoding="utf-8"
    ) as man:
        for i, r in enumerate(targets, 1):
            vid = r["video_id"]
            existing = already_have(stash, vid)
            if existing:
                skip += 1
                if i % 50 == 0 or i == 1:
                    print(f"[{i}/{len(targets)}] skip have {vid}", flush=True)
                continue

            print(
                f"[{i}/{len(targets)}] download {vid} "
                f"({(r['duration_sec'] or 0)/60:.0f}m) {(r['title'] or '')[:50]}",
                flush=True,
            )
            success, detail = download_one(
                yt_dlp,
                video_id=vid,
                url=r["url"] or "",
                stash=stash,
                archive=archive,
                sleep_requests=args.sleep_requests,
                cookie=cookie,
            )
            rec = {
                "video_id": vid,
                "url": r["url"],
                "title": r["title"],
                "channel": r["channel"],
                "duration_sec": r["duration_sec"],
                "ok": success,
                "path": detail if success else None,
                "error": None if success else detail,
            }
            man.write(json.dumps(rec, ensure_ascii=False) + "\n")
            man.flush()
            log.write(json.dumps(rec, ensure_ascii=False) + "\n")
            log.flush()

            if success:
                ok += 1
                print(f"  ok → {detail}", flush=True)
            else:
                fail += 1
                print(f"  FAIL {detail}", flush=True)

            if args.sleep_between > 0:
                time.sleep(args.sleep_between)

    # Disk usage summary
    total_bytes = sum(f.stat().st_size for f in stash.iterdir() if f.is_file())
    print(
        json.dumps(
            {
                "downloaded_ok": ok,
                "skipped_existing": skip,
                "failed": fail,
                "stash_bytes": total_bytes,
                "stash_gb": round(total_bytes / (1024**3), 2),
                "stash": str(stash),
            },
            indent=2,
        ),
        flush=True,
    )
    return 0 if fail == 0 or ok + skip > 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
