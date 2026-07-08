#!/usr/bin/env python3
"""Build a stratified citation-labeling sample from the ODE DuckDB export."""

from __future__ import annotations

import argparse
import csv
import json
import shutil
import subprocess
from pathlib import Path


DEFAULT_DB = Path(
    "/home/adam/datasets/OpenDebateEvidenceBM25DuckDB/opendebateevidence_good.duckdb"
)
DEFAULT_OUTPUT_DIR = Path("datasets/citation_finetune")
DEFAULT_PER_STRATUM = 300


STRATA = [
    "likely_reject",
    "very_short",
    "short",
    "very_long_spillover",
    "long",
    "no_year_or_date",
    "multiple_date_signals",
    "no_date_label",
    "card_signature",
    "doi",
    "pages",
    "published_date",
    "lexis",
    "jstor",
    "accessed_or_doa",
    "et_al",
    "author_qualifications",
    "url",
    "ordinary",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Create CSV/JSONL stratified samples for citation labeling."
    )
    parser.add_argument("--db", type=Path, default=DEFAULT_DB, help="DuckDB file path")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=DEFAULT_OUTPUT_DIR,
        help="Directory for generated artifacts",
    )
    parser.add_argument(
        "--per-stratum",
        type=int,
        default=DEFAULT_PER_STRATUM,
        help="Maximum rows to sample from each primary stratum",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=1729,
        help="Seed passed to DuckDB setseed for repeatable random ordering",
    )
    parser.add_argument(
        "--prefix",
        default="stratified_sample",
        help="Output file prefix",
    )
    parser.add_argument(
        "--weird-limit",
        type=int,
        default=25,
        help="Maximum examples per weird-case bucket",
    )
    return parser.parse_args()


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def sample_sql(csv_path: Path, per_stratum: int, seed: int) -> str:
    stratum_values = ", ".join(f"({sql_literal(name)})" for name in STRATA)
    csv_target = str(csv_path.resolve()).replace("'", "''")

    return f"""
SELECT setseed({seed / 10000.0});
COPY (
WITH
strata(name, rank) AS (
  SELECT name, row_number() OVER () FROM (VALUES {stratum_values}) AS v(name)
),
base AS (
  SELECT
    id,
    fullcite,
    length(fullcite) AS char_len,
    length(fullcite) - length(replace(fullcite, chr(10), '')) AS newline_count
  FROM bm25_tables.documents
),
features AS (
  SELECT
    *,
    regexp_matches(fullcite, 'https?://|www\\.') AS has_url,
    regexp_matches(
      fullcite,
      '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}'
    ) AS has_year_or_date,
    regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b') AS has_early_short_year,
    regexp_matches(lower(fullcite), 'lexis|nexis') AS has_lexis,
    regexp_matches(lower(fullcite), 'jstor') AS has_jstor,
    regexp_matches(lower(fullcite), 'accessed|doa|retrieved') AS has_accessed,
    regexp_matches(lower(fullcite), '\\bet al\\.?') AS has_et_al,
    regexp_matches(lower(fullcite), 'doi[: ]|10\\.[0-9]{{4,9}}/') AS has_doi,
    regexp_matches(
      lower(fullcite),
      '\\bp\\.? ?[0-9]+|\\bpp\\.? ?[0-9]+|pages? [0-9]+'
    ) AS has_pages,
    regexp_matches(lower(fullcite), '\\bpublished\\b|publication date|posted|updated') AS has_published_signal,
    (
      regexp_matches(lower(fullcite), '\\bno date\\b')
      OR regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
      OR regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')
    ) AS has_no_date_label,
    regexp_matches(
      fullcite,
      '(^|[^:])//[[:space:]]*[^[:space:]][^\\n]{{0,80}}$|[)\\]][[:space:]]*[A-Za-z][A-Za-z0-9_-]{{1,24}}[[:space:]]*(\\*[^\\n]*)?$'
    ) AS has_card_signature,
    regexp_matches(
      lower(fullcite),
      'professor|ph\\.?d|j\\.?d\\.?|m\\.?a\\.?|b\\.?a\\.?|fellow|director|researcher|writer|editor|staff|candidate|attorney|university|college|school'
    ) AS has_author_qualifications,
    regexp_matches(fullcite, '[12][0-9]{{3}}.*[‘’'']\\d{{2}}|[‘’'']\\d{{2}}.*[12][0-9]{{3}}') AS has_multiple_date_signals,
    (
      NOT regexp_matches(fullcite, 'https?://|www\\.')
      AND NOT regexp_matches(fullcite, '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}')
      AND NOT regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b')
      AND NOT regexp_matches(lower(fullcite), '\\bno date\\b')
      AND NOT regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
      AND NOT regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')
      AND (
        regexp_matches(lower(fullcite), '^\\s*(at|a2|answers? to|overview|impact|link|internal link|solvency|turn|case|framework|fw|perm|cp|da|kritik|k)\\b')
        OR regexp_matches(fullcite, '^[A-Z][A-Za-z ]{{1,40}}:\\s')
        OR char_len < 12
      )
    ) AS is_likely_reject
  FROM base
),
classified AS (
  SELECT
    *,
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
  SELECT
    c.*,
    row_number() OVER (PARTITION BY primary_stratum ORDER BY random()) AS sample_rank
  FROM classified c
)
SELECT
  id,
  primary_stratum,
  char_len,
  newline_count,
  has_url,
  has_year_or_date,
  has_early_short_year,
  has_lexis,
  has_jstor,
  has_accessed,
  has_et_al,
  has_doi,
  has_pages,
  has_published_signal,
  has_no_date_label,
  has_card_signature,
  has_author_qualifications,
  has_multiple_date_signals,
  is_likely_reject,
  fullcite,
  '' AS label_json,
  '' AS label_notes
FROM ranked
JOIN strata ON ranked.primary_stratum = strata.name
WHERE sample_rank <= {per_stratum}
ORDER BY strata.rank, sample_rank
) TO '{csv_target}' (HEADER, DELIMITER ',');
"""


