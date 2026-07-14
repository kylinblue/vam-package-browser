"""Pre-fill `hub_url_if_found` + `matchable_on_hub` in a labels CSV by
normalized-slug match against the cached `hub_resources` sitemap (45k+ entries).

For each not_found row in the CSV:
  - normalize(package_name) is compared against normalize(slug) for every
    cached hub resource. Normalization mirrors hub.rs `normalize_compare`:
    strip non-alphanumeric, lowercase. So
        "Standing_Pose(513-576)"  ->  "standingpose513576"
        "standing-pose-513-576"   ->  "standingpose513576"
        match.
  - On exactly one match: fill hub_url_if_found + matchable_on_hub=yes,
    append `[auto:slug-match]` to notes so the human knows to spot-check.
  - On multiple matches (common-word slugs like "jump", "test"): leave
    fields blank, append `[auto:ambiguous-N]` to notes so the row gets
    human attention.
  - On zero matches: leave row untouched.

Rows where the human has already filled matchable_on_hub or hub_url_if_found
are skipped entirely -- this is purely additive.

Read-only against the DB. Writes a `.bak` next to the target CSV before
overwriting (skip with --no-backup).
"""

import argparse
import csv
import os
import shutil
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path


def normalize(s: str) -> str:
    return "".join(c.lower() for c in s if c.isalnum())


def default_db_path() -> Path:
    return Path(os.environ["APPDATA"]) / "com.github.kylinblue.vam-package-browser" / "index.sqlite"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--csv",
        type=Path,
        required=True,
        help="Path to the labels CSV to pre-fill (read + write).",
    )
    p.add_argument("--db", type=Path, default=default_db_path())
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Print what would change without writing.",
    )
    p.add_argument(
        "--no-backup",
        action="store_true",
        help="Skip writing a .bak next to the target.",
    )
    return p.parse_args()


def build_slug_index(c: sqlite3.Connection) -> dict[str, list[tuple[int, str]]]:
    """norm_slug -> list of (resource_id, original_slug)."""
    idx: dict[str, list[tuple[int, str]]] = defaultdict(list)
    for resource_id, slug in c.execute("SELECT resource_id, slug FROM hub_resources"):
        idx[normalize(slug)].append((resource_id, slug))
    return idx


def build_resource_creator_index(c: sqlite3.Connection) -> dict[int, str]:
    """resource_id -> creator, drawn from already-matched local packages.

    Opportunistic: only covers resource_ids that have at least one
    successfully-matched local package. When present, lets us detect
    creator mismatches in slug-match candidates (e.g. local "Shapes.Pana"
    matching slug "pana" that belongs to some other author).
    """
    idx: dict[int, str] = {}
    for rid, creator in c.execute(
        """
        SELECT hub_resource_id, creator
          FROM packages
         WHERE hub_resource_id IS NOT NULL
           AND hub_sync_state='matched'
           AND creator IS NOT NULL
        """
    ):
        if rid not in idx:
            idx[rid] = creator
    return idx


def main() -> int:
    args = parse_args()

    if not args.csv.exists():
        print(f"error: csv not found at {args.csv}", file=sys.stderr)
        return 2
    if not args.db.exists():
        print(f"error: db not found at {args.db}", file=sys.stderr)
        return 2

    with args.csv.open("r", encoding="utf-8", newline="") as f:
        reader = csv.DictReader(f)
        fieldnames = list(reader.fieldnames or [])
        rows = list(reader)

    required = {"package_name", "hub_url_if_found", "matchable_on_hub", "notes"}
    missing = required - set(fieldnames)
    if missing:
        print(f"error: csv missing required columns: {sorted(missing)}", file=sys.stderr)
        return 2

    c = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    slug_idx = build_slug_index(c)
    creator_idx = build_resource_creator_index(c)
    print(f"loaded {sum(len(v) for v in slug_idx.values())} hub slugs across "
          f"{len(slug_idx)} normalized keys", file=sys.stderr)
    print(f"loaded {len(creator_idx)} resource->creator cross-refs from matched rows",
          file=sys.stderr)

    counts = {
        "skipped_already_labeled": 0,
        "filled_unique_match": 0,
        "noted_creator_mismatch": 0,
        "noted_ambiguous": 0,
        "no_match": 0,
    }

    for row in rows:
        already_labeled = bool(
            (row.get("matchable_on_hub") or "").strip()
            or (row.get("hub_url_if_found") or "").strip()
        )
        if already_labeled:
            counts["skipped_already_labeled"] += 1
            continue

        norm_pkg = normalize(row.get("package_name", ""))
        if not norm_pkg:
            counts["no_match"] += 1
            continue

        candidates = slug_idx.get(norm_pkg, [])
        if len(candidates) == 1:
            rid, slug = candidates[0]
            known_creator = creator_idx.get(rid)
            local_creator = row.get("creator", "")
            existing_notes = (row.get("notes") or "").strip()
            url = f"https://hub.virtamate.com/resources/{slug}.{rid}/"
            if (
                known_creator
                and normalize(known_creator) != normalize(local_creator)
            ):
                # We have a cross-ref saying this resource belongs to a
                # different creator. Don't fill -- flag for human review.
                tag = f"[auto:creator-mismatch slug={slug} owner={known_creator}]"
                row["notes"] = (
                    f"{existing_notes} {tag}".strip() if existing_notes else tag
                )
                counts["noted_creator_mismatch"] += 1
            else:
                row["hub_url_if_found"] = url
                row["matchable_on_hub"] = "yes"
                tag = (
                    "[auto:slug-match+creator-confirmed]"
                    if known_creator
                    else "[auto:slug-match]"
                )
                row["notes"] = (
                    f"{existing_notes} {tag}".strip() if existing_notes else tag
                )
                counts["filled_unique_match"] += 1
        elif len(candidates) > 1:
            existing_notes = (row.get("notes") or "").strip()
            tag = f"[auto:ambiguous-{len(candidates)}]"
            row["notes"] = f"{existing_notes} {tag}".strip() if existing_notes else tag
            counts["noted_ambiguous"] += 1
        else:
            counts["no_match"] += 1

    print("\nsummary:", file=sys.stderr)
    for k, v in counts.items():
        print(f"  {k:28s} {v}", file=sys.stderr)
    print(f"  total rows                   {len(rows)}", file=sys.stderr)

    if args.dry_run:
        print("\n--dry-run: not writing.", file=sys.stderr)
        # Preview the unique-match fills so the user can sanity-check
        print("\nunique-match fills (first 20):", file=sys.stderr)
        n = 0
        for row in rows:
            if "[auto:slug-match]" in (row.get("notes") or ""):
                print(
                    f"  {row['creator']:>20s}.{row['package_name']:30s} "
                    f"-> {row['hub_url_if_found']}",
                    file=sys.stderr,
                )
                n += 1
                if n >= 20:
                    break
        return 0

    if not args.no_backup:
        bak = args.csv.with_suffix(args.csv.suffix + ".bak")
        shutil.copy2(args.csv, bak)
        print(f"\nbackup written: {bak}", file=sys.stderr)

    with args.csv.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)
    print(f"wrote: {args.csv}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
