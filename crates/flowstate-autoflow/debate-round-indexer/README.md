# Debate round URL indexer

Python pipeline that builds a deduplicated index of competitive debate round
URLs (policy primary; PF / LD / World Schools secondary) from public YouTube
channels. Metadata only — no media download.

Spec: [`../policy-debate-round-indexer-spec.md`](../policy-debate-round-indexer-spec.md)

## Requirements

- Python 3.10+
- [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) on `PATH`  
  `uv tool install yt-dlp`

Optional: browser cookies or `cookies.txt` for more reliable volume crawls.

## Outputs (next to the spec)

| File | Role |
|------|------|
| `../rounds.sqlite` | Source of truth |
| `../rounds.jsonl` | Export of retained rows |

## Usage

```bash
# Full pipeline: resolve name seeds → search discovery → harvest → enrich
python3 indexer.py --full --no-update

# Pieces
python3 indexer.py --resolve-names --no-update
python3 indexer.py --discover --no-update --max-discover 40
python3 indexer.py --harvest --no-update                 # videos+streams for pending
python3 indexer.py --playlists-only --no-update          # playlists for known channels
python3 indexer.py --enrich --no-update --enrich-limit 400
python3 indexer.py --reclassify
python3 indexer.py --export-only

# Cookies (recommended at volume)
python3 indexer.py --full --cookies-from-browser firefox
python3 indexer.py --full --cookies /path/to/cookies.txt
```

Default with no flags is `--harvest` (enumerate known channels).

## Audio stash (policy bestaudio)

Downloads **native** YouTube bestaudio (usually Opus in `.webm`) — no re-encode.

```bash
# All policy rounds → ../audio-stash/policy/
python3 download_audio.py

# Resume anytime (skips existing + yt-dlp archive)
python3 download_audio.py

# Smoke test
python3 download_audio.py --limit 3
```

Outputs:

| Path | Role |
|------|------|
| `../audio-stash/policy/<video_id>.webm` (or `.m4a`) | Audio files |
| `../audio-stash/policy/.yt-dlp-archive.txt` | Resume archive |
| `../audio-stash/policy/manifest.jsonl` | Per-file success/fail log |

## Layout

- `indexer.py` — CLI: discover, resolve, harvest, enrich, export
- `download_audio.py` — policy bestaudio → stash
- `classify.py` — round / format heuristics
- `store.py` — SQLite schema + export
- `seeds.py` — verified seeds, resolve names, search queries
