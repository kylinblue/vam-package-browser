"""Read-only diagnostic: break down the matched-rows population by
thumbnail + hub_preview_url state. Tells us why the backfill query
returns 0 candidates."""

import os
import sqlite3


def main() -> None:
    db = os.path.join(
        os.environ["APPDATA"], "com.github.kylinblue.vam-package-browser", "index.sqlite"
    )
    if not os.path.exists(db):
        legacy = os.path.join(
            os.environ["APPDATA"], "com.github.kylinblue.vam-package-browser", "index.sqlite"
        )
        if os.path.exists(legacy):
            db = legacy
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)

    overall = c.execute(
        """
        SELECT
          COUNT(*)                                                AS total_matched,
          SUM(has_preview)                                        AS with_local_thumb,
          SUM(CASE WHEN hub_preview_url IS NOT NULL THEN 1 ELSE 0 END) AS with_hub_url,
          SUM(CASE WHEN hub_preview_pulled_at IS NOT NULL THEN 1 ELSE 0 END) AS hub_pulled,
          SUM(CASE WHEN hub_preview_url IS NOT NULL
                    AND hub_preview_pulled_at IS NULL THEN 1 ELSE 0 END) AS backfill_candidates_now,
          SUM(CASE WHEN has_preview = 0 AND hub_preview_url IS NULL    THEN 1 ELSE 0 END) AS missing_both,
          SUM(CASE WHEN has_preview = 1 AND hub_preview_url IS NULL    THEN 1 ELSE 0 END) AS local_only
        FROM packages
        WHERE hub_sync_state = 'matched' AND is_hidden = 0
        """
    ).fetchone()

    total, w_local, w_hub_url, hub_pulled, backfill_now, missing_both, local_only = overall
    print(f"Matched rows (excluding hidden): {total}")
    print(f"  with local thumbnail (has_preview=1):  {w_local}")
    print(f"  with hub_preview_url set:              {w_hub_url}")
    print(f"  hub-pulled (hub_preview_pulled_at NOT NULL): {hub_pulled}")
    print()
    print("Current backfill state:")
    print(f"  remaining candidates (URL set, never pulled): {backfill_now}")
    print(f"  hub-pulled so far:                            {hub_pulled}")
    print(f"  no URL stored (matcher missed):               {missing_both + local_only}")
    print()

    # Break down has_preview=0 rows by hub_match_method (which tier
    # matched them?) — useful to spot e.g. "filename matches lose preview_url
    # while slug_match matches retain it".
    print("has_preview=0 rows by hub_match_method:")
    rows = c.execute(
        """
        SELECT COALESCE(hub_match_method, 'NULL'),
               COUNT(*) AS n,
               SUM(CASE WHEN hub_preview_url IS NOT NULL THEN 1 ELSE 0 END) AS has_url
        FROM packages
        WHERE hub_sync_state = 'matched'
          AND is_hidden = 0
          AND has_preview = 0
        GROUP BY hub_match_method
        ORDER BY n DESC
        """
    ).fetchall()
    for method, n, has_url in rows:
        pct = (100.0 * has_url / n) if n else 0.0
        print(f"  {method:18s}  n={n:5d}   with hub_url={has_url:5d}  ({pct:5.1f}%)")
    print()

    # Sample a few backfill-candidate rows so the user can eyeball whether
    # the hub_preview_url looks legit.
    print("Sample backfill candidates (up to 5):")
    rows = c.execute(
        """
        SELECT id, creator, package_name, hub_match_method, hub_preview_url
        FROM packages
        WHERE hub_sync_state = 'matched'
          AND is_hidden = 0
          AND has_preview = 0
          AND hub_preview_url IS NOT NULL
        LIMIT 5
        """
    ).fetchall()
    if not rows:
        print("  (none — confirms the backfill log)")
    for r in rows:
        print(f"  id={r[0]:5d}  {r[1]}.{r[2]:30s}  method={r[3]}  url={r[4]}")


if __name__ == "__main__":
    main()
