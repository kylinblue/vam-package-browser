"""Stratified sample of `hub_sync_state = 'not_found'` packages for hand-labeling.

This is the §1 / §3 first-move of [TODO-hub-sync.md].
Produces a CSV with one row per sampled package, with empty labeling
columns ready for human annotation. Read-only against the shared
index.sqlite -- safe to run from any worktree without taking the DB lock.

Stratification (default n=50):

  long-tail oversample:
    +2 each from predicted_hub_category in (Lighting + HDRI, Poses, Morphs)
    -- §2 of the TODO frames rare-category hub matches as
       disproportionately high-value (each match grows classifier training
       data for an under-sampled class).

  primary axis: predicted_method x predicted_confidence bucket
    kind-vote, high (>= 0.8) : 18  (dominant cell; spread across categories)
    kind-vote, mid (0.6-0.8) : 6
    kind-vote, low (< 0.6)   : take all (~2)
    graph-prop, any          : 8
    embed-knn, any           : 6   (k-NN is the long-tail-friendly method)
    NULL method              : take all (~3)

  within each (method, conf_bucket) cell, rows are picked round-robin
  across predicted_hub_category via ROW_NUMBER PARTITION BY category so
  Looks/Clothing don't drown out smaller categories.

Reproducibility: SQLite RANDOM() is unseeded, so each run picks a fresh
sample. The committed CSV at `labels/hub-sync/notfound-sample-<date>.csv`
is the canonical artifact -- treat re-runs as drawing a new sample.

Output CSV columns:

  Sampled-from-DB (do not edit):
    id, var_path, var_filename, creator, package_name, version,
    package_type, predicted_hub_category, predicted_method,
    predicted_confidence, hub_author, hub_synced_at

  Labeling fields (fill in by hand):
    matchable_on_hub          Y / N / ?         is this package actually on the hub?
    miss_mode                 one of:
                                creator-alias       (local creator != hub author handle)
                                package-rename      (package_name differs from hub title)
                                abbreviated-title   (hub title is a shortened/expanded form)
                                umbrella-pack       (hub resource bundles N local packages)
                                paid-offsite        (paid resource with no hub-hosted file)
                                hub-search-tail     (matchable but buried in XF search ranking)
                                gate                (page returned the age gate)
                                other
                                N-A                 (not matchable; matchable_on_hub = N)
    predicted_category_correct  Y / N / ?       is predicted_hub_category right?
    actual_hub_category         if N above, what should it be (or blank if unmatchable)
    hub_url_if_found            full URL for follow-up matcher work
    notes                       free text
"""

import argparse
import csv
import os
import sqlite3
import sys
from datetime import date
from pathlib import Path

LABEL_COLUMNS = [
    "matchable_on_hub",
    "miss_mode",
    "predicted_category_correct",
    "actual_hub_category",
    "hub_url_if_found",
    "notes",
]

DB_COLUMNS = [
    "id",
    "var_path",
    "var_filename",
    "creator",
    "package_name",
    "version",
    "package_type",
    "predicted_hub_category",
    "predicted_method",
    "predicted_confidence",
    "hub_author",
    "hub_synced_at",
]


def default_db_path() -> Path:
    return Path(os.environ["APPDATA"]) / "com.github.kylinblue.vam-package-browser" / "index.sqlite"


def default_out_path() -> Path:
    return (
        Path(__file__).resolve().parent.parent
        / "labels"
        / "hub-sync"
        / f"notfound-sample-{date.today().isoformat()}.csv"
    )


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--db", type=Path, default=default_db_path())
    p.add_argument("--out", type=Path, default=default_out_path())
    p.add_argument(
        "--force",
        action="store_true",
        help="Overwrite the output CSV if it already exists.",
    )
    return p.parse_args()


