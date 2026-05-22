"""Turn a hand-labeled hub-sync CSV into the two deliverables:

  - §1 miss-mode histogram (which matcher fixes are highest leverage?)
  - §3 classifier production-population accuracy (do not_found rows agree
    with the classifier's predicted_hub_category?)

The CSV does not have a `miss_mode` column filled by the human (notes
carry the rationale instead), so this script auto-infers a coarse
miss_mode bucket per row from the structured fields. Rules:

  1. Notes contains `[auto:slug-match]` + URL present
                  -> matcher-bug-slug
     The offline slug-match pre-fill found this URL, meaning the live
     matcher's score_match should have caught it but didn't. These are
     the highest-leverage fixes (a normalize-and-compare change to
     score_match probably resolves all of them).

  2. URL present, notes lacks slug-match tag
                  -> matcher-bug-other if normalize(pkg)==normalize(slug-stem)
                     wrong-section if hub_url contains "/threads/"
                     package_rename otherwise
     "package_rename" is the structurally-different case: hub title is a
     marketing-copy expansion (Creatives_LUTs -> creatives-luts-for-
     exotic-visual-experiences) or a reordering (Tree_Static -> static-
     tree). Needs a different matcher strategy (search-by-creator-page
     scan, or title-similarity scoring).

  3. URL present, hub_author looks distinct from local creator
                  -> creator-alias
     Examples: WeebU -> weebuvr.24, OOOO -> vam-oooo.106128.

  4. No URL, actual_hub_category == "Removed"   -> removed-from-hub
  5. No URL, matchable_on_hub == "no"           -> not-on-hub
  6. No URL, actual_hub_category filled         -> inferred-not-on-hub
                                                   (user labeled by memory)
  7. No URL, everything else blank              -> unknown

Multiple buckets can apply (e.g. matcher-bug + creator-alias). We pick
the *primary structural cause* — the bucket whose fix would have caught
the row. matcher-bug-slug wins if it applies; otherwise package_rename
or creator-alias.

Classifier accuracy is straightforward: for each row with both
predicted_hub_category and actual_hub_category filled (and actual not
== "Removed"), tally pred == actual. Note this only includes rows where
the human could confirm a category (with or without URL); rows with
blank actual_hub_category are excluded as "no ground truth".
"""

import argparse
import csv
import re
from collections import defaultdict
from pathlib import Path


def normalize(s: str) -> str:
    return "".join(c.lower() for c in s if c.isalnum())


def slug_from_url(url: str) -> str | None:
    """Extract the slug stem from a hub URL.
    e.g. https://hub.virtamate.com/resources/standing-pose-513-576.17983/
         -> "standing-pose-513-576"
    """
    m = re.search(r"/(?:resources|threads)/([a-z0-9\-]+?)(?:\.\d+)?/?$", url.strip())
    return m.group(1) if m else None


