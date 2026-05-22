use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

pub fn open_and_migrate(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("open sqlite db at {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 1 {
        migrate_v0_to_v1(&conn)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    if current < 2 {
        migrate_v1_to_v2(&conn)?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    if current < 3 {
        migrate_v2_to_v3(&conn)?;
        conn.pragma_update(None, "user_version", 3)?;
    }
    if current < 4 {
        migrate_v3_to_v4(&conn)?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    if current < 5 {
        migrate_v4_to_v5(&conn)?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    if current < 6 {
        migrate_v5_to_v6(&conn)?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    if current < 7 {
        migrate_v6_to_v7(&conn)?;
        conn.pragma_update(None, "user_version", 7)?;
    }
    if current < 8 {
        migrate_v7_to_v8(&conn)?;
        conn.pragma_update(None, "user_version", 8)?;
    }
    if current < 9 {
        migrate_v8_to_v9(&conn)?;
        conn.pragma_update(None, "user_version", 9)?;
    }
    if current < 10 {
        migrate_v9_to_v10(&conn)?;
        conn.pragma_update(None, "user_version", 10)?;
    }
    if current < 11 {
        migrate_v10_to_v11(&conn)?;
        conn.pragma_update(None, "user_version", 11)?;
    }
    if current < 12 {
        migrate_v11_to_v12(&conn)?;
        conn.pragma_update(None, "user_version", 12)?;
    }
    if current < 13 {
        migrate_v12_to_v13(&conn)?;
        conn.pragma_update(None, "user_version", 13)?;
    }
    if current < 14 {
        migrate_v13_to_v14(&conn)?;
        conn.pragma_update(None, "user_version", 14)?;
    }
    if current < 15 {
        migrate_v14_to_v15(&conn)?;
        conn.pragma_update(None, "user_version", 15)?;
    }
    if current < 16 {
        migrate_v15_to_v16(&conn)?;
        conn.pragma_update(None, "user_version", 16)?;
    }
    Ok(conn)
}

fn migrate_v15_to_v16(conn: &Connection) -> Result<()> {
    // Visibility presets / Load-Unload feature. Four tables:
    //
    //   visibility_presets             — named seed bags ("Looks I'm working on")
    //   visibility_preset_creators     — author seeds, resolved fresh at closure time
    //                                    against packages.creator (so new .vars by
    //                                    a seeded author auto-join on next Load)
    //   visibility_preset_packages     — explicit package seeds (hand-picked)
    //   active_folder_state            — what's currently hardlinked into addon_root,
    //                                    authoritative for diff/cleanup. Every row
    //                                    is, by construction, an NTFS hardlink to the
    //                                    matching managed_root file.
    //
    // The closure (= seeds ∪ transitive deps via package_dep_links) is computed
    // on demand in visibility::compute_closure, not materialized into a table.
    //
    // See TODO-visibility-presets.md for the full design rationale, including
    // why this changes the read-only invariant currently in CLAUDE.md
    // (post-setup, managed_root is read-only; addon_root becomes the active
    // folder). A CLAUDE-followup.md flags that change for a later doc commit.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS visibility_presets (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL UNIQUE,
            description TEXT,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS visibility_preset_creators (
            preset_id INTEGER NOT NULL,
            creator   TEXT NOT NULL,
            PRIMARY KEY (preset_id, creator),
            FOREIGN KEY (preset_id) REFERENCES visibility_presets(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS visibility_preset_packages (
            preset_id  INTEGER NOT NULL,
            package_id INTEGER NOT NULL,
            PRIMARY KEY (preset_id, package_id),
            FOREIGN KEY (preset_id)  REFERENCES visibility_presets(id) ON DELETE CASCADE,
            FOREIGN KEY (package_id) REFERENCES packages(id)            ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS active_folder_state (
            package_id      INTEGER PRIMARY KEY,
            active_path     TEXT NOT NULL,
            materialized_at INTEGER NOT NULL,
            FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_preset_pkgs_pkg
            ON visibility_preset_packages(package_id);
        CREATE INDEX IF NOT EXISTS idx_preset_creators_cr
            ON visibility_preset_creators(creator);
        "#,
    )?;
    Ok(())
}

fn migrate_v14_to_v15(conn: &Connection) -> Result<()> {
    // Two columns split out of v14 after a downstream Cowork recon revealed
    // that paid resources expose their offsite URL via the 301 Location
    // header on /resources/.../download (capture that as hub_external_url),
    // and that we need to distinguish how each pin was made for UI
    // transparency (hub_match_method: 'filename' | 'fuzzy_title' | 'manual').
    // These could not be folded back into v14 without breaking installs that
    // already applied the original 4-column v14 — adding them in a fresh
    // migration is the clean fix.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN hub_external_url TEXT;
        ALTER TABLE packages ADD COLUMN hub_match_method TEXT;
        "#,
    )?;
    Ok(())
}

fn migrate_v13_to_v14(conn: &Connection) -> Result<()> {
    // Hub sync v2: cache the hub's resource catalog (the sitemap-derived list
    // of `(resource_id, slug, lastmod)`) so the per-package sync can become
    // delta-driven instead of a full N-package crawl. With the catalog cached,
    // each subsequent sync touches only packages where (a) we have no pin yet
    // or (b) the hub's lastmod is newer than what we've enriched.
    //
    // The new packages.hub_* columns capture per-resource metadata extracted
    // from the search-result row (silver = category, green = hub-hosted,
    // license = third label) without requiring a per-resource page fetch.
    // All NULL-by-default so existing rows mean "never synced".
    //
    // `hub_category` already existed (v?); we leave it untouched and use it
    // as the deduped content-type axis (Looks, Scenes, Plugins, etc.) after
    // the silver-label billing prefix is stripped at scrape time.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN hub_billing_tier  TEXT;
        ALTER TABLE packages ADD COLUMN hub_is_hub_hosted INTEGER;
        ALTER TABLE packages ADD COLUMN hub_license       TEXT;
        ALTER TABLE packages ADD COLUMN hub_lastmod       INTEGER;

        CREATE INDEX IF NOT EXISTS idx_packages_hub_category
            ON packages(hub_category)
            WHERE hub_category IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_packages_hub_billing_tier
            ON packages(hub_billing_tier)
            WHERE hub_billing_tier IS NOT NULL;

        CREATE TABLE IF NOT EXISTS hub_resources (
            resource_id INTEGER PRIMARY KEY,
            slug        TEXT NOT NULL,
            lastmod     INTEGER NOT NULL,
            fetched_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_hub_resources_lastmod
            ON hub_resources(lastmod);
        "#,
    )?;
    Ok(())
}

fn migrate_v12_to_v13(conn: &Connection) -> Result<()> {
    // Local embedding pipeline storage. The v11 `package_family.embedding`
    // columns assumed one vector per family; the actual pipeline produces
    // N variants per family (model x input_kind), so move embeddings to
    // their own table keyed by (family_id, model, input_kind). The v11
    // columns are left in place but unused — harmless and avoids a
    // destructive ALTER on a populated table.
    //
    // input_kind today is one of "purpose" or "purpose-with-tags"; model
    // is the fastembed model name (e.g. "bge-small-en-v1.5"). dim is
    // stored alongside as a sanity check so a partial re-embed with the
    // wrong model can be caught at read time.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS family_embeddings (
            family_id   INTEGER NOT NULL,
            model       TEXT NOT NULL,
            input_kind  TEXT NOT NULL,
            embedding   BLOB NOT NULL,
            dim         INTEGER NOT NULL,
            embedded_at INTEGER NOT NULL,
            PRIMARY KEY (family_id, model, input_kind),
            FOREIGN KEY (family_id) REFERENCES package_family(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_family_embeddings_lookup
            ON family_embeddings(model, input_kind);
        "#,
    )?;
    Ok(())
}

fn migrate_v11_to_v12(conn: &Connection) -> Result<()> {
    // v4 taxonomy schema extensions. The `tag` column already carries the
    // full namespaced string (e.g. "kind:character-look"); these columns
    // give us namespace-keyed queries, cardinality + applies_to constraints
    // for UI / validation, and a deprecation flag so v3 entries can sit
    // dormant after v4 seeds without polluting the active set.
    //
    // applies_to_json holds either the string "any" or a JSON array of
    // kind: values; cardinality is one of "exactly-1" / "0-1" / "0-N".
    conn.execute_batch(
        r#"
        ALTER TABLE taxonomy ADD COLUMN namespace TEXT;
        ALTER TABLE taxonomy ADD COLUMN applies_to_json TEXT;
        ALTER TABLE taxonomy ADD COLUMN cardinality TEXT;
        ALTER TABLE taxonomy ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1;

        CREATE INDEX IF NOT EXISTS idx_taxonomy_namespace
            ON taxonomy(namespace);
        CREATE INDEX IF NOT EXISTS idx_taxonomy_active
            ON taxonomy(is_active)
            WHERE is_active = 1;
        "#,
    )?;
    Ok(())
}

fn migrate_v10_to_v11(conn: &Connection) -> Result<()> {
    // Package families group rows that share (creator, package_name) so a
    // "Skynet.Puppeteer.{1..8}" set is treated as ONE plugin instead of 8
    // inflated copies. Tagging / purpose / embeddings attach to the family,
    // not individual versions. The family's `latest_package_id` points at
    // the highest-version row, which drives prompt construction. Older
    // versions remain in `packages` (visible in the grid, addressable by
    // var_path) but don't drive tagging.
    //
    // `family_tags` mirrors the v8 `package_tags` shape but keyed by
    // family_id. Existing v3 tagging data on `packages` is preserved
    // unchanged; the `--recompute-families` CLI step copies it into the
    // family-level structure on first run.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS package_family (
            id                          INTEGER PRIMARY KEY AUTOINCREMENT,
            creator                     TEXT NOT NULL,
            package_name                TEXT NOT NULL,
            latest_package_id           INTEGER,
            purpose                     TEXT,
            out_of_scope                INTEGER NOT NULL DEFAULT 0,
            tagging_state               TEXT,
            tagging_model               TEXT,
            taxonomy_version            TEXT,
            tagged_at                   INTEGER,
            tagging_error               TEXT,
            tagging_suggested_new_tag   TEXT,
            tagging_notes               TEXT,
            embedding                   BLOB,
            embedding_model             TEXT,
            embedded_at                 INTEGER,
            UNIQUE(creator, package_name)
        );

        CREATE INDEX IF NOT EXISTS idx_family_state
            ON package_family(tagging_state);
        CREATE INDEX IF NOT EXISTS idx_family_suggested
            ON package_family(tagging_suggested_new_tag)
            WHERE tagging_suggested_new_tag IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_family_latest_pkg
            ON package_family(latest_package_id);

        CREATE TABLE IF NOT EXISTS family_tags (
            family_id INTEGER NOT NULL,
            tag       TEXT NOT NULL,
            PRIMARY KEY (family_id, tag),
            FOREIGN KEY (family_id) REFERENCES package_family(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_family_tags_tag ON family_tags(tag);

        ALTER TABLE packages ADD COLUMN family_id INTEGER REFERENCES package_family(id);
        CREATE INDEX IF NOT EXISTS idx_packages_family ON packages(family_id);
        "#,
    )?;
    Ok(())
}

fn migrate_v9_to_v10(conn: &Connection) -> Result<()> {
    // Resolved dependency edges. One row per (src package, raw dep key); dst is
    // the local packages row that key resolves to, or NULL when the user
    // doesn't have a matching package installed. Repopulated by the scanner
    // after each scan — see scanner::resolve_dep_links.
    //
    // idx on dst is for the reverse-dep query ("which packages depend on me").
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS package_dep_links (
            src_package_id INTEGER NOT NULL,
            dst_package_id INTEGER,
            raw_dep_key    TEXT NOT NULL,
            PRIMARY KEY (src_package_id, raw_dep_key),
            FOREIGN KEY (src_package_id) REFERENCES packages(id) ON DELETE CASCADE,
            FOREIGN KEY (dst_package_id) REFERENCES packages(id) ON DELETE SET NULL
        );
        CREATE INDEX IF NOT EXISTS idx_dep_links_dst ON package_dep_links(dst_package_id);
        "#,
    )?;
    Ok(())
}

fn migrate_v8_to_v9(conn: &Connection) -> Result<()> {
    // Capture two side-channel signals from each Grok response so the
    // taxonomy-evolution review pass has something to look at:
    //  - `tagging_suggested_new_tag`: kebab-case name Grok proposed when no
    //    existing tag fit. Aggregate across the corpus to drive v4 additions.
    //  - `tagging_notes`: free-text disambiguation note. Useful for
    //    debugging low-quality records or unusual classifications.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN tagging_suggested_new_tag TEXT;
        ALTER TABLE packages ADD COLUMN tagging_notes TEXT;
        CREATE INDEX IF NOT EXISTS idx_packages_suggested_new_tag
            ON packages(tagging_suggested_new_tag)
            WHERE tagging_suggested_new_tag IS NOT NULL;
        "#,
    )?;
    Ok(())
}

fn migrate_v7_to_v8(conn: &Connection) -> Result<()> {
    // LLM tagging + embedding pipeline schema. All columns NULL-by-default so
    // existing rows mean "not yet tagged/embedded". `package_tags` is the
    // normalized list of assigned tags (multi-row per package). `taxonomy` is
    // the self-describing tag catalog — seeded from tagging/taxonomy-v3.json
    // on first run by the tag-library binary. `tagging_runs` is per-run audit.
    //
    // Indexes:
    //  - package_tags(tag): fast "all packages with tag X" filter for UI
    //  - packages(tagging_state): fast resume queries for the batched runner
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN purpose          TEXT;
        ALTER TABLE packages ADD COLUMN out_of_scope     INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN tagging_state    TEXT;
        ALTER TABLE packages ADD COLUMN tagging_model    TEXT;
        ALTER TABLE packages ADD COLUMN taxonomy_version TEXT;
        ALTER TABLE packages ADD COLUMN tagged_at        INTEGER;
        ALTER TABLE packages ADD COLUMN tagging_error    TEXT;
        ALTER TABLE packages ADD COLUMN embedding        BLOB;
        ALTER TABLE packages ADD COLUMN embedding_model  TEXT;
        ALTER TABLE packages ADD COLUMN embedded_at      INTEGER;

        CREATE TABLE IF NOT EXISTS package_tags (
            package_id INTEGER NOT NULL,
            tag        TEXT NOT NULL,
            PRIMARY KEY (package_id, tag),
            FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_package_tags_tag ON package_tags(tag);

        CREATE TABLE IF NOT EXISTS taxonomy (
            tag                TEXT PRIMARY KEY,
            category           TEXT NOT NULL,
            description        TEXT NOT NULL,
            examples_json      TEXT NOT NULL,
            state              TEXT NOT NULL,
            reason_speculative TEXT,
            version_added      TEXT NOT NULL,
            created_at         INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_taxonomy_state    ON taxonomy(state);
        CREATE INDEX IF NOT EXISTS idx_taxonomy_category ON taxonomy(category);

        CREATE TABLE IF NOT EXISTS tagging_runs (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at        INTEGER NOT NULL,
            completed_at      INTEGER,
            taxonomy_version  TEXT NOT NULL,
            model             TEXT NOT NULL,
            total             INTEGER NOT NULL DEFAULT 0,
            succeeded         INTEGER NOT NULL DEFAULT 0,
            failed            INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_packages_tagging_state ON packages(tagging_state);
        "#,
    )?;
    Ok(())
}

fn migrate_v6_to_v7(conn: &Connection) -> Result<()> {
    // Index `instructions` from meta.json. Previously only pulled live in
    // get_package_detail; now also written by scanner so the LLM tagging
    // pipeline can read description + instructions without re-opening every
    // .var. Existing rows hold NULL until rescanned.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN instructions TEXT;
        "#,
    )?;
    Ok(())
}

fn migrate_v5_to_v6(conn: &Connection) -> Result<()> {
    // `package_mtime`: max last-modified timestamp across all entries inside the
    // .var (i.e. when the *author* zipped the package), distinct from `file_mtime`
    // which is when this machine last touched the .var on disk. Useful for
    // sort-by-release-date even after the file was re-downloaded or sync-touched.
    // Pre-existing rows keep 0 until rescanned.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN package_mtime INTEGER NOT NULL DEFAULT 0;
        "#,
    )?;
    Ok(())
}

fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    // Hub sync columns. Populated by the optional VaM Hub scrape, which
    // matches each local package to its hub.virtamate.com resource page.
    // `hub_sync_state` is one of: 'matched', 'not_found', 'failed', 'gate'.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN hub_resource_id INTEGER;
        ALTER TABLE packages ADD COLUMN hub_url         TEXT;
        ALTER TABLE packages ADD COLUMN hub_title       TEXT;
        ALTER TABLE packages ADD COLUMN hub_author      TEXT;
        ALTER TABLE packages ADD COLUMN hub_category    TEXT;
        ALTER TABLE packages ADD COLUMN hub_preview_url TEXT;
        ALTER TABLE packages ADD COLUMN hub_synced_at   INTEGER;
        ALTER TABLE packages ADD COLUMN hub_sync_state  TEXT;
        CREATE INDEX IF NOT EXISTS idx_packages_hub_category ON packages(hub_category);
        "#,
    )?;
    Ok(())
}

fn migrate_v0_to_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS app_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS packages (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            var_path        TEXT NOT NULL UNIQUE,
            file_size       INTEGER NOT NULL,
            file_mtime      INTEGER NOT NULL,
            creator         TEXT NOT NULL DEFAULT '',
            package_name    TEXT NOT NULL DEFAULT '',
            version         TEXT NOT NULL DEFAULT '',
            license         TEXT,
            program_version TEXT,
            description     TEXT,
            package_type    TEXT NOT NULL DEFAULT 'Unknown',
            content_count   INTEGER NOT NULL DEFAULT 0,
            dep_count       INTEGER NOT NULL DEFAULT 0,
            has_preview     INTEGER NOT NULL DEFAULT 0,
            scanned_at      INTEGER NOT NULL,
            error           TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_packages_creator      ON packages(creator);
        CREATE INDEX IF NOT EXISTS idx_packages_type         ON packages(package_type);
        CREATE INDEX IF NOT EXISTS idx_packages_package_name ON packages(package_name);

        CREATE TABLE IF NOT EXISTS package_dependencies (
            package_id INTEGER NOT NULL,
            dep_key    TEXT NOT NULL,
            PRIMARY KEY (package_id, dep_key),
            FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
        );
        "#,
    )?;
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    // Preview picker output (chosen zip entry path) — read by the thumbnail extractor.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN preview_path TEXT;
        ALTER TABLE packages ADD COLUMN preview_item_count INTEGER NOT NULL DEFAULT 0;
        "#,
    )?;
    Ok(())
}

fn migrate_v3_to_v4(conn: &Connection) -> Result<()> {
    // User-set flags for triage workflow. Hidden packages are excluded by default
    // from the grid; favorites can be filtered to. Indexes are small wins at our
    // scale but keep "favorites only" / "include hidden" filters O(log n).
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN is_favorite INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN is_hidden   INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX IF NOT EXISTS idx_packages_favorite ON packages(is_favorite) WHERE is_favorite = 1;
        CREATE INDEX IF NOT EXISTS idx_packages_hidden   ON packages(is_hidden)   WHERE is_hidden = 1;
        "#,
    )?;
    Ok(())
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    // Per-category previewable item counts — drive the emoji+count strip in tiles.
    // `preview_item_count` from v2 is left in place but no longer written by the scanner.
    conn.execute_batch(
        r#"
        ALTER TABLE packages ADD COLUMN scene_count    INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN look_count     INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN plugin_count   INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN clothing_count INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN hair_count     INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN pose_count     INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE packages ADD COLUMN subscene_count INTEGER NOT NULL DEFAULT 0;
        "#,
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM app_settings WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, String>(0)?))
    } else {
        Ok(None)
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_settings(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}