def confidence_bucket_sql(col: str) -> str:
    """SQL expression bucketing a confidence column into 'high'/'mid'/'low'/'null'."""
    return f"""CASE
        WHEN {col} IS NULL THEN 'null'
        WHEN {col} >= 0.8 THEN 'high'
        WHEN {col} >= 0.6 THEN 'mid'
        ELSE 'low'
    END"""


# Dedupe predicate: 208 of 1494 not_found rows are duplicate installs / versions
# of the same package. Hub-match is family-invariant (TODO §4), so labeling one
# row per (creator, package_name) is sufficient. Pick the lowest id deterministically.
DEDUPE_FILTER = """
    id IN (
        SELECT MIN(id) FROM packages
         WHERE hub_sync_state='not_found' AND error IS NULL AND is_hidden=0
         GROUP BY creator, package_name
    )
"""


CELL_TARGETS = [
    # (method, bucket, target)  -- bucket=None means "any bucket"
    ("kind-vote", "high", 18),
    ("kind-vote", "mid", 6),
    ("kind-vote", "low", 999),  # take all (~2)
    ("graph-prop", None, 8),
    ("embed-knn", None, 6),
    (None, None, 999),  # NULL predicted_method: take all (~3)
]

LONG_TAIL_CATEGORIES = ["Lighting + HDRI", "Poses", "Morphs"]
LONG_TAIL_PER_CATEGORY = 2


def collect_long_tail_ids(c: sqlite3.Connection) -> list[int]:
    """Force-include up to LONG_TAIL_PER_CATEGORY rows per long-tail category.

    Drawn from anywhere in the not_found pool regardless of method/confidence;
    each match in these categories is high-leverage per TODO §2.
    """
    sql = f"""
        WITH ranked AS (
            SELECT id,
                   ROW_NUMBER() OVER (
                     PARTITION BY predicted_hub_category
                     ORDER BY RANDOM()
                   ) AS rn
              FROM packages
             WHERE hub_sync_state='not_found'
               AND error IS NULL
               AND is_hidden=0
               AND predicted_hub_category = ?
               AND {DEDUPE_FILTER}
        )
        SELECT id FROM ranked WHERE rn <= ?
    """
    ids: list[int] = []
    for cat in LONG_TAIL_CATEGORIES:
        rows = c.execute(sql, (cat, LONG_TAIL_PER_CATEGORY)).fetchall()
        ids.extend(r[0] for r in rows)
    return ids


def collect_cell(
    c: sqlite3.Connection,
    method: str | None,
    bucket: str | None,
    target: int,
    exclude_ids: set[int],
) -> list[int]:
    """Pick up to `target` ids from the (method, bucket) cell.

    Within the cell, rows are round-robined across predicted_hub_category
    via ROW_NUMBER PARTITION BY category so dominant categories (Looks,
    Clothing) don't squeeze out smaller ones.
    """
    where = [
        "hub_sync_state='not_found'",
        "error IS NULL",
        "is_hidden=0",
        DEDUPE_FILTER,
    ]
    params: list = []
    if method is None:
        where.append("predicted_method IS NULL")
    else:
        where.append("predicted_method = ?")
        params.append(method)
    if bucket is not None:
        where.append(f"{confidence_bucket_sql('predicted_confidence')} = ?")
        params.append(bucket)
    if exclude_ids:
        placeholders = ",".join("?" * len(exclude_ids))
        where.append(f"id NOT IN ({placeholders})")
        params.extend(exclude_ids)

    sql = f"""
        WITH ranked AS (
            SELECT id,
                   ROW_NUMBER() OVER (
                     PARTITION BY COALESCE(predicted_hub_category,'__null__')
                     ORDER BY RANDOM()
                   ) AS rn_in_cat
              FROM packages
             WHERE {' AND '.join(where)}
        )
        SELECT id FROM ranked
         ORDER BY rn_in_cat, RANDOM()
         LIMIT ?
    """
    params.append(target)
    return [r[0] for r in c.execute(sql, params).fetchall()]


