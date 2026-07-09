# Build Spec: Competitive Debate Round URL Indexer

## 1. Objective

Build a command-line pipeline that discovers and indexes a large corpus of **competitive debate rounds on YouTube** into a single deduplicated dataset of URLs plus metadata.

The index exists because the **value is the audio** (and video) of the rounds themselves. This pipeline does **not** download or rehost media — it produces a durable, queryable map of where public rounds live so they can be found, prioritized, and used later. Deliverable for this task: **links + metadata only**.

**Format priority**

| Priority | Formats | Role |
|----------|---------|------|
| Primary | Policy (CX) | Optimize discovery, recall, and export ranking for these |
| Secondary | Public Forum (PF), Lincoln-Douglas (LD), World Schools (WS) | Explicitly in scope; retain and label when found |
| Out of scope | Lectures, promos, awards, announcements, Q&A, camp ads, etc. | Not mission-critical; **do not keep** once confidently identified as non-rounds |

The pipeline must be fully non-interactive (agent / cron), resumable, and hard to interrupt by platform friction.

## 2. What counts as a "round"

A *round* is a recorded competitive debate — typically ~45–150 minutes, with identifiable sides/teams, often labeled by tournament and elim/prelim. Livestreamed tournament rounds count.

**In scope**

- Full rounds and multi-part rounds (split videos)
- Elimination rounds (finals, semis, quarters, octas/octos, doubles, triples, …)
- Preliminary rounds (R1–R8, etc.)
- Practice / lab debates from camps when they are actual rounds
- Formats: **policy (primary)**, **PF, LD, World Schools (secondary)**

**Out of scope (drop when confident)**

- Lectures, topic talks, theory dumps, “how to” content
- Camp promos, recruiting, trailers
- Awards ceremonies, announcements, Q&A panels
- Anything that is clearly not a competitive round

**Uncertainty rule:** when the classifier is unsure whether a video is a round, **keep and flag** (`is_round` null/unknown, low confidence). Prefer false positives in the store over silently losing rounds. Only drop on **high-confidence non-round**.

## 3. Core design principle

**Discover via search; harvest via channel enumeration.**

YouTube keyword search is lossy and (via the official Data API) quota-capped. Do **not** try to build the corpus by paginating search for videos.

1. Use search only to **find channels** that host rounds.
2. Use `yt-dlp` channel enumeration to pull **all** videos from each channel (no API quota).

Search is a **channel discovery** tool, not a **round inventory** tool. Never treat search result sets as the corpus. This inversion is why the project is tractable at scale.

## 4. Tech constraints

- **Primary tool: `yt-dlp`.** Keep it current (`yt-dlp -U`, or pin to a nightly). YouTube breaks extractors regularly; stale builds silently return partial results.
- Use `--flat-playlist` for enumeration (URLs/metadata without downloading media).
- Pass cookies for volume reliability: `--cookies-from-browser firefox` for interactive machines, or an exported `cookies.txt` for cron/agent runs (with a refresh path when sessions die).
- **YouTube Data API v3 is optional and used only for metadata enrichment**, never for discovery. `videos.list` costs 1 unit and accepts up to 50 IDs/call → ~25k videos/day of clean metadata in the free 10k-unit pool. Do **not** use `search.list` (100 units, separate ~100-call/day bucket, lossy).
- Language: open (Python or shell orchestrator both fine). Prefer a durable local store (**SQLite** recommended). Lives under `flowstate-autoflow` as a CLI pipeline; language choice can stay pragmatic until tighter product integration is needed.

## 5. Seed channel list

Treat as a **seed**, not the final set — §6 grows it automatically.

**Verified channel URLs**

- CEDA Debate — https://www.youtube.com/user/cedadebate
- Michigan Debate — https://www.youtube.com/channel/UC2gnLZUFVVhjTy4UL9MhvRg
- Georgetown Debate Seminar — https://www.youtube.com/channel/UCK_L-9TX6z4rMCeh8X-oTyA
- National Speech & Debate Association — https://www.youtube.com/c/NationalSpeechDebateAssociation
- Policy Debate Central — https://www.youtube.com/@PolicyDebateCentral

**Resolve programmatically** (e.g. `ytsearch` for the name → take channel of top match → verify name similarity before trusting):

- DDI Debate (Dartmouth)
- Emory National Debate Institute / ENDI
- Cal / Berkeley debate institute (CNDI)
- Wake Forest Debate
- Northwestern NHSI
- Baylor Debate
- UTNIF (University of Texas)
- Spartan Debate Institute (Michigan State / SDI)
- Gonzaga Debate Institute (GDI)
- Kentucky Debate (UK)
- Samford Debate
- Harvard Debate Council
- NAUDL and regional urban debate leagues
- Tournament channels: Tournament of Champions (TOC), Greenhill, Glenbrooks, Berkeley, Harvard, Emory