def run_duckdb(db_path: Path, sql: str) -> None:
    if not shutil.which("duckdb"):
        raise SystemExit("duckdb CLI not found on PATH")
    subprocess.run(
        ["duckdb", "-readonly", str(db_path), "-c", sql],
        check=True,
    )


def weird_cases_sql(csv_path: Path, limit: int) -> str:
    csv_target = str(csv_path.resolve()).replace("'", "''")
    return f"""
COPY (
WITH base AS (
  SELECT
    id,
    fullcite,
    length(fullcite) AS char_len,
    length(fullcite) - length(replace(fullcite, chr(10), '')) AS newline_count
  FROM bm25_tables.documents
),
cases AS (
  SELECT
    'multiple_date_signals' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE regexp_matches(fullcite, '[12][0-9]{{3}}.*[‘’'']\\d{{2}}|[‘’'']\\d{{2}}.*[12][0-9]{{3}}')
  UNION ALL
  SELECT
    'possible_signature_or_real_source_suffix' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE regexp_matches(fullcite, '(^|[^:])//[[:space:]]*[^[:space:]][^\\n]{{0,80}}$|[)\\]][[:space:]]*[A-Za-z][A-Za-z0-9_-]{{1,24}}[[:space:]]*(\\*[^\\n]*)?$')
  UNION ALL
  SELECT
    'likely_reject' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE
    NOT regexp_matches(fullcite, 'https?://|www\\.')
    AND NOT regexp_matches(fullcite, '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}')
    AND NOT regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b')
    AND NOT regexp_matches(lower(fullcite), '\\bno date\\b')
    AND NOT regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
    AND NOT regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')
    AND (
      regexp_matches(lower(fullcite), '^\\s*(at|a2|answers? to|overview|impact|link|internal link|solvency|turn|case|framework|fw|perm|cp|da|kritik|k)\\b')
      OR regexp_matches(fullcite, '^[A-Z][A-Za-z ]{{1,40}}:\\s')
      OR char_len < 12
    )
  UNION ALL
  SELECT
    'legal_district_or_no_date_ambiguity' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE regexp_matches(fullcite, '\\b[ENSW]\\.?D\\.?[[:space:]]+(Cal|Tex|N\\.Y|Ill|Pa|Fla|Va|Ohio|Mich|Ga|Wash|Mo|La)\\.?\\b')
  UNION ALL
  SELECT
    'very_long_single_paragraph' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE char_len > 4000 AND newline_count <= 1
  UNION ALL
  SELECT
    'no_year_without_no_date_label' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE
    NOT regexp_matches(fullcite, '[12][0-9]{{3}}|[‘’'']\\d{{2}}|\\b2[kK]\\b|\\b\\d{{1,2}}[-/]\\d{{1,2}}[-/]\\d{{2,4}}')
    AND NOT regexp_matches(left(fullcite, 80), '\\b\\d{{1,2}}\\b')
    AND NOT regexp_matches(lower(fullcite), '\\bno date\\b')
    AND NOT regexp_matches(lower(fullcite), '(^|[^a-z])n\\.d\\.($|[^a-z])')
    AND NOT regexp_matches(fullcite, '\\bN\\.?D\\.?\\b')
  UNION ALL
  SELECT
    'multiple_url_or_doi_candidates' AS bucket,
    id,
    char_len,
    fullcite
  FROM base
  WHERE
    length(lower(fullcite)) - length(replace(lower(fullcite), 'http', '')) >= 8
    OR length(lower(fullcite)) - length(replace(lower(fullcite), 'doi', '')) >= 6
),
ranked AS (
  SELECT
    *,
    row_number() OVER (PARTITION BY bucket ORDER BY random()) AS bucket_rank
  FROM cases
)
SELECT bucket, id, char_len, fullcite
FROM ranked
WHERE bucket_rank <= {limit}
ORDER BY bucket, bucket_rank
) TO '{csv_target}' (HEADER, DELIMITER ',');
"""


