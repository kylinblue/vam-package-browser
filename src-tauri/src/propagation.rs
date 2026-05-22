//! Hub-match propagation: when one package's hub_* fields are established
//! (manually pinned, overridden, or auto-matched), spread the link to
//! related rows that don't have a confirmed match of their own.
//!
//! Two propagation tiers, both gated by the same strict predicate:
//!
//!   eligible_for_propagation: hub_match_method IS NULL
//!                          OR hub_match_method = 'inherit'
//!
//! Confirmed matches ('filename' | 'fuzzy_title' | 'manual' | 'override')
//! are NEVER overwritten by propagation, regardless of trigger source.
//! This is the rule that protects auto-sync results AND user pins from
//! being stomped by a sibling's later pin.
//!
//! 1. Package-wide (same `creator + package_name`): every hub_* field is
//!    copied to sibling versions. The propagated row's match method is
//!    set to 'inherit'. The user-driven category override flag
//!    (hub_category_manual = 1) is respected via a CASE expression — a
//!    sibling that has a manual category lock keeps its category.
//!
//! 2. Author-wide (same `creator`, different `package_name`): ONLY
//!    `hub_author` is copied, as the cross-package author-identity
//!    normalization. No other hub_* fields propagate at this tier — they
//!    are per-resource, not per-author.
//!
//! Inherited rows are subject to background verification (see the
//! verification queue in commands.rs) — a HEAD probe confirms the .var
//! filename actually matches the resource. On mismatch the inherited
//! fields are wiped and the row falls back to NULL.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Counts of rows touched by a single `propagate_hub_match` call.
/// Surfaced in the action-result toast (translated to user-friendly text
/// in the frontend).
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct PropagationReport {
    /// Rows updated by the package-wide pass (sibling versions of the
    /// same Creator.PackageName).
    pub siblings_updated: i64,
    /// Rows updated by the author-wide hub_author backfill.
    pub authors_updated: i64,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Apply propagation from a freshly-pinned source row to its package-wide
/// and author-wide neighbors. Idempotent and safe to call from within an
/// open transaction.
///
/// Returns the count of rows touched at each tier. If the source row has
/// no hub_resource_id (it isn't actually pinned), this is a no-op and
/// returns zeros — callers needn't gate it themselves.
pub fn propagate_hub_match(conn: &Connection, src_row_id: i64) -> Result<PropagationReport> {
    // Source row's identifying fields + the full hub_* bundle.
    let src = conn.query_row(
        "SELECT creator, package_name, hub_resource_id, hub_url, hub_title,
                hub_author, hub_category, hub_preview_url, hub_billing_tier,
                hub_is_hub_hosted, hub_license, hub_lastmod, hub_external_url
         FROM packages
         WHERE id = ?1",
        params![src_row_id],
        |r| {
            Ok(SourceRow {
                creator: r.get(0)?,
                package_name: r.get(1)?,
                hub_resource_id: r.get(2)?,
                hub_url: r.get(3)?,
                hub_title: r.get(4)?,
                hub_author: r.get(5)?,
                hub_category: r.get(6)?,
                hub_preview_url: r.get(7)?,
                hub_billing_tier: r.get(8)?,
                hub_is_hub_hosted: r.get(9)?,
                hub_license: r.get(10)?,
                hub_lastmod: r.get(11)?,
                hub_external_url: r.get(12)?,
            })
        },
    )?;

    // No pin on the source → nothing to propagate. Callers can skip the
    // check; we do it here so the helper is safe to call unconditionally
    // from a sync-flow loop.
    if src.hub_resource_id.is_none() {
        return Ok(PropagationReport::default());
    }

    let now = unix_now();

    // ── Tier 1: package-wide ──────────────────────────────────────────────
    // Sibling rows = same creator+package_name, different id, in the soft
    // state (NULL or 'inherit'). Confirmed matches are excluded by the
    // guard predicate.
    //
    // The hub_category CASE preserves a sibling's user-locked category
    // (hub_category_manual = 1) — propagation must not silently undo a
    // deliberate category override even when the resource ID matches.
    let siblings_updated = conn.execute(
        "UPDATE packages SET
           hub_resource_id   = ?1,
           hub_url           = ?2,
           hub_title         = ?3,
           hub_author        = ?4,
           hub_category      = CASE WHEN hub_category_manual = 1
                                    THEN hub_category
                                    ELSE ?5 END,
           hub_preview_url   = ?6,
           hub_billing_tier  = ?7,
           hub_is_hub_hosted = ?8,
           hub_license       = ?9,
           hub_lastmod       = ?10,
           hub_external_url  = ?11,
           hub_synced_at     = ?12,
           hub_sync_state    = 'matched',
           hub_match_method  = 'inherit'
         WHERE creator = ?13
           AND package_name = ?14
           AND id != ?15
           AND (hub_match_method IS NULL OR hub_match_method = 'inherit')",
        params![
            src.hub_resource_id,
            src.hub_url,
            src.hub_title,
            src.hub_author,
            src.hub_category,
            src.hub_preview_url,
            src.hub_billing_tier,
            src.hub_is_hub_hosted,
            src.hub_license,
            src.hub_lastmod,
            src.hub_external_url,
            now,
            &src.creator,
            &src.package_name,
            src_row_id,
        ],
    )? as i64;

    // ── Tier 2: author-wide hub_author backfill ───────────────────────────
    // Rows by the same creator but a DIFFERENT package_name (the sibling
    // set was handled above). Only `hub_author` is copied — the other
    // hub_* fields are resource-specific and don't generalize across
    // packages.
    //
    // Two guards apply:
    //   (a) NULL|inherit hub_match_method — same protection as Tier 1
    //   (b) hub_author_manual = 0 — a user-locked author display name
    //       takes precedence over any propagation source. Both v17+.
    //
    // Last-write-wins among soft rows on the rare case where two
    // different packages by the same creator pin to resources with
    // diverging hub_author strings — we don't reconcile, just reflect
    // the latest pin's view.
    let authors_updated = if let Some(ref hub_author) = src.hub_author {
        conn.execute(
            "UPDATE packages SET hub_author = ?1
             WHERE creator = ?2
               AND id != ?3
               AND package_name != ?4
               AND (hub_match_method IS NULL OR hub_match_method = 'inherit')
               AND hub_author_manual = 0",
            params![hub_author, &src.creator, src_row_id, &src.package_name],
        )? as i64
    } else {
        0
    };

    Ok(PropagationReport {
        siblings_updated,
        authors_updated,
    })
}

struct SourceRow {
    creator: String,
    package_name: String,
    hub_resource_id: Option<i64>,
    hub_url: Option<String>,
    hub_title: Option<String>,
    hub_author: Option<String>,
    hub_category: Option<String>,
    hub_preview_url: Option<String>,
    hub_billing_tier: Option<String>,
    hub_is_hub_hosted: Option<i64>,
    hub_license: Option<String>,
    hub_lastmod: Option<i64>,
    hub_external_url: Option<String>,
}

// ─── Tests ────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal `packages` schema covering only the columns the propagation
    /// helper reads or writes. We don't run the full app migration chain
    /// in tests — too much surface area for what's a focused predicate
    /// check. The shape mirrors the v16 layout.
    fn schema(conn: &Connection) {
        conn.execute_batch(
            r#"
            CREATE TABLE packages (
                id INTEGER PRIMARY KEY,
                creator TEXT NOT NULL,
                package_name TEXT NOT NULL,
                hub_resource_id INTEGER,
                hub_url TEXT,
                hub_title TEXT,
                hub_author TEXT,
                hub_category TEXT,
                hub_preview_url TEXT,
                hub_billing_tier TEXT,
                hub_is_hub_hosted INTEGER,
                hub_license TEXT,
                hub_lastmod INTEGER,
                hub_external_url TEXT,
                hub_synced_at INTEGER,
                hub_sync_state TEXT,
                hub_match_method TEXT,
                hub_category_manual INTEGER DEFAULT 0,
                hub_author_manual INTEGER DEFAULT 0
            );
            "#,
        )
        .unwrap();
    }

    /// Convenience inserter. Defaults to the "fully unmatched" state so
    /// each test only specifies what it cares about.
    #[derive(Default)]
    struct Row<'a> {
        id: i64,
        creator: &'a str,
        package_name: &'a str,
        hub_resource_id: Option<i64>,
        hub_author: Option<&'a str>,
        hub_category: Option<&'a str>,
        hub_match_method: Option<&'a str>,
        hub_category_manual: i64,
        hub_author_manual: i64,
    }

    fn insert(conn: &Connection, r: Row) {
        conn.execute(
            "INSERT INTO packages
               (id, creator, package_name, hub_resource_id, hub_author,
                hub_category, hub_match_method, hub_category_manual,
                hub_author_manual)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                r.id,
                r.creator,
                r.package_name,
                r.hub_resource_id,
                r.hub_author,
                r.hub_category,
                r.hub_match_method,
                r.hub_category_manual,
                r.hub_author_manual,
            ],
        )
        .unwrap();
    }

    fn method_of(conn: &Connection, id: i64) -> Option<String> {
        conn.query_row(
            "SELECT hub_match_method FROM packages WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn author_of(conn: &Connection, id: i64) -> Option<String> {
        conn.query_row(
            "SELECT hub_author FROM packages WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn category_of(conn: &Connection, id: i64) -> Option<String> {
        conn.query_row(
            "SELECT hub_category FROM packages WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Source has no resource id → nothing propagates.
    #[test]
    fn skips_when_source_has_no_pin() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row { id: 1, creator: "A", package_name: "P", ..Default::default() });
        insert(&conn, Row { id: 2, creator: "A", package_name: "P", ..Default::default() });
        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.siblings_updated, 0);
        assert_eq!(report.authors_updated, 0);
    }

    /// Sibling with NULL method gets filled, sibling with 'filename'
    /// match is left alone. This is the core guard predicate.
    #[test]
    fn package_wide_protects_confirmed_matches() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        // Source: freshly pinned v1.
        insert(&conn, Row {
            id: 1, creator: "A", package_name: "P",
            hub_resource_id: Some(42), hub_author: Some("HubA"),
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        // Sibling v2: unmatched → should inherit.
        insert(&conn, Row { id: 2, creator: "A", package_name: "P", ..Default::default() });
        // Sibling v3: already auto-matched via filename → must NOT be touched.
        insert(&conn, Row {
            id: 3, creator: "A", package_name: "P",
            hub_resource_id: Some(99), hub_author: Some("OtherAuthor"),
            hub_match_method: Some("filename"),
            ..Default::default()
        });
        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.siblings_updated, 1);
        assert_eq!(method_of(&conn, 2).as_deref(), Some("inherit"));
        assert_eq!(method_of(&conn, 3).as_deref(), Some("filename")); // untouched
        assert_eq!(author_of(&conn, 3).as_deref(), Some("OtherAuthor")); // untouched
    }

    /// Inherit rows ARE eligible for re-propagation — they're a soft
    /// state, callers can supersede them.
    #[test]
    fn package_wide_overwrites_inherit() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row {
            id: 1, creator: "A", package_name: "P",
            hub_resource_id: Some(42), hub_author: Some("New"),
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        insert(&conn, Row {
            id: 2, creator: "A", package_name: "P",
            hub_resource_id: Some(7), hub_author: Some("Old"),
            hub_match_method: Some("inherit"),
            ..Default::default()
        });
        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.siblings_updated, 1);
        assert_eq!(method_of(&conn, 2).as_deref(), Some("inherit"));
        assert_eq!(author_of(&conn, 2).as_deref(), Some("New"));
    }

    /// User-locked category survives propagation even when the rest of
    /// the hub_* bundle is overwritten.
    #[test]
    fn package_wide_respects_hub_category_manual() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row {
            id: 1, creator: "A", package_name: "P",
            hub_resource_id: Some(42), hub_category: Some("Scenes"),
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        insert(&conn, Row {
            id: 2, creator: "A", package_name: "P",
            hub_category: Some("Looks"),
            hub_category_manual: 1,
            // Note: hub_match_method is NULL → eligible for the rest of
            // the bundle, but category should stick.
            ..Default::default()
        });
        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.siblings_updated, 1);
        assert_eq!(method_of(&conn, 2).as_deref(), Some("inherit"));
        assert_eq!(category_of(&conn, 2).as_deref(), Some("Looks")); // sticky
    }

    /// Author-wide pass touches only same-creator different-package_name
    /// rows in the soft state. Confirmed matches and the source row's
    /// siblings (handled separately) are excluded.
    #[test]
    fn author_wide_backfills_hub_author_only_on_soft_rows() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row {
            id: 1, creator: "testAuthor", package_name: "Foo",
            hub_resource_id: Some(42), hub_author: Some("TAuthor"),
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        // Other package by same creator, unmatched → backfill.
        insert(&conn, Row { id: 2, creator: "testAuthor", package_name: "Bar", ..Default::default() });
        // Other package by same creator, already auto-matched → protected.
        insert(&conn, Row {
            id: 3, creator: "testAuthor", package_name: "Baz",
            hub_resource_id: Some(7), hub_author: Some("DifferentName"),
            hub_match_method: Some("fuzzy_title"),
            ..Default::default()
        });
        // Different creator → never touched.
        insert(&conn, Row { id: 4, creator: "otherAuthor", package_name: "Quux", ..Default::default() });
        // Sibling of the source (same package_name) — handled by the
        // package-wide pass, must NOT be double-counted by author-wide.
        insert(&conn, Row { id: 5, creator: "testAuthor", package_name: "Foo", ..Default::default() });

        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.siblings_updated, 1); // id=5 (other Foo version)
        assert_eq!(report.authors_updated, 1);  // id=2 only
        assert_eq!(author_of(&conn, 2).as_deref(), Some("TAuthor"));
        assert_eq!(author_of(&conn, 3).as_deref(), Some("DifferentName")); // protected
        assert_eq!(author_of(&conn, 4), None);
    }

    /// A row with hub_author_manual=1 protects its hub_author from
    /// author-wide propagation, even when the row is otherwise in the
    /// soft state (NULL or 'inherit' match method).
    #[test]
    fn author_wide_respects_hub_author_manual() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row {
            id: 1, creator: "testAuthor", package_name: "Foo",
            hub_resource_id: Some(42), hub_author: Some("TAuthorHub"),
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        // Other package by same creator with user-locked author display.
        // Soft state (hub_match_method NULL), so guard (a) would otherwise
        // allow the backfill — but the manual flag overrides.
        insert(&conn, Row {
            id: 2, creator: "testAuthor", package_name: "Bar",
            hub_author: Some("UserPreferredName"),
            hub_author_manual: 1,
            ..Default::default()
        });
        // Control: same creator, soft state, no manual flag → does
        // receive the backfill (so we know the propagation actually
        // ran and we're not just observing a global skip).
        insert(&conn, Row {
            id: 3, creator: "testAuthor", package_name: "Baz",
            ..Default::default()
        });

        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.authors_updated, 1); // id=3 only
        assert_eq!(author_of(&conn, 2).as_deref(), Some("UserPreferredName"));
        assert_eq!(author_of(&conn, 3).as_deref(), Some("TAuthorHub"));
    }

    /// Author-wide is a no-op when the source row has no hub_author.
    #[test]
    fn author_wide_skips_when_no_hub_author() {
        let conn = Connection::open_in_memory().unwrap();
        schema(&conn);
        insert(&conn, Row {
            id: 1, creator: "A", package_name: "P",
            hub_resource_id: Some(42),
            hub_author: None,
            hub_match_method: Some("manual"),
            ..Default::default()
        });
        insert(&conn, Row { id: 2, creator: "A", package_name: "Q", ..Default::default() });
        let report = propagate_hub_match(&conn, 1).unwrap();
        assert_eq!(report.authors_updated, 0);
        assert_eq!(author_of(&conn, 2), None);
    }
}