def classify_miss_mode(row: dict) -> tuple[str, list[str]]:
    """Return (primary_miss_mode, list_of_secondary_buckets)."""
    notes = row.get("notes", "") or ""
    url = (row.get("hub_url_if_found") or "").strip()
    actual = (row.get("actual_hub_category") or "").strip()
    matchable = (row.get("matchable_on_hub") or "").strip().lower()
    creator = row.get("creator", "") or ""
    hub_author = (row.get("hub_author") or "").strip()
    pkg = row.get("package_name", "") or ""

    secondary: list[str] = []

    # Detect creator-alias regardless of primary bucket
    if hub_author:
        # hub_author often looks like "slug.NNN" -- strip the trailing .digits
        ha_clean = re.sub(r"\.\d+$", "", hub_author)
        if normalize(ha_clean) != normalize(creator):
            secondary.append("creator-alias")

    if url:
        if "/threads/" in url:
            return "wrong-section", secondary
        if "[auto:slug-match]" in notes:
            return "matcher-bug-slug", secondary
        slug = slug_from_url(url)
        if slug and normalize(pkg) == normalize(slug):
            return "matcher-bug-other", secondary
        # URL present but pkg and slug don't normalize-match -> rename
        if secondary == ["creator-alias"]:
            # If the only structural delta is the author handle, call it
            # creator-alias primary; otherwise package_rename
            slug_n = normalize(slug or "")
            pkg_n = normalize(pkg)
            if slug_n and (slug_n in pkg_n or pkg_n in slug_n):
                return "creator-alias", []
        return "package_rename", secondary

    # No URL
    if actual.lower() == "removed":
        return "removed-from-hub", secondary
    if matchable == "no":
        return "not-on-hub", secondary
    if actual:
        # Human inferred a category but couldn't find a URL -- either
        # removed before the sitemap snapshot or genuinely never on hub.
        if "removed" in notes.lower() or "deleted" in notes.lower():
            return "removed-from-hub", secondary
        return "inferred-not-on-hub", secondary
    return "unknown", secondary


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--csv",
        type=Path,
        default=Path("labels/hub-sync/notfound-sample-2026-05-19.csv"),
    )
    p.add_argument("--show-rows", action="store_true",
                   help="Print the inferred miss_mode for every row.")
    args = p.parse_args()

    # Spreadsheet apps often save CSV as cp1252 on Windows. Try utf-8 first,
    # fall back to cp1252 if a stray smart-quote / ellipsis trips the decoder.
    raw = args.csv.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode("cp1252")
    rows = list(csv.DictReader(text.splitlines()))

    # 1) miss-mode histogram
    miss_hist: dict[str, int] = defaultdict(int)
    secondary_hist: dict[str, int] = defaultdict(int)
    per_row: list[tuple[dict, str, list[str]]] = []
    for row in rows:
        primary, secondary = classify_miss_mode(row)
        miss_hist[primary] += 1
        for s in secondary:
            secondary_hist[s] += 1
        per_row.append((row, primary, secondary))

    print("=" * 70)
    print("§1 MISS-MODE HISTOGRAM")
    print("=" * 70)
    print("\nPrimary miss mode (structural cause):")
    for mode, n in sorted(miss_hist.items(), key=lambda x: -x[1]):
        pct = 100.0 * n / len(rows)
        print(f"  {mode:25s} {n:3d}  ({pct:5.1f}%)")
    print(f"  {'TOTAL':25s} {len(rows):3d}")

    if secondary_hist:
        print("\nSecondary modes co-occurring:")
        for mode, n in sorted(secondary_hist.items(), key=lambda x: -x[1]):
            print(f"  {mode:25s} {n:3d}")

    # 2) classifier accuracy on production population
    print("\n" + "=" * 70)
    print("§3 CLASSIFIER PRODUCTION-POPULATION ACCURACY")
    print("=" * 70)

    overall_total = 0
    overall_correct = 0
    by_method: dict[str, dict[str, int]] = defaultdict(lambda: {"n": 0, "ok": 0})
    by_pred_cat: dict[str, dict[str, int]] = defaultdict(lambda: {"n": 0, "ok": 0})
    by_conf_bucket: dict[str, dict[str, int]] = defaultdict(lambda: {"n": 0, "ok": 0})
    confusions: list[tuple[str, str, str]] = []
    for row in rows:
        pred = (row.get("predicted_hub_category") or "").strip()
        actual = (row.get("actual_hub_category") or "").strip()
        if not pred or not actual or actual.lower() == "removed":
            continue
        overall_total += 1
        is_correct = pred == actual
        if is_correct:
            overall_correct += 1
        else:
            confusions.append((row.get("var_filename", ""), pred, actual))
        method = (row.get("predicted_method") or "NULL").strip()
        by_method[method]["n"] += 1
        by_method[method]["ok"] += int(is_correct)
        by_pred_cat[pred]["n"] += 1
        by_pred_cat[pred]["ok"] += int(is_correct)
        try:
            c = float(row.get("predicted_confidence") or "nan")
            if c >= 0.8:
                bucket = ">=0.8"
            elif c >= 0.6:
                bucket = "0.6-0.8"
            elif c >= 0:
                bucket = "<0.6"
            else:
                bucket = "NULL"
        except ValueError:
            bucket = "NULL"
        by_conf_bucket[bucket]["n"] += 1
        by_conf_bucket[bucket]["ok"] += int(is_correct)

    if overall_total:
        print(
            f"\nOverall: {overall_correct}/{overall_total} = "
            f"{100.0 * overall_correct / overall_total:.1f}%"
        )
    else:
        print("\nNo rows with both predicted and actual category filled.")

    def print_breakdown(title: str, d: dict[str, dict[str, int]]) -> None:
        print(f"\n{title}:")
        for key, v in sorted(d.items(), key=lambda x: -x[1]["n"]):
            n, ok = v["n"], v["ok"]
            pct = (100.0 * ok / n) if n else 0.0
            print(f"  {key:20s} {ok:2d}/{n:2d}  ({pct:5.1f}%)")

    print_breakdown("By predicted_method", by_method)
    print_breakdown("By predicted_confidence bucket", by_conf_bucket)
    print_breakdown("By predicted_hub_category", by_pred_cat)

    if confusions:
        print("\nMisclassifications (predicted -> actual):")
        for fn, pred, actual in confusions:
            print(f"  {fn:55s} {pred:25s} -> {actual}")

    # 3) long-tail coverage gain
    print("\n" + "=" * 70)
    print("§2 LONG-TAIL TRAINING-DATA GAIN IF THESE GET MATCHED")
    print("=" * 70)
    print("(Counts each row where actual_hub_category is filled = a hub")
    print("match that would grow training data for that category.)")
    print()
    cat_gain: dict[str, int] = defaultdict(int)
    for row in rows:
        actual = (row.get("actual_hub_category") or "").strip()
        if actual and actual.lower() != "removed":
            cat_gain[actual] += 1
    for cat, n in sorted(cat_gain.items(), key=lambda x: -x[1]):
        print(f"  {cat:30s} +{n}")

    if args.show_rows:
        print("\n" + "=" * 70)
        print("PER-ROW INFERRED MISS_MODE")
        print("=" * 70)
        for row, primary, secondary in per_row:
            extra = f"  [+{','.join(secondary)}]" if secondary else ""
            fn = row.get("var_filename", "")[:55]
            print(f"  {primary:22s}{extra:25s}  {fn}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
