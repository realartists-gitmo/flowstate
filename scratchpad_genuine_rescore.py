#!/usr/bin/env python3
"""Score a replay JSONL against the 118 cleaned genuine-silent workbook rows."""

import json
import re
import sys
import xml.etree.ElementTree as ET
import zipfile
from collections import Counter


def fold(value: str) -> str:
    return re.sub(r"[^a-z0-9]", "", value.lower())


def workbook_rows(path: str) -> list[dict[str, str]]:
    namespace = {"x": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    with zipfile.ZipFile(path) as archive:
        root = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
    rows = []
    for row in root.findall(".//x:sheetData/x:row", namespace)[1:]:
        values = {}
        for cell in row.findall("x:c", namespace):
            column = re.match(r"[A-Z]+", cell.attrib["r"]).group()
            text = cell.find("x:is/x:t", namespace)
            values[column] = "" if text is None else text.text or ""
        rows.append(values)
    return rows


def correct_set(value: str) -> set[str]:
    if value.strip().lower() in {"", "(none)"}:
        return set()
    return {fold(part) for part in value.split(",") if fold(part)}


def main() -> None:
    scored_path = sys.argv[1]
    workbook_path = sys.argv[2] if len(sys.argv) > 2 else "citation_genuine_silents.xlsx"
    scored = {}
    with open(scored_path, encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                row = json.loads(line)
                scored[row["id"]] = row

    results = []
    for clean in workbook_rows(workbook_path):
        row = scored.get(clean["F"])
        if row is None:
            raise SystemExit(f"missing scored id {clean['F']}")
        expected = correct_set(clean["D"])
        actual = {fold(value) for value in row.get("model_surnames", []) if fold(value)}
        results.append((clean, expected, actual))

    matched = sum(expected == actual for _, expected, actual in results)
    categories = Counter(clean["E"] for clean, expected, actual in results if expected != actual)
    print(f"genuine rows: {len(results)}  exact: {matched}  remaining: {len(results) - matched}")
    print("remaining by category:", dict(categories))
    for clean, expected, actual in results:
        if expected == actual:
            continue
        print(
            f"[{clean['F']}] {clean['E']} "
            f"miss={sorted(expected - actual)} extra={sorted(actual - expected)}"
        )


if __name__ == "__main__":
    main()
