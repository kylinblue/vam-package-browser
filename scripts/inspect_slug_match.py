"""Diff the labeled hub-sync CSV against the live DB to inspect how the
slug_match tier performed on the labeled set. Read-only against the DB.

Prints a single-screen-ish summary plus a per-row table. Each row gets
one of these status markers:

  [SLUG OK]  was tagged [auto:slug-match] in the labels, and the live DB
             now shows hub_match_method='slug_match' for it. The new
             tier did its job.

  [SLUG XF]  was tagged [auto:slug-match], but the live DB now shows
             hub_match_method='filename' or 'fuzzy_title' (XF search
             ranking found it after all — counts as a win for recall
             but the slug-match tier didn't earn credit).

  [SLUG XX]  was tagged [auto:slug-match] and STILL not_found in the
             live DB. The slug-match tier should have caught this and
             didn't — look at the row and debug.

  [HAND OK]  user filled hub_url_if_found by hand (no auto: tag), DB now
             matched. The structural fix (package_rename, etc.) wasn't
             our target, but the row landed anyway.

  [HAND XX]  user said this is matchable, but live DB still not_found.
             Expected residual for the harder miss modes (rename, etc.)
             that this matcher fix doesn't address.

  [NEG OK]   labeled matchable_on_hub=no or actual='Removed', live DB
             still not_found. Correct negative.

  [NEG !!]   labeled not-on-hub but live DB now matched. Sus — verify.

  [UNK]      no labeled signal (10 rows landed here in the analysis pass).

Headline numbers up top: how many of the 14 expected slug-match rows
flipped to slug_match, how many escaped to other methods, how many
were still missed.

Usage:
  py scripts\\inspect_slug_match.py                       # pretty TTY output
  py scripts\\inspect_slug_match.py --no-color            # plain text
  py scripts\\inspect_slug_match.py --only-misses         # filter to interesting rows
  py scripts\\inspect_slug_match.py | less -RS            # for paging
"""

import argparse
import csv
import os
import re
import sqlite3
import sys
from pathlib import Path


def normalize(s: str) -> str:
    """Mirrors hub.rs normalize_compare: lowercase + alphanumeric only."""
    return "".join(c.lower() for c in s if c.isalnum())


def slug_from_url(url: str) -> str | None:
    """Pull the slug stem from /resources/{slug}.{id}/ or /threads/{slug}.{id}/."""
    m = re.search(
        r"/(?:resources|threads)/([a-z0-9\-]+?)(?:\.\d+)?/?$", url.strip()
    )
    return m.group(1) if m else None

# ----- color helpers --------------------------------------------------------