Seeds lean policy-heavy by design; Stage A queries should still surface PF/LD/WS-heavy channels so secondary formats are not systematically missed.

## 6. Pipeline stages

### Stage A — Channel discovery (search)

Run varied search queries and collect **channel URLs** from the hits (not the videos as the corpus). Dedupe channels. Merge with the seed list.

```bash
yt-dlp --flat-playlist --ignore-errors \
  --print "%(channel_url)s\t%(channel)s\t%(title)s" \
  "ytsearch500:policy debate round finals"
```

**Query battery** (rotate; each slice hits different channels):

- Policy: tournament names (NDT, CEDA, TOC, Greenhill, Glenbrooks, Berkeley, Emory, Wake Forest), elim names (`octafinals`, `quarterfinals`, `policy debate finals`), format terms (`CX debate round`, `policy debate aff neg`), camp names
- Secondary formats: `public forum finals`, `PF debate round`, `Lincoln Douglas finals`, `LD debate round`, `World Schools debate round`, `WSDC debate`

Also expand discovery from known channels using whatever is reliable in practice:

- Co-occurring channels in search hits (high signal, easy)
- Channel playlists that point outward
- Featured/related channels when extractable (support is uneven — do not hard-depend on it)

New channels feed Stage B. Iterate until the new-channel rate flattens (e.g. fewer than N new channels for two consecutive discovery iterations).

### Stage B — Channel enumeration (harvest)

For every channel, exhaustively enumerate **uploads, livestreams, and (optionally) shorts**. The `/streams` tab is critical — tournament rounds are often livestreams and disappear if you only walk `/videos`.

```bash
yt-dlp --flat-playlist --ignore-errors \
  --extractor-args "youtubetab:approximate_date" \
  --print "%(id)s\t%(webpage_url)s\t%(title)s\t%(channel)s\t%(channel_url)s\t%(upload_date)s\t%(duration)s" \
  "https://www.youtube.com/@HANDLE/videos" \
  "https://www.youtube.com/@HANDLE/streams"
```

Also enumerate the channel’s **playlists**. Playlist titles (e.g. `NDT 2023 Elims`, `TOC PF Finals`) are strong classification signal and useful discovery context — not an end-product taxonomy of tournament/year/teams.

Some fields (duration, exact upload date) may be missing or approximate in flat mode. Expected; enrich in Stage D if configured.

### Stage C — Dedup + store

Key every record on the **11-character YouTube video ID** (natural primary key). Upsert into SQLite. A video reachable from multiple channels/playlists appears once; record all discovery paths.

Do **not** use `--download-archive` for dedup here (it is tied to media downloads). Dedup in the local store.

### Stage D — Enrichment (optional, API)

For uniform metadata (exact duration, publish date, view count, description), batch IDs 50 at a time into Data API `videos.list` (`part=snippet,contentDetails,statistics`). 1 unit/call. Cache results; never re-fetch a known ID.

If no API key is configured, flat-playlist metadata is sufficient for an index that still points at the media.

### Stage E — Classification

Apply §7. For each retained record set:

- `is_round` — true / false / unknown
- `format` — `policy` | `pf` | `ld` | `world_schools` | `unknown`
- `round_label` — e.g. `elim/finals`, `prelim/r3`, `practice`, `unknown`
- `confidence` — [0, 1]

**Retention policy**

| Outcome | Action |
|---------|--------|
| High-confidence round (any in-scope format) | Keep |
| High-confidence non-round | Drop (or mark deleted and exclude from exports — do not invest further) |
| Uncertain | Keep with `is_round` unknown / low confidence |

Classification is not a reason to drop secondary formats. PF/LD/WS rounds stay; they rank below policy in default exports.

## 7. Classification heuristics

Start with a title/description/playlist/channel regex pass (case-insensitive).

**Positive signals (round-ish)**

- `\b(aff|neg)\b`, `\bvs?\.?\b`, `\bround\s?[1-8]\b`
- `\b(octas?|octos?|doubles|triples|quarters|quads|semis?|finals?|elims?)\b`
- Team-code patterns (two capitalized surnames, `SCHOOL AB` abbreviations)
- Tournament names (NDT, CEDA, TOC, Greenhill, Glenbrooks, Berkeley, Emory, Wake, …)

**Format signals**

