"""SQLite store for debate round URL index."""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any, Iterable

SCHEMA = """
CREATE TABLE IF NOT EXISTS channels (
    channel_key TEXT PRIMARY KEY,
    name TEXT,
    url TEXT NOT NULL,
    last_enumerated_at TEXT,
    status TEXT DEFAULT 'pending',
    playlists_status TEXT DEFAULT 'pending',
    source TEXT DEFAULT 'seed'
);

CREATE TABLE IF NOT EXISTS videos (
    video_id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    title TEXT,
    channel TEXT,
    channel_url TEXT,
    playlist_title TEXT,
    upload_date TEXT,
    duration_sec INTEGER,
    view_count INTEGER,
    description TEXT,
    discovered_via TEXT NOT NULL DEFAULT '[]',
    is_round INTEGER,
    round_label TEXT,
    format TEXT,
    confidence REAL,
    dropped INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_videos_format ON videos(format);
CREATE INDEX IF NOT EXISTS idx_videos_is_round ON videos(is_round);
CREATE INDEX IF NOT EXISTS idx_videos_dropped ON videos(dropped);
CREATE INDEX IF NOT EXISTS idx_videos_duration ON videos(duration_sec);
"""


class Store:
    def __init__(self, path: Path) -> None:
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(path)
        self.conn.row_factory = sqlite3.Row
        self.conn.executescript(SCHEMA)
        self._migrate()
        self.conn.commit()

    def _migrate(self) -> None:
        ch_cols = {r[1] for r in self.conn.execute("PRAGMA table_info(channels)")}
        if "playlists_status" not in ch_cols:
            self.conn.execute(
                "ALTER TABLE channels ADD COLUMN playlists_status TEXT DEFAULT 'pending'"
            )
        if "source" not in ch_cols:
            self.conn.execute(
                "ALTER TABLE channels ADD COLUMN source TEXT DEFAULT 'seed'"
            )
        v_cols = {r[1] for r in self.conn.execute("PRAGMA table_info(videos)")}
        if "availability" not in v_cols:
            self.conn.execute(
                "ALTER TABLE videos ADD COLUMN availability TEXT DEFAULT NULL"
            )
        if "enrich_error" not in v_cols:
            self.conn.execute(
                "ALTER TABLE videos ADD COLUMN enrich_error TEXT DEFAULT NULL"
            )

    def close(self) -> None:
        self.conn.close()

    def upsert_channel(
        self,
        channel_key: str,
        name: str,
        url: str,
        *,
        status: str | None = None,
        source: str = "seed",
    ) -> None:
        existing = self.conn.execute(
            "SELECT status, playlists_status FROM channels WHERE channel_key = ?",
            (channel_key,),
        ).fetchone()
        if existing:
            self.conn.execute(
                """
                UPDATE channels SET name = ?, url = ?,
                    source = CASE WHEN source = 'seed' THEN source ELSE ? END
                WHERE channel_key = ?
                """,
                (name, url, source, channel_key),
            )
        else:
            self.conn.execute(
                """
                INSERT INTO channels (channel_key, name, url, status, playlists_status, source)
                VALUES (?, ?, ?, ?, 'pending', ?)
                """,
                (channel_key, name, url, status or "pending", source),
            )
        self.conn.commit()

    def mark_channel_videos_done(self, channel_key: str, when_iso: str) -> None:
        self.conn.execute(
            """
            UPDATE channels
            SET status = 'done', last_enumerated_at = ?
            WHERE channel_key = ?
            """,
            (when_iso, channel_key),
        )
        self.conn.commit()

    def mark_channel_playlists_done(self, channel_key: str) -> None:
        self.conn.execute(
            """
            UPDATE channels SET playlists_status = 'done' WHERE channel_key = ?
            """,
            (channel_key,),
        )
        self.conn.commit()

    def channel_videos_done(self, channel_key: str) -> bool:
        row = self.conn.execute(
            "SELECT status FROM channels WHERE channel_key = ?", (channel_key,)
        ).fetchone()
        return bool(row and row["status"] == "done")

    def channel_playlists_done(self, channel_key: str) -> bool:
        row = self.conn.execute(
            "SELECT playlists_status FROM channels WHERE channel_key = ?",
            (channel_key,),
        ).fetchone()
        return bool(row and row["playlists_status"] == "done")

    def list_channels(self) -> list[dict[str, Any]]:
        rows = self.conn.execute(
            "SELECT channel_key, name, url, status, playlists_status, source FROM channels"
        ).fetchall()
        return [dict(r) for r in rows]

    def upsert_video(self, record: dict[str, Any], *, commit: bool = True) -> None:
        video_id = record["video_id"]
        existing = self.conn.execute(
            "SELECT discovered_via, playlist_title, duration_sec, view_count, description "
            "FROM videos WHERE video_id = ?",
            (video_id,),
        ).fetchone()

        discovered: list[str] = list(record.get("discovered_via") or [])
        playlist = record.get("playlist_title") or ""
        duration = record.get("duration_sec")
        view_count = record.get("view_count")
        description = record.get("description")

        if existing:
            prev = json.loads(existing["discovered_via"] or "[]")
            for d in discovered:
                if d not in prev:
                    prev.append(d)
            discovered = prev
            if not playlist and existing["playlist_title"]:
                playlist = existing["playlist_title"]
            if duration is None and existing["duration_sec"] is not None:
                duration = existing["duration_sec"]
            if view_count is None and existing["view_count"] is not None:
                view_count = existing["view_count"]
            if not description and existing["description"]:
                description = existing["description"]

        self.conn.execute(
            """
            INSERT INTO videos (
                video_id, url, title, channel, channel_url, playlist_title,
                upload_date, duration_sec, view_count, description,
                discovered_via, is_round, round_label, format, confidence,
                dropped, updated_at
            ) VALUES (
                ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?,
                ?, ?, ?, ?, ?,
                ?, ?
            )
            ON CONFLICT(video_id) DO UPDATE SET
                url = excluded.url,
                title = COALESCE(excluded.title, videos.title),
                channel = COALESCE(NULLIF(excluded.channel, ''), videos.channel),
                channel_url = COALESCE(NULLIF(excluded.channel_url, ''), videos.channel_url),
                playlist_title = COALESCE(NULLIF(excluded.playlist_title, ''), videos.playlist_title),
                upload_date = COALESCE(excluded.upload_date, videos.upload_date),
                duration_sec = COALESCE(excluded.duration_sec, videos.duration_sec),
                view_count = COALESCE(excluded.view_count, videos.view_count),
                description = COALESCE(NULLIF(excluded.description, ''), videos.description),
                discovered_via = excluded.discovered_via,
                is_round = excluded.is_round,
                round_label = excluded.round_label,
                format = excluded.format,
                confidence = excluded.confidence,
                dropped = excluded.dropped,
                updated_at = excluded.updated_at
            """,
            (
                video_id,
                record["url"],
                record.get("title"),
                record.get("channel"),
                record.get("channel_url"),
                playlist,
                record.get("upload_date"),
                duration,
                view_count,
                description,
                json.dumps(discovered),
                _bool_to_int(record.get("is_round")),
                record.get("round_label"),
                record.get("format"),
                record.get("confidence"),
                1 if record.get("dropped") else 0,
                record["updated_at"],
            ),
        )
        if commit:
            self.conn.commit()

    def commit(self) -> None:
        self.conn.commit()

    def iter_videos(self) -> Iterable[sqlite3.Row]:
        yield from self.conn.execute("SELECT * FROM videos")

    def videos_missing_duration(self, *, limit: int | None = None) -> list[dict[str, Any]]:
        """Candidates for enrichment: retained rows still needing a duration check."""
        q = """
            SELECT video_id, url, title, channel, channel_url, playlist_title,
                   upload_date, duration_sec, view_count, description,
                   discovered_via, is_round, round_label, format, confidence, dropped,
                   availability, enrich_error
            FROM videos
            WHERE duration_sec IS NULL
              AND dropped = 0
              AND (availability IS NULL OR availability = 'error')
            ORDER BY
                CASE WHEN is_round = 1 THEN 0 WHEN is_round IS NULL THEN 1 ELSE 2 END,
                confidence DESC
        """
        if limit is not None:
            q += f" LIMIT {int(limit)}"
        return [dict(r) for r in self.conn.execute(q)]

    def patch_metadata(
        self,
        video_id: str,
        *,
        duration_sec: int | None = None,
        view_count: int | None = None,
        description: str | None = None,
        upload_date: str | None = None,
        title: str | None = None,
        channel: str | None = None,
        channel_url: str | None = None,
        availability: str | None = None,
        enrich_error: str | None = None,
        dropped: bool | None = None,
        commit: bool = True,
    ) -> None:
        self.conn.execute(
            """
            UPDATE videos SET
                duration_sec = COALESCE(?, duration_sec),
                view_count = COALESCE(?, view_count),
                description = COALESCE(NULLIF(?, ''), description),
                upload_date = COALESCE(NULLIF(?, ''), upload_date),
                title = COALESCE(NULLIF(?, ''), title),
                channel = COALESCE(NULLIF(?, ''), channel),
                channel_url = COALESCE(NULLIF(?, ''), channel_url),
                availability = COALESCE(?, availability),
                enrich_error = CASE
                    WHEN ? = 1 THEN NULLIF(?, '')
                    ELSE enrich_error
                END,
                dropped = CASE WHEN ? IS NULL THEN dropped ELSE ? END
            WHERE video_id = ?
            """,
            (
                duration_sec,
                view_count,
                description or "",
                upload_date or "",
                title or "",
                channel or "",
                channel_url or "",
                availability,
                1 if enrich_error is not None else 0,
                enrich_error if enrich_error is not None else "",
                None if dropped is None else (1 if dropped else 0),
                None if dropped is None else (1 if dropped else 0),
                video_id,
            ),
        )
        if commit:
            self.conn.commit()

    def iter_retained(self) -> Iterable[dict[str, Any]]:
        rows = self.conn.execute(
            """
            SELECT * FROM videos
            WHERE dropped = 0
            ORDER BY
                CASE format
                    WHEN 'policy' THEN 0
                    WHEN 'pf' THEN 1
                    WHEN 'ld' THEN 2
                    WHEN 'world_schools' THEN 3
                    ELSE 4
                END,
                confidence DESC,
                upload_date DESC
            """
        )
        for row in rows:
            yield _row_to_export(row)

    def stats(self) -> dict[str, Any]:
        total = self.conn.execute("SELECT COUNT(*) AS c FROM videos").fetchone()["c"]
        retained = self.conn.execute(
            "SELECT COUNT(*) AS c FROM videos WHERE dropped = 0"
        ).fetchone()["c"]
        dropped = self.conn.execute(
            "SELECT COUNT(*) AS c FROM videos WHERE dropped = 1"
        ).fetchone()["c"]
        rounds = self.conn.execute(
            "SELECT COUNT(*) AS c FROM videos WHERE dropped = 0 AND is_round = 1"
        ).fetchone()["c"]
        with_dur = self.conn.execute(
            "SELECT COUNT(*) AS c FROM videos WHERE duration_sec IS NOT NULL AND dropped = 0"
        ).fetchone()["c"]
        need_enrich = self.conn.execute(
            """
            SELECT COUNT(*) AS c FROM videos
            WHERE duration_sec IS NULL AND dropped = 0
              AND (availability IS NULL OR availability = 'error')
            """
        ).fetchone()["c"]
        channels = self.conn.execute("SELECT COUNT(*) AS c FROM channels").fetchone()["c"]
        by_fmt = {
            r["format"] or "null": r["c"]
            for r in self.conn.execute(
                """
                SELECT format, COUNT(*) AS c FROM videos
                WHERE dropped = 0 AND is_round = 1
                GROUP BY format
                """
            )
        }
        by_avail = {
            (r["availability"] or "unknown"): r["c"]
            for r in self.conn.execute(
                """
                SELECT availability, COUNT(*) AS c FROM videos
                GROUP BY availability
                """
            )
        }
        return {
            "total_seen": total,
            "retained": retained,
            "dropped": dropped,
            "rounds": rounds,
            "retained_with_duration": with_dur,
            "need_enrich": need_enrich,
            "channels": channels,
            **{f"rounds_{k}": v for k, v in by_fmt.items()},
            **{f"avail_{k}": v for k, v in by_avail.items()},
        }

    def export_jsonl(self, path: Path) -> int:
        path.parent.mkdir(parents=True, exist_ok=True)
        n = 0
        with path.open("w", encoding="utf-8") as f:
            for rec in self.iter_retained():
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
                n += 1
        return n


def _bool_to_int(v: bool | None) -> int | None:
    if v is None:
        return None
    return 1 if v else 0


def _row_to_export(row: sqlite3.Row) -> dict[str, Any]:
    is_round_raw = row["is_round"]
    if is_round_raw is None:
        is_round: bool | None = None
    else:
        is_round = bool(is_round_raw)
    return {
        "video_id": row["video_id"],
        "url": row["url"],
        "title": row["title"],
        "channel": row["channel"],
        "channel_url": row["channel_url"],
        "playlist_title": row["playlist_title"],
        "discovered_via": json.loads(row["discovered_via"] or "[]"),
        "upload_date": row["upload_date"],
        "duration_sec": row["duration_sec"],
        "view_count": row["view_count"],
        "is_round": is_round,
        "round_label": row["round_label"],
        "format": row["format"],
        "confidence": row["confidence"],
        "availability": row["availability"] if "availability" in row.keys() else None,
    }