class C:
    """ANSI color codes. Set to empty strings when not a TTY or --no-color."""
    RESET = "\033[0m"
    DIM = "\033[2m"
    BOLD = "\033[1m"
    GREEN = "\033[32m"
    RED = "\033[31m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    CYAN = "\033[36m"
    GRAY = "\033[90m"

    @classmethod
    def disable(cls) -> None:
        for k in list(vars(cls).keys()):
            if isinstance(getattr(cls, k), str) and getattr(cls, k).startswith("\033"):
                setattr(cls, k, "")


# ----- status classification ------------------------------------------------

# Expectations derived from the labeled CSV:
#   expected_slug_match: row was tagged [auto:slug-match] -> slug-match tier
#                        is the targeted fix for it
#   expected_matchable:  user said the row IS on the hub (URL filled, or
#                        explicitly matchable_on_hub=yes)
#   expected_negative:   user said NOT on hub (matchable_on_hub=no, or
#                        actual_hub_category='Removed')
#   no_signal:           labeled CSV has no opinion on this row

EXPECT_SLUG = "slug-match"
EXPECT_MATCH = "matchable"
EXPECT_NEG = "negative"
EXPECT_UNK = "unknown"


def classify_expected(label_row: dict) -> str:
    notes = (label_row.get("notes") or "").strip()
    url = (label_row.get("hub_url_if_found") or "").strip()
    matchable = (label_row.get("matchable_on_hub") or "").strip().lower()
    actual = (label_row.get("actual_hub_category") or "").strip()
    pkg = label_row.get("package_name", "")

    # Negatives first -- explicit unmatchable signal trumps anything else.
    if matchable == "no":
        return EXPECT_NEG
    if actual.lower() == "removed":
        return EXPECT_NEG

    # Slug-match expectation: either the auto-prefill tagged it, OR the
    # human-found URL's slug normalizes to the package name (in which case
    # the slug-match tier should also catch it, regardless of how the
    # human discovered it).
    if "[auto:slug-match]" in notes:
        return EXPECT_SLUG
    if url:
        slug = slug_from_url(url)
        if slug and normalize(pkg) == normalize(slug):
            return EXPECT_SLUG
        # URL found but slug differs from pkg name -> structural rename;
        # the slug-match tier *won't* catch this; needs a different fix.
        return EXPECT_MATCH
    if matchable == "yes":
        return EXPECT_MATCH
    return EXPECT_UNK


def status_marker(expected: str, db_state: str, db_method: str | None) -> tuple[str, str]:
    """Return (marker, color). Marker is a 9-char string for column alignment."""
    matched = db_state == "matched"
    method = (db_method or "").lower()

    if expected == EXPECT_SLUG:
        if matched and method == "slug_match":
            return ("[SLUG OK]", C.GREEN)
        if matched:
            return ("[SLUG XF]", C.CYAN)  # XF caught it; recall win, attribution loss
        return ("[SLUG XX]", C.RED)
    if expected == EXPECT_MATCH:
        if matched:
            return ("[HAND OK]", C.GREEN)
        return ("[HAND XX]", C.YELLOW)
    if expected == EXPECT_NEG:
        if matched:
            return ("[NEG  !!]", C.RED)
        return ("[NEG  OK]", C.GREEN)
    return ("[UNK    ]", C.GRAY)


# ----- main -----------------------------------------------------------------

def default_db_path() -> Path:
    base = Path(os.environ["APPDATA"])
    current = base / "com.github.kylinblue.vam-package-browser" / "index.sqlite"
    if current.exists():
        return current
    legacy = base / "com.github.kylinblue.vam-package-browser" / "index.sqlite"
    return legacy if legacy.exists() else current


def default_csv_path() -> Path:
    return Path("labels") / "hub-sync" / "notfound-sample-2026-05-19.csv"


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--csv", type=Path, default=default_csv_path())
    p.add_argument("--db", type=Path, default=default_db_path())
    p.add_argument("--no-color", action="store_true")
    p.add_argument(
        "--only-misses",
        action="store_true",
        help="Only show rows where slug-match was expected but failed, "
        "or hand-labeled matchable rows still not_found.",
    )
    p.add_argument(
        "--all-slug-matches",
        action="store_true",
        help="In addition to the labeled-set diff, list EVERY package in "
        "the DB with hub_match_method='slug_match'. Useful for "
        "spot-checking the new tier's full output.",
    )
    p.add_argument(
        "--slug-match-limit",
        type=int,
        default=200,
        help="Max rows to show with --all-slug-matches (default 200).",
    )
    p.add_argument(
        "--sort",
        choices=["status", "id", "creator"],
        default="status",
        help="Row sort. 'status' groups failures together (default).",
    )
    args = p.parse_args()

    if args.no_color or not sys.stdout.isatty():
        C.disable()

    if not args.csv.exists():
        print(f"error: labels csv not found at {args.csv}", file=sys.stderr)
        return 2
    if not args.db.exists():
        print(f"error: db not found at {args.db}", file=sys.stderr)
        return 2

    # Tolerate cp1252 fallback for spreadsheet-saved CSVs (smart quotes etc.).
    raw = args.csv.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode("cp1252")
    label_rows = list(csv.DictReader(text.splitlines()))

    c = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)

    # Pull live DB state for the labeled package ids in one query.
    ids = [int(r["id"]) for r in label_rows if r.get("id", "").strip().isdigit()]
    placeholders = ",".join("?" * len(ids))
    db_state: dict[int, dict] = {}
    for row in c.execute(
        f"""
        SELECT id, hub_sync_state, hub_match_method, hub_url, hub_synced_at
          FROM packages WHERE id IN ({placeholders})
        """,
        ids,
    ):
        db_state[row[0]] = {
            "state": row[1],
            "method": row[2],
            "url": row[3],
            "synced_at": row[4],
        }

    # Build enriched per-row records.
    records = []
    for lr in label_rows:
        try:
            pid = int(lr["id"])
        except (ValueError, KeyError):
            continue
        expected = classify_expected(lr)
        db = db_state.get(pid, {"state": "<missing>", "method": None, "url": None})
        marker, color = status_marker(expected, db.get("state") or "", db.get("method"))
        records.append(
            {
                "id": pid,
                "creator": lr.get("creator", ""),
                "package_name": lr.get("package_name", ""),
                "expected": expected,
                "labeled_url": (lr.get("hub_url_if_found") or "").strip(),
                "db_state": db.get("state"),
                "db_method": db.get("method"),
                "db_url": db.get("url"),
                "marker": marker,
                "color": color,
                "notes": (lr.get("notes") or "").strip(),
            }
        )

    # ---- summary -----------------------------------------------------------

    def count(pred) -> int:
        return sum(1 for r in records if pred(r))

    n_slug_expected = count(lambda r: r["expected"] == EXPECT_SLUG)
    n_slug_caught = count(
        lambda r: r["expected"] == EXPECT_SLUG and r["marker"] == "[SLUG OK]"
    )
    n_slug_escaped = count(
        lambda r: r["expected"] == EXPECT_SLUG and r["marker"] == "[SLUG XF]"
    )
    n_slug_missed = count(
        lambda r: r["expected"] == EXPECT_SLUG and r["marker"] == "[SLUG XX]"
    )

    n_hand_expected = count(lambda r: r["expected"] == EXPECT_MATCH)
    n_hand_caught = count(
        lambda r: r["expected"] == EXPECT_MATCH and r["marker"] == "[HAND OK]"
    )

    n_neg_expected = count(lambda r: r["expected"] == EXPECT_NEG)
    n_neg_correct = count(
        lambda r: r["expected"] == EXPECT_NEG and r["marker"] == "[NEG  OK]"
    )
    n_neg_violated = count(
        lambda r: r["expected"] == EXPECT_NEG and r["marker"] == "[NEG  !!]"
    )

    db_method_hist: dict[str, int] = {}
    for r in records:
        if r["db_state"] == "matched":
            m = r["db_method"] or "<null>"
            db_method_hist[m] = db_method_hist.get(m, 0) + 1

    print(f"{C.BOLD}slug_match performance vs labeled set{C.RESET}")
    print(f"  csv: {args.csv}")
    print(f"  db:  {args.db}")
    print()

    def fmt_pct(n: int, d: int) -> str:
        if d == 0:
            return "n/a"
        return f"{100.0 * n / d:.0f}%"

    g = C.GREEN
    r = C.RED
    y = C.YELLOW
    z = C.RESET

    print(f"  {C.BOLD}Slug-match-expected ({n_slug_expected} rows){z}")
    print(
        f"    caught by slug_match:    {g}{n_slug_caught}/{n_slug_expected}{z}"
        f"  ({fmt_pct(n_slug_caught, n_slug_expected)})"
    )
    print(
        f"    escaped to other method: {C.CYAN}{n_slug_escaped}/{n_slug_expected}{z}"
        f"  (recall win, attribution loss)"
    )
    print(
        f"    still missed:            {r}{n_slug_missed}/{n_slug_expected}{z}"
        f"  (investigate — see [SLUG XX] rows below)"
    )
    print()
    print(f"  {C.BOLD}Hand-labeled matchable ({n_hand_expected} rows){z}")
    print(
        f"    matched in live DB:      {g}{n_hand_caught}/{n_hand_expected}{z}"
        f"  ({fmt_pct(n_hand_caught, n_hand_expected)})"
    )
    print(
        f"    still missed:            {y}{n_hand_expected - n_hand_caught}/{n_hand_expected}{z}"
        f"  (expected residual — different miss modes)"
    )
    print()
    print(f"  {C.BOLD}Labeled not-on-hub ({n_neg_expected} rows){z}")
    print(
        f"    correctly still missed:  {g}{n_neg_correct}/{n_neg_expected}{z}"
    )
    print(
        f"    spuriously matched:      {r}{n_neg_violated}/{n_neg_expected}{z}"
        f"  (precision violation — verify URLs)"
    )
    print()
    if db_method_hist:
        print(f"  {C.BOLD}DB method histogram (matched rows in sample){z}")
        for m, n in sorted(db_method_hist.items(), key=lambda kv: -kv[1]):
            print(f"    {m:14s} {n}")
        print()

    # ---- per-row table -----------------------------------------------------

    if args.only_misses:
        records = [
            r
            for r in records
            if r["marker"] in ("[SLUG XX]", "[HAND XX]", "[NEG  !!]")
        ]

    sort_key = {
        "status": lambda r: (r["marker"], r["creator"]),
        "id": lambda r: r["id"],
        "creator": lambda r: (r["creator"], r["package_name"]),
    }[args.sort]
    records.sort(key=sort_key)

    if not records:
        print(f"{C.DIM}(no rows to show){z}")
        return 0

    # Column widths tuned for ~120-col terminal.
    print(f"{C.BOLD}{'STATUS':10s} {'ID':>5s}  {'CREATOR.PACKAGE':45s}  "
          f"{'DB STATE':12s} {'METHOD':12s}  NOTES{z}")
    print(f"{C.DIM}{'-' * 10:10s} {'-' * 5:>5s}  {'-' * 45:45s}  "
          f"{'-' * 12:12s} {'-' * 12:12s}  -----{z}")
    for rec in records:
        full_name = f"{rec['creator']}.{rec['package_name']}"
        if len(full_name) > 45:
            full_name = full_name[:42] + "..."
        method = rec["db_method"] or "-"
        state = rec["db_state"] or "-"
        # Highlight the method cell when it's slug_match -- the whole point.
        method_disp = f"{C.GREEN}{method}{z}" if method == "slug_match" else method
        notes = rec["notes"]
        if len(notes) > 40:
            notes = notes[:37] + "..."
        print(
            f"{rec['color']}{rec['marker']:10s}{z} {rec['id']:>5d}  "
            f"{full_name:45s}  {state:12s} {method_disp:21s}  "
            f"{C.DIM}{notes}{z}"
        )

    print()
    print(f"{C.DIM}Legend:{z}")
    print(f"  {C.GREEN}[SLUG OK]{z}  slug-match tier caught it (the target win)")
    print(f"  {C.CYAN}[SLUG XF]{z}  matched, but via filename/fuzzy_title instead")
    print(f"  {C.RED}[SLUG XX]{z}  slug-match expected to catch it but didn't")
    print(f"  {C.GREEN}[HAND OK]{z}  hand-labeled matchable, now matched")
    print(f"  {C.YELLOW}[HAND XX]{z}  hand-labeled matchable, still not_found (other miss modes)")
    print(f"  {C.GREEN}[NEG  OK]{z}  labeled not-on-hub, correctly still not_found")
    print(f"  {C.RED}[NEG  !!]{z}  labeled not-on-hub but spuriously matched")
    print(f"  {C.GRAY}[UNK    ]{z}  no labeled signal")

    # ---- full slug-match listing (DB-wide spot-check) ----------------------
    if args.all_slug_matches:
        labeled_ids = {r["id"] for r in records}
        all_slug = c.execute(
            """
            SELECT p.id, p.creator, p.package_name, p.hub_url, p.hub_title,
                   p.hub_author, p.hub_category, p.hub_billing_tier,
                   p.hub_is_hub_hosted, p.hub_synced_at
              FROM packages p
             WHERE p.hub_match_method = 'slug_match'
               AND p.error IS NULL
               AND p.is_hidden = 0
             ORDER BY p.hub_synced_at DESC, p.creator COLLATE NOCASE
             LIMIT ?
            """,
            (args.slug_match_limit,),
        ).fetchall()

        total_slug = c.execute(
            "SELECT COUNT(*) FROM packages WHERE hub_match_method='slug_match' "
            "AND error IS NULL AND is_hidden=0"
        ).fetchone()[0]

        print()
        print(f"{C.BOLD}All slug_match rows in DB (DB-wide spot check){z}")
        print(
            f"  Showing {min(len(all_slug), args.slug_match_limit)} of {total_slug} "
            f"({'LIMIT applied' if total_slug > args.slug_match_limit else 'all'})"
        )
        print()
        print(
            f"{C.BOLD}{'TAG':5s} {'ID':>5s}  {'CREATOR.PACKAGE':38s}  "
            f"{'HUB TITLE':30s}  {'CATEGORY':22s}  {'AUTHOR':18s}  HUB URL{z}"
        )
        print(f"{C.DIM}{'-' * 5:5s} {'-' * 5:>5s}  {'-' * 38:38s}  "
              f"{'-' * 30:30s}  {'-' * 22:22s}  {'-' * 18:18s}  -------{z}")
        for row in all_slug:
            pid, creator, pkg, hub_url, hub_title, hub_author, hub_cat, billing, hosted, synced = row
            full_name = f"{creator}.{pkg}"
            if len(full_name) > 38:
                full_name = full_name[:35] + "..."
            title = (hub_title or "")[:30]
            cat = hub_cat or "-"
            if billing:
                cat = f"{cat} [{billing}]"
            cat = cat[:22]
            author = (hub_author or "-")[:18]
            url = hub_url or "-"
            # Tag: [LBL] = was in labeled set, [NEW] = not in labeled set
            in_labeled = pid in labeled_ids
            tag = f"{C.CYAN}[LBL]{z}" if in_labeled else f"{C.GREEN}[NEW]{z}"
            # Authority hint: green check if author normalize-matches creator
            creator_n = normalize(creator)
            author_n = normalize(hub_author or "")
            # XF author handles often have a .N suffix in slugs; strip trailing
            # .digits before comparing.
            author_n_clean = re.sub(r"\d+$", "", author_n).rstrip()
            if creator_n and author_n_clean and (
                creator_n == author_n_clean
                or creator_n in author_n_clean
                or author_n_clean in creator_n
            ):
                author_color = ""
                author_reset = ""
            else:
                author_color = C.YELLOW
                author_reset = z
            print(
                f"{tag} {pid:>5d}  {full_name:38s}  {title:30s}  "
                f"{cat:22s}  {author_color}{author:18s}{author_reset}  {C.DIM}{url}{z}"
            )

        print()
        print(f"{C.DIM}Tags:{z}")
        print(f"  {C.CYAN}[LBL]{z}  also in labeled CSV — cross-referenced above")
        print(f"  {C.GREEN}[NEW]{z}  not in labeled set — fresh slug_match recall")
        print(f"  {C.YELLOW}yellow author{z}  hub_author handle doesn't normalize-match the local creator")
        print(f"               (possible cross-creator false positive — eyeball before trusting)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