| Format | Signals |
|--------|---------|
| Policy | `\b(CX|policy)\b`, classic 1AR/2NC-style titles when present, many camp/tournament policy channels |
| PF | `public forum`, `\bPF\b` |
| LD | `Lincoln.?Douglas`, `\bLD\b` |
| World Schools | `World Schools`, `\bWSDC\b`, `\bWS\b` (careful: `WS` alone is noisy) |

**Negative signals (non-round — downweight hard; drop only when confidence is high)**

- `lecture`, `seminar`, `how to`, `intro to`, `awards`, `announcement`, `promo`, `Q&A`, `trailer`, `recruiting`

Do **not** treat PF/LD/WS format markers as non-round signals. Those mark **format**, not rejection.

**Duration signal**

- Rounds are typically 45–150 min.
- Very short (&lt;20 min) is often not a full round; soft signal only (multi-part splits exist).

Regex covers most volume; titles are noisy. Optional second stage: cheap LLM over `title + description + channel + playlist_title` for records near the confidence threshold. Gate it so spend only hits ambiguous rows.

**Default export ranking:** policy rounds first (by confidence / views / recency as needed), then PF/LD/WS. Full store still holds all retained formats.

## 8. Operational reliability (stay under the radar)

This project indexes **public URLs and metadata only**. No media download or rehost in this pipeline.

Goal is not legal theater — it is **not getting the crawl burned**:

- Rate-limit: `--sleep-requests 1.5` and a small `--sleep-interval` between channels. Avoid aggressive parallelism; one flagged IP poisons the run.
- Prefer cookies (browser or `cookies.txt`) at volume so “sign in to confirm” walls do not truncate enumeration.
- `--ignore-errors` so one dead channel does not halt the job.
- Resumable state: re-runs skip already-enumerated channels/videos unless `--refresh`.
- Keep the footprint metadata-only and sequential enough to finish large harvests without tripwires.

## 9. Output schema

Emit newline-delimited JSON (`rounds.jsonl`) backed by SQLite. One record per video ID retained after classification:

```json
{
  "video_id": "abc123XYZ_0",
  "url": "https://www.youtube.com/watch?v=abc123XYZ_0",
  "title": "NDT 2023 Finals - Michigan KM vs Kentucky ...",
  "channel": "CEDA Debate",
  "channel_url": "https://www.youtube.com/user/cedadebate",
  "playlist_title": "NDT 2023 Elims",
  "discovered_via": ["seed:cedadebate", "search:policy debate finals"],
  "upload_date": "20230326",
  "duration_sec": 5412,
  "view_count": 1843,
  "is_round": true,
  "round_label": "elim/finals",
  "format": "policy",
  "confidence": 0.94
}
```

`format` is one of: `policy`, `pf`, `ld`, `world_schools`, `unknown`.

Optional filtered exports (e.g. policy-only JSONL) are fine; SQLite remains the source of truth for everything retained.

Structured tournament / year / team parsing is **not** a goal of this index. Playlist/title text stays raw for classification and human triage; the product is a reliable URL map to round media.

## 10. Acceptance criteria

- [ ] Runs end-to-end non-interactively and is **resumable** (re-run skips already-enumerated channels/videos unless `--refresh`).
- [ ] Every channel is walked across **both** `/videos` and `/streams`; playlists enumerated for classification context.
- [ ] Dedup is on the 11-char video ID; no duplicate records.
- [ ] `yt-dlp` version is checked/updated at start; cookies wired in for non-interactive runs.
- [ ] No use of `search.list`. If the Data API is configured, only `videos.list` enrichment; if not, flat-playlist metadata is enough.
- [ ] Output is valid JSONL + queryable SQLite matching the schema.
- [ ] Discovery is iterative: Stage A channels are enumerated and expansion re-feeds discovery until the new-channel rate flattens.
- [ ] Primary success: high recall of **policy** rounds in the retained set.
- [ ] Secondary success: PF/LD/WS rounds that appear in harvests are **kept and labeled**, not discarded as non-rounds.
- [ ] High-confidence lectures/promos/awards are **not** retained as first-class index rows.
- [ ] Ambiguous candidates are retained-and-flagged, not dropped.
- [ ] Spot-check sample (e.g. 100 records) shows reasonable precision on `is_round` and `format`; false-negative rounds are not systematically deleted.

## 11. Stretch goals

- Periodic re-crawl (weekly) that only fetches deltas via per-channel most-recent-upload watermark.
- Multi-part round detection (same round split across videos) and grouping by ID sets.
- Policy-first ranked export views (and secondary-format exports) without re-crawling.
- Downstream (out of this spec): use the index to pull audio for preservation / analysis — still not rehosting video as a public mirror unless a later plan says so.
