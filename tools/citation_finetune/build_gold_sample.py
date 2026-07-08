#!/usr/bin/env python3
"""Draw a NATURAL-distribution held-out gold sample for eval.

Unlike stratified_sample.py (which balances 300/stratum for training coverage),
this samples rows in their *real* corpus proportions so the resulting eval set
tells us production-realistic accuracy.  Rows already in the training pool
(stratified_sample.jsonl) are excluded via anti-join to prevent leakage.  Each
row is still tagged with primary_stratum (same CASE logic) for per-stratum
reporting.
"""

from __future__ import annotations

import argparse
import csv
import json
import shutil
import subprocess
from pathlib import Path

DEFAULT_DB = Path("/home/adam/datasets/OpenDebateEvidenceBM25DuckDB/opendebateevidence_good.duckdb")
DEFAULT_OUT_DIR = Path("datasets/citation_finetune")
DEFAULT_TRAIN_POOL = Path("datasets/citation_finetune/stratified_sample.jsonl")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Build a natural-distribution gold sample.")
    p.add_argument("--db", type=Path, default=DEFAULT_DB)
    p.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    p.add_argument("--train-pool", type=Path, default=DEFAULT_TRAIN_POOL)
    p.add_argument("--n", type=int, default=250, help="rows to draw")
    p.add_argument("--seed", type=int, default=90210)
    p.add_argument("--prefix", default="gold_sample")
    return p.parse_args()


def sample_sql(db_pool_json: str, csv_path: Path, n: int, seed: int) -> str:
    csv_target = str(csv_path.resolve()).replace("'", "''")
    pool = db_pool_json.replace("'", "''")
    return f"""
SELECT setseed({seed / 100000.0});
COPY (
WITH
base AS (
  SELECT id, fullcite,
    length(fullcite) AS char_len,
    length(fullcite) - length(replace(fullcite, chr(10), '')) AS newline_count
  FROM bm25_tables.documents
  WHERE fullcite IS NOT NULL AND length(trim(fullcite)) > 0
    AND id NOT IN (SELECT id FROM read_json_auto('{pool}'))
),
features AS (
  SELECT *,
    regexp_matches(fullcite, 'https?://|www\\.') AS has_url,
    regexp_matches(fullcite, '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}') AS has_year_or_date,
    regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b') AS has_early_short_year,
    regexp_matches(lower(fullcite), 'lexis|nexis') AS has_lexis,
    regexp_matches(lower(fullcite), 'jstor') AS has_jstor,
    regexp_matches(lower(fullcite), 'accessed|doa|retrieved') AS has_accessed,
    regexp_matches(lower(fullcite), '\\bet al\\.?') AS has_et_al,
    regexp_matches(lower(fullcite), 'doi[: ]|10\\.[0-9]{{4,9}}/') AS has_doi,
    regexp_matches(lower(fullcite), '\\bp\\.? ?[0-9]+|\\bpp\\.? ?[0-9]+|pages? [0-9]+') AS has_pages,
    regexp_matches(lower(fullcite), '\\bpublished\\b|publication date|posted|updated') AS has_published_signal,
    (regexp_matches(lower(fullcite), '\\bno date\\b')
       OR regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
       OR regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')) AS has_no_date_label,
    regexp_matches(fullcite, '(^|[^:])//[[:space:]]*[^[:space:]][^\\n]{{0,80}}$|[)\\]][[:space:]]*[A-Za-z][A-Za-z0-9_-]{{1,24}}[[:space:]]*(\\*[^\\n]*)?$') AS has_card_signature,
    regexp_matches(lower(fullcite), 'professor|ph\\.?d|j\\.?d\\.?|m\\.?a\\.?|b\\.?a\\.?|fellow|director|researcher|writer|editor|staff|candidate|attorney|university|college|school') AS has_author_qualifications,
    regexp_matches(fullcite, '[12][0-9]{{3}}.*[‘’'']\\d{{2}}|[‘’'']\\d{{2}}.*[12][0-9]{{3}}') AS has_multiple_date_signals,
    (NOT regexp_matches(fullcite, 'https?://|www\\.')
       AND NOT regexp_matches(fullcite, '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}')
       AND NOT regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b')
       AND NOT regexp_matches(lower(fullcite), '\\bno date\\b')
       AND NOT regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
       AND NOT regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')
       AND (regexp_matches(lower(fullcite), '^\\s*(at|a2|answers? to|overview|impact|link|internal link|solvency|turn|case|framework|fw|perm|cp|da|kritik|k)\\b')
            OR regexp_matches(fullcite, '^[A-Z][A-Za-z ]{{1,40}}:\\s')
            OR char_len < 12)) AS is_likely_reject
  FROM base
),
classified AS (
  SELECT *,
    CASE
      WHEN is_likely_reject THEN 'likely_reject'
      WHEN char_len < 20 THEN 'very_short'
      WHEN char_len < 50 THEN 'short'
      WHEN char_len > 4000 THEN 'very_long_spillover'
      WHEN char_len > 1000 THEN 'long'
      WHEN has_no_date_label THEN 'no_date_label'
      WHEN NOT has_year_or_date AND NOT has_early_short_year THEN 'no_year_or_date'
      WHEN has_multiple_date_signals THEN 'multiple_date_signals'
      WHEN has_card_signature THEN 'card_signature'
      WHEN has_doi THEN 'doi'
      WHEN has_pages THEN 'pages'
      WHEN has_published_signal THEN 'published_date'
      WHEN has_lexis THEN 'lexis'
      WHEN has_jstor THEN 'jstor'
      WHEN has_accessed THEN 'accessed_or_doa'
      WHEN has_et_al THEN 'et_al'
      WHEN has_author_qualifications THEN 'author_qualifications'
      WHEN has_url THEN 'url'
      ELSE 'ordinary'
    END AS primary_stratum
  FROM features
),
ranked AS (
  SELECT id, primary_stratum, char_len, is_likely_reject, fullcite,
    row_number() OVER (ORDER BY random()) AS rn
  FROM classified
)
SELECT id, primary_stratum, char_len, is_likely_reject, fullcite
FROM ranked WHERE rn <= {n} ORDER BY rn
) TO '{csv_target}' (HEADER, DELIMITER ',');
"""


def main() -> None:
    args = parse_args()
    if not shutil.which("duckdb"):
        raise SystemExit("duckdb CLI not found")
    if not args.db.exists():
        raise SystemExit(f"db not found: {args.db}")
    args.out_dir.mkdir(parents=True, exist_ok=True)
    csv_path = args.out_dir / f"{args.prefix}.csv"
    jsonl_path = args.out_dir / f"{args.prefix}.jsonl"

    sql = sample_sql(str(args.train_pool.resolve()), csv_path, args.n, args.seed)
    subprocess.run(["duckdb", "-readonly", str(args.db), "-c", sql], check=True)

    count = 0
    with csv_path.open(newline="", encoding="utf-8") as src, jsonl_path.open("w", encoding="utf-8") as dst:
        reader = csv.DictReader(src)
        for row in reader:
            row["char_len"] = int(row["char_len"])
            row["is_likely_reject"] = row["is_likely_reject"].lower() == "true"
            dst.write(json.dumps(row, ensure_ascii=False) + "\n")
            count += 1
    print(f"Wrote {count} gold rows -> {jsonl_path}")


if __name__ == "__main__":
    main()