def fetch_rows(c: sqlite3.Connection, ids: list[int]) -> list[dict]:
    placeholders = ",".join("?" * len(ids))
    rows = c.execute(
        f"""
        SELECT id, var_path, creator, package_name, version,
               package_type, predicted_hub_category, predicted_method,
               predicted_confidence, hub_author, hub_synced_at
          FROM packages
         WHERE id IN ({placeholders})
        """,
        ids,
    ).fetchall()
    out = []
    for r in rows:
        (
            pid,
            var_path,
            creator,
            package_name,
            version,
            package_type,
            pred_cat,
            pred_method,
            pred_conf,
            hub_author,
            hub_synced_at,
        ) = r
        var_filename = Path(var_path).name if var_path else ""
        out.append(
            {
                "id": pid,
                "var_path": var_path,
                "var_filename": var_filename,
                "creator": creator,
                "package_name": package_name,
                "version": version,
                "package_type": package_type,
                "predicted_hub_category": pred_cat,
                "predicted_method": pred_method,
                "predicted_confidence": pred_conf,
                "hub_author": hub_author,
                "hub_synced_at": hub_synced_at,
            }
        )
    return out


def main() -> int:
    args = parse_args()

    if not args.db.exists():
        print(f"error: db not found at {args.db}", file=sys.stderr)
        return 2
    if args.out.exists() and not args.force:
        print(
            f"error: {args.out} already exists; pass --force to overwrite",
            file=sys.stderr,
        )
        return 2

    c = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)

    seen: set[int] = set()
    selected_ids: list[int] = []

    # Pass 0: long-tail category oversample
    long_tail = collect_long_tail_ids(c)
    for pid in long_tail:
        if pid not in seen:
            seen.add(pid)
            selected_ids.append(pid)

    # Pass 1..N: stratified cells
    for method, bucket, target in CELL_TARGETS:
        cell_ids = collect_cell(c, method, bucket, target, seen)
        for pid in cell_ids:
            if pid not in seen:
                seen.add(pid)
                selected_ids.append(pid)

    rows = fetch_rows(c, selected_ids)
    # Restore the long-tail + cell ordering for the CSV (helpful for review).
    by_id = {r["id"]: r for r in rows}
    ordered = [by_id[pid] for pid in selected_ids if pid in by_id]

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8", newline="") as f:
        w = csv.writer(f)
        w.writerow(DB_COLUMNS + LABEL_COLUMNS)
        for r in ordered:
            w.writerow([r[k] for k in DB_COLUMNS] + [""] * len(LABEL_COLUMNS))

    # Stratification report to stderr
    print(f"db:      {args.db}", file=sys.stderr)
    print(f"out:     {args.out}", file=sys.stderr)
    print(f"sampled: {len(ordered)} rows", file=sys.stderr)

    print("\nby (predicted_method, confidence bucket):", file=sys.stderr)
    by_cell: dict[tuple, int] = {}
    for r in ordered:
        b = (
            r["predicted_method"] or "NULL",
            (
                "high"
                if (r["predicted_confidence"] or -1) >= 0.8
                else "mid"
                if (r["predicted_confidence"] or -1) >= 0.6
                else "low"
                if (r["predicted_confidence"] or -1) >= 0
                else "null"
            ),
        )
        by_cell[b] = by_cell.get(b, 0) + 1
    for (m, b), n in sorted(by_cell.items(), key=lambda x: -x[1]):
        print(f"  {m:12s} {b:5s}  {n}", file=sys.stderr)

    print("\nby predicted_hub_category:", file=sys.stderr)
    by_cat: dict[str, int] = {}
    for r in ordered:
        cat = r["predicted_hub_category"] or "NULL"
        by_cat[cat] = by_cat.get(cat, 0) + 1
    for cat, n in sorted(by_cat.items(), key=lambda x: -x[1]):
        print(f"  {cat:30s} {n}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