def csv_to_jsonl(csv_path: Path, jsonl_path: Path) -> int:
    count = 0
    with csv_path.open(newline="", encoding="utf-8") as src, jsonl_path.open(
        "w", encoding="utf-8"
    ) as dst:
        reader = csv.DictReader(src)
        for row in reader:
            row["char_len"] = int(row["char_len"])
            row["newline_count"] = int(row["newline_count"])
            for key in [
                "has_url",
                "has_year_or_date",
                "has_early_short_year",
                "has_lexis",
                "has_jstor",
                "has_accessed",
                "has_et_al",
                "has_doi",
                "has_pages",
                "has_published_signal",
                "has_no_date_label",
                "has_card_signature",
                "has_author_qualifications",
                "has_multiple_date_signals",
                "is_likely_reject",
            ]:
                value = row[key].lower()
                if value not in {"true", "false"}:
                    raise SystemExit(f"Unexpected boolean value for {key}: {row[key]}")
                row[key] = value == "true"
            dst.write(json.dumps(row, ensure_ascii=False) + "\n")
            count += 1
    return count


def validate_outputs(csv_path: Path, row_count: int, per_stratum: int) -> dict[str, int]:
    counts: dict[str, int] = {name: 0 for name in STRATA}
    ids: set[str] = set()
    with csv_path.open(newline="", encoding="utf-8") as src:
        reader = csv.DictReader(src)
        required = {
            "id",
            "primary_stratum",
            "char_len",
            "fullcite",
            "label_json",
            "label_notes",
        }
        if not reader.fieldnames:
            raise SystemExit("Sample CSV has no header")
        missing = required - set(reader.fieldnames)
        if missing:
            raise SystemExit(f"Sample CSV missing required columns: {sorted(missing)}")
        for row in reader:
            stratum = row["primary_stratum"]
            if stratum not in counts:
                raise SystemExit(f"Unexpected stratum emitted: {stratum}")
            if not row["id"]:
                raise SystemExit("Sample CSV contains a row with an empty id")
            if row["id"] in ids:
                raise SystemExit(f"Sample CSV contains duplicate id: {row['id']}")
            if not row["fullcite"].strip():
                raise SystemExit(f"Sample CSV contains blank fullcite for id {row['id']}")
            ids.add(row["id"])
            counts[stratum] += 1

    if row_count != sum(counts.values()):
        raise SystemExit(
            f"CSV/JSONL row-count mismatch: jsonl={row_count}, csv={sum(counts.values())}"
        )

    weak = {name: count for name, count in counts.items() if count < per_stratum}
    if weak:
        raise SystemExit(f"Underfilled strata: {weak}")
    return counts


def main() -> None:
    args = parse_args()
    if not args.db.exists():
        raise SystemExit(f"DuckDB file not found: {args.db}")
    if args.per_stratum < 1:
        raise SystemExit("--per-stratum must be positive")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    csv_path = args.output_dir / f"{args.prefix}.csv"
    jsonl_path = args.output_dir / f"{args.prefix}.jsonl"
    manifest_path = args.output_dir / f"{args.prefix}_manifest.json"
    weird_cases_path = args.output_dir / "weird_cases.csv"

    run_duckdb(args.db, sample_sql(csv_path, args.per_stratum, args.seed))
    run_duckdb(args.db, weird_cases_sql(weird_cases_path, args.weird_limit))
    row_count = csv_to_jsonl(csv_path, jsonl_path)
    counts = validate_outputs(csv_path, row_count, args.per_stratum)

    manifest = {
        "source_db": str(args.db),
        "source_table": "bm25_tables.documents",
        "csv_path": str(csv_path),
        "jsonl_path": str(jsonl_path),
        "weird_cases_path": str(weird_cases_path),
        "row_count": row_count,
        "per_stratum": args.per_stratum,
        "seed": args.seed,
        "weird_limit": args.weird_limit,
        "strata": STRATA,
        "stratum_counts": counts,
    }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {row_count} rows")
    print(csv_path)
    print(jsonl_path)
    print(weird_cases_path)
    print(manifest_path)


if __name__ == "__main__":
    main()
