//! Production tagging binary. Reads packages from the SQLite index, calls
//! the Grok API in batches, writes tag assignments back. First-run setup:
//! `tag_library --set-api-key <key>` to seed the API key into app_settings;
//! `--seed-taxonomy <path>` to load tagging/taxonomy-v3.json into the
//! taxonomy table (only inserts; does not update existing rows).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use vam_package_browser_lib::tagging::{family, grok::GrokClient, runner, seeder};
use vam_package_browser_lib::{index, tagging::prompt};

const SETTING_API_KEY: &str = "xai_api_key";

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    set_api_key: Option<String>,
    seed_taxonomy: Option<PathBuf>,
    taxonomy_version: Option<String>,
    model: Option<String>,
    batch_size: Option<usize>,
    rate_limit_ms: Option<u64>,
    limit: Option<usize>,
    only_ids: Option<PathBuf>,
    dry_run: bool,
    show_status: bool,
    show_recent: Option<usize>,
    show_summary: bool,
    show_suggested: Option<String>,
    show_tag: Option<String>,
    recompute_families: bool,
    help: bool,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    if args.help {
        print_help();
        return Ok(());
    }

    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!(
            "index db not found at {}\n(run a scan from the GUI first, or pass --db)",
            db_path.display()
        ));
    }
    let conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;
    let conn = Arc::new(Mutex::new(conn));

    if let Some(key) = args.set_api_key {
        index::set_setting(&conn.lock(), SETTING_API_KEY, key.trim())?;
        eprintln!("api key stored in app_settings (len={})", key.trim().len());
        return Ok(());
    }

    if let Some(path) = args.seed_taxonomy.as_ref() {
        let version_label = args
            .taxonomy_version
            .clone()
            .unwrap_or_else(|| "v4".to_string());
        // Auto-detect: file content drives v3 vs v4 dispatch. v4 prints
        // namespace stats, v3 prints the legacy category breakdown.
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read taxonomy file {}", path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("parse taxonomy JSON {}", path.display()))?;
        if json.get("namespaces").is_some() {
            let file: seeder::V4TaxonomyFile = serde_json::from_value(json)
                .with_context(|| format!("parse v4 taxonomy {}", path.display()))?;
            let stats = seeder::seed_v4_from_parsed(&conn.lock(), &file, &version_label)
                .with_context(|| format!("seed v4 taxonomy from {}", path.display()))?;
            eprintln!(
                "v4 seed ({}): {} namespaces; tags added {}, reactivated {}, updated {}, deprecated {} (active total {})",
                version_label,
                stats.namespaces,
                stats.tags_added,
                stats.tags_reactivated,
                stats.tags_updated,
                stats.tags_deprecated,
                stats.total_active()
            );
        } else {
            let stats = seeder::seed_from_file(&conn.lock(), path, &version_label)
                .with_context(|| format!("seed taxonomy from {}", path.display()))?;
            eprintln!(
                "v3 seed: utility +{}/={}, location +{}/={}, speculative +{}/={} (total added {}, existing {})",
                stats.utility_added,
                stats.utility_existing,
                stats.location_added,
                stats.location_existing,
                stats.speculative_added,
                stats.speculative_existing,
                stats.total_added(),
                stats.total_existing()
            );
        }
        // Setup action — exit so we don't fall through into a tagging run
        // before the user has confirmed they want one (and before the API
        // key has been set, etc.).
        return Ok(());
    }

    if args.show_status {
        print_status(&conn.lock())?;
        return Ok(());
    }

    if let Some(n) = args.show_recent {
        print_recent(&conn.lock(), n)?;
        return Ok(());
    }

    if args.show_summary {
        print_summary(&conn.lock())?;
        return Ok(());
    }

    if let Some(t) = args.show_suggested.as_deref() {
        print_by_suggested(&conn.lock(), t)?;
        return Ok(());
    }

    if let Some(t) = args.show_tag.as_deref() {
        print_by_tag(&conn.lock(), t)?;
        return Ok(());
    }

    if args.recompute_families {
        let stats = family::recompute(&conn.lock())?;
        eprintln!("Families:");
        eprintln!("  before:                   {}", stats.families_before);
        eprintln!("  after:                    {}", stats.families_after);
        eprintln!("  added this run:           {}", stats.families_added);
        eprintln!("  packages linked this run: {}", stats.packages_linked_this_run);
        eprintln!("  with a latest_package:    {}", stats.families_with_latest);
        eprintln!("  inherited v3 tag state:   {}", stats.families_inheriting_tags);
        eprintln!("  family_tags rows added:   {}", stats.family_tag_rows_added);
        return Ok(());
    }

    // Default action: run the tagger. Bail if taxonomy is empty (likely
    // forgot to --seed-taxonomy).
    let taxonomy_count: i64 = conn
        .lock()
        .query_row("SELECT COUNT(*) FROM taxonomy", [], |r| r.get(0))?;
    if taxonomy_count == 0 {
        return Err(anyhow!(
            "taxonomy table is empty — run with --seed-taxonomy <path> first"
        ));
    }

    let cfg = runner::RunnerConfig {
        taxonomy_version: args
            .taxonomy_version
            .clone()
            .unwrap_or_else(|| "v4".to_string()),
        model: args.model.clone().unwrap_or_else(|| "grok-4.3".to_string()),
        batch_size: args.batch_size.unwrap_or(100),
        rate_limit_ms: args.rate_limit_ms.unwrap_or(1000),
        limit: args.limit,
        only_ids: load_only_ids(args.only_ids.as_deref())?,
        dry_run: args.dry_run,
    };

    let client = if args.dry_run {
        None
    } else {
        let api_key = index::get_setting(&conn.lock(), SETTING_API_KEY)?
            .ok_or_else(|| {
                anyhow!(
                    "no api key in app_settings; run with --set-api-key <key> first \
                     (or pass --dry-run to skip API calls)"
                )
            })?;
        Some(GrokClient::new(api_key, cfg.model.clone()))
    };

    eprintln!(
        "config: model={} taxonomy_version={} batch_size={} rate_limit_ms={} dry_run={} prompt_version={}",
        cfg.model, cfg.taxonomy_version, cfg.batch_size, cfg.rate_limit_ms, cfg.dry_run, prompt::PROMPT_VERSION
    );
    let stats = runner::run(&conn, client.as_ref(), &cfg)?;
    eprintln!(
        "\n=== run complete: batches={} sent={} done={} failed={} prompt_tokens={} completion_tokens={} ===",
        stats.batches,
        stats.records_sent,
        stats.records_done,
        stats.records_failed,
        stats.prompt_tokens,
        stats.completion_tokens
    );
    Ok(())
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let mut a = Args::default();
    let mut i = 1;
    while i < raw.len() {
        let arg = raw[i].as_str();
        match arg {
            "--db" => {
                i += 1;
                a.db = Some(PathBuf::from(needs_value(&raw, i, "--db")?));
            }
            "--set-api-key" => {
                i += 1;
                a.set_api_key = Some(needs_value(&raw, i, "--set-api-key")?.to_string());
            }
            "--seed-taxonomy" => {
                i += 1;
                a.seed_taxonomy = Some(PathBuf::from(needs_value(&raw, i, "--seed-taxonomy")?));
            }
            "--taxonomy-version" => {
                i += 1;
                a.taxonomy_version = Some(needs_value(&raw, i, "--taxonomy-version")?.to_string());
            }
            "--model" => {
                i += 1;
                a.model = Some(needs_value(&raw, i, "--model")?.to_string());
            }
            "--batch-size" => {
                i += 1;
                a.batch_size = Some(
                    needs_value(&raw, i, "--batch-size")?
                        .parse()
                        .context("--batch-size must be a number")?,
                );
            }
            "--rate-limit-ms" => {
                i += 1;
                a.rate_limit_ms = Some(
                    needs_value(&raw, i, "--rate-limit-ms")?
                        .parse()
                        .context("--rate-limit-ms must be a number")?,
                );
            }
            "--limit" => {
                i += 1;
                a.limit = Some(
                    needs_value(&raw, i, "--limit")?
                        .parse()
                        .context("--limit must be a number")?,
                );
            }
            "--only-ids" => {
                i += 1;
                a.only_ids = Some(PathBuf::from(needs_value(&raw, i, "--only-ids")?));
            }
            "--dry-run" => a.dry_run = true,
            "--status" => a.show_status = true,
            "--summary" => a.show_summary = true,
            "--show-recent" => {
                i += 1;
                a.show_recent = Some(
                    needs_value(&raw, i, "--show-recent")?
                        .parse()
                        .context("--show-recent must be a number")?,
                );
            }
            "--show-suggested" => {
                i += 1;
                a.show_suggested = Some(needs_value(&raw, i, "--show-suggested")?.to_string());
            }
            "--show-tag" => {
                i += 1;
                a.show_tag = Some(needs_value(&raw, i, "--show-tag")?.to_string());
            }
            "--recompute-families" => a.recompute_families = true,
            "--help" | "-h" => a.help = true,
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

fn needs_value<'a>(raw: &'a [String], i: usize, flag: &str) -> Result<&'a str> {
    raw.get(i)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn default_db_path() -> PathBuf {
    vam_package_browser_lib::paths::default_db_path()
}

/// Load explicit ids from a file. Accepts either:
///   - JSONL with `id` field per line (e.g. tagging/pilot/sample-v3.jsonl)
///   - plain text with one integer per line
fn load_only_ids(path: Option<&Path>) -> Result<Option<Vec<i64>>> {
    let Some(p) = path else { return Ok(None) };
    let content = std::fs::read_to_string(p)
        .with_context(|| format!("read --only-ids {}", p.display()))?;
    let mut ids = Vec::new();
    for (ln, line) in content.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('{') {
            let v: serde_json::Value = serde_json::from_str(t)
                .with_context(|| format!("parse line {} of {}", ln + 1, p.display()))?;
            let id = v.get("id").and_then(|x| x.as_i64()).ok_or_else(|| {
                anyhow!("missing id field on line {} of {}", ln + 1, p.display())
            })?;
            ids.push(id);
        } else {
            let id: i64 = t.parse().with_context(|| {
                format!("parse id on line {} of {}", ln + 1, p.display())
            })?;
            ids.push(id);
        }
    }
    Ok(Some(ids))
}

fn print_status(conn: &rusqlite::Connection) -> Result<()> {
    let taxonomy_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM taxonomy", [], |r| r.get(0))?;
    let taxonomy_active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM taxonomy WHERE is_active = 1",
        [],
        |r| r.get(0),
    )?;
    let taxonomy_deprecated: i64 = conn.query_row(
        "SELECT COUNT(*) FROM taxonomy WHERE is_active = 0",
        [],
        |r| r.get(0),
    )?;
    let taxonomy_namespaces: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT namespace) FROM taxonomy WHERE is_active = 1 AND namespace IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let pkg_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM packages WHERE error IS NULL", [], |r| {
            r.get(0)
        })?;
    let pkg_with_family: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages WHERE family_id IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let family_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM package_family", [], |r| r.get(0))?;
    let family_pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state IS NULL",
        [],
        |r| r.get(0),
    )?;
    let family_done: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state='done'",
        [],
        |r| r.get(0),
    )?;
    let family_failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state='failed'",
        [],
        |r| r.get(0),
    )?;
    let family_oos: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE out_of_scope = 1",
        [],
        |r| r.get(0),
    )?;
    let family_tag_assignments: i64 =
        conn.query_row("SELECT COUNT(*) FROM family_tags", [], |r| r.get(0))?;
    let api_key_present: bool = index::get_setting(conn, SETTING_API_KEY)?.is_some();

    let mut all_settings: Vec<(String, usize)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT key, length(value) FROM app_settings ORDER BY key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
        })?;
        for row in rows {
            all_settings.push(row?);
        }
    }

    println!("Taxonomy:");
    println!("  total tags:        {taxonomy_total}");
    println!("  active:            {taxonomy_active}  (across {taxonomy_namespaces} namespaces)");
    println!("  deprecated:        {taxonomy_deprecated}  (legacy v3 entries after v4 seed)");
    println!();
    println!("Packages (error IS NULL):");
    println!("  total:             {pkg_total}");
    println!("  linked to family:  {pkg_with_family}");
    println!();
    println!("Package families (tagging unit):");
    println!("  total:             {family_total}");
    println!("  pending tagging:   {family_pending}");
    println!("  tagged (done):     {family_done}");
    println!("  tagged (failed):   {family_failed}");
    println!("  out_of_scope:      {family_oos}");
    println!();
    println!("Family tag assignments: {family_tag_assignments}");
    println!("API key configured:     {api_key_present}");
    println!();
    println!("app_settings rows:");
    if all_settings.is_empty() {
        println!("  (none)");
    } else {
        for (k, vlen) in all_settings {
            println!("  {k}  (value length: {vlen})");
        }
    }
    Ok(())
}

type DisplayRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

/// Print a list of DisplayRow tuples in the standard QA format, fetching
/// per-family tags inline. Shared by --show-recent / --show-suggested /
/// --show-tag. `id` is a family_id and tags come from `family_tags`.
fn print_rows(conn: &rusqlite::Connection, rows: Vec<DisplayRow>) -> Result<()> {
    if rows.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    let mut tag_stmt =
        conn.prepare("SELECT tag FROM family_tags WHERE family_id = ?1 ORDER BY tag")?;
    for (id, creator, name, version, ptype, purpose, oos, suggested, notes) in rows {
        let tags: Vec<String> = tag_stmt
            .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let tags_str = if tags.is_empty() {
            "(none)".to_string()
        } else {
            tags.join(", ")
        };
        println!("--- id {id} ---");
        println!("  pkg:       {creator}.{name}.{version}  [{ptype}]");
        println!("  oos:       {}", oos != 0);
        println!("  tags:      {tags_str}");
        if let Some(p) = purpose {
            println!("  purpose:   {p}");
        }
        if let Some(s) = suggested {
            println!("  suggested: {s}");
        }
        if let Some(n) = notes {
            println!("  notes:     {n}");
        }
        println!();
    }
    Ok(())
}

/// SELECT list for the family-level display queries. `id` is `package_family.id`;
/// version + package_type come from the joined latest package row (LEFT JOIN so
/// orphaned families without a resolved latest still render — degenerate but
/// shouldn't occur post-recompute).
const FAMILY_COLUMNS: &str = "pf.id, pf.creator, pf.package_name,
                              COALESCE(p.version, '?'),
                              COALESCE(p.package_type, '?'),
                              pf.purpose, pf.out_of_scope,
                              pf.tagging_suggested_new_tag, pf.tagging_notes";

fn map_pkg_row(r: &rusqlite::Row) -> rusqlite::Result<DisplayRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

/// Print the N most recently tagged families with their tags + purpose.
/// For quick eyeball QA after a small batch run.
fn print_recent(conn: &rusqlite::Connection, n: usize) -> Result<()> {
    let sql = format!(
        "SELECT {FAMILY_COLUMNS}
           FROM package_family pf
           LEFT JOIN packages p ON p.id = pf.latest_package_id
          WHERE pf.tagging_state = 'done'
          ORDER BY pf.tagged_at DESC, pf.id DESC
          LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<DisplayRow> = stmt
        .query_map(rusqlite::params![n as i64], map_pkg_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    print_rows(conn, rows)
}

/// Show all families whose `tagging_suggested_new_tag` matches the given
/// name. Used to inspect taxonomy-gap candidates surfaced by Grok:
///   tag_library --show-suggested collider-configuration
fn print_by_suggested(conn: &rusqlite::Connection, suggested: &str) -> Result<()> {
    let sql = format!(
        "SELECT {FAMILY_COLUMNS}
           FROM package_family pf
           LEFT JOIN packages p ON p.id = pf.latest_package_id
          WHERE pf.tagging_suggested_new_tag = ?1
          ORDER BY pf.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<DisplayRow> = stmt
        .query_map(rusqlite::params![suggested], map_pkg_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    print_rows(conn, rows)
}

/// Show all families tagged with the given existing taxonomy tag. Used for
/// overlap comparisons (e.g. "what's currently in pose-ik-helper" while
/// deciding whether to split off bone-control).
fn print_by_tag(conn: &rusqlite::Connection, tag: &str) -> Result<()> {
    let sql = format!(
        "SELECT {FAMILY_COLUMNS}
           FROM package_family pf
           LEFT JOIN packages p ON p.id = pf.latest_package_id
           JOIN family_tags ft ON ft.family_id = pf.id
          WHERE ft.tag = ?1
          ORDER BY pf.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<DisplayRow> = stmt
        .query_map(rusqlite::params![tag], map_pkg_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    print_rows(conn, rows)
}

/// Cross-cutting analysis of all tagged families: in-scope vs OOS counts,
/// top tags by frequency, taxonomy-evolution candidates (suggested_new_tag
/// proposals aggregated by name + count). Operates at the family level
/// (post-v11) so version-inflated plugins count once.
fn print_summary(conn: &rusqlite::Connection) -> Result<()> {
    let total_tagged: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state = 'done'",
        [],
        |r| r.get(0),
    )?;
    let in_scope: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state = 'done' AND out_of_scope = 0",
        [],
        |r| r.get(0),
    )?;
    let out_of_scope: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE tagging_state = 'done' AND out_of_scope = 1",
        [],
        |r| r.get(0),
    )?;
    let with_tags: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT family_id) FROM family_tags",
        [],
        |r| r.get(0),
    )?;
    let untagged_in_scope: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family pf
          WHERE pf.tagging_state = 'done'
            AND pf.out_of_scope = 0
            AND NOT EXISTS (SELECT 1 FROM family_tags t WHERE t.family_id = pf.id)",
        [],
        |r| r.get(0),
    )?;

    println!("Tagged: {total_tagged} families");
    println!("  in-scope:           {in_scope}");
    println!("  out-of-scope:       {out_of_scope}");
    println!("  with >=1 tag:       {with_tags}");
    println!("  in-scope, no tag:   {untagged_in_scope}  (suggested_new_tag candidates)");
    println!();

    println!("Tag frequency (top 30):");
    let mut stmt = conn.prepare(
        "SELECT t.tag,
                COUNT(*) AS n,
                COALESCE(tx.state, 'unknown') AS state
           FROM family_tags t
           LEFT JOIN taxonomy tx ON tx.tag = t.tag
          GROUP BY t.tag
          ORDER BY n DESC, t.tag ASC
          LIMIT 30",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (tag, n, state) = row?;
        let marker = if state == "speculative" { " [SPEC->graduate]" } else { "" };
        println!("  {n:>4}  {tag}{marker}");
    }
    println!();

    println!("Suggested new tags (proposed >=1 time):");
    let mut stmt = conn.prepare(
        "SELECT tagging_suggested_new_tag, COUNT(*) AS n
           FROM package_family
          WHERE tagging_suggested_new_tag IS NOT NULL
          GROUP BY tagging_suggested_new_tag
          ORDER BY n DESC, tagging_suggested_new_tag ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut any = false;
    for row in rows {
        let (tag, n) = row?;
        println!("  {n:>4}  {tag}");
        any = true;
    }
    if !any {
        println!("  (none)");
    }
    Ok(())
}

fn print_help() {
    eprintln!("Usage: tag_library [flags]");
    eprintln!();
    eprintln!("Tags packages by calling the Grok API in batches and writing back to SQLite.");
    eprintln!();
    eprintln!("Setup:");
    eprintln!("  --set-api-key <key>          Store xAI API key in app_settings and exit");
    eprintln!("  --seed-taxonomy <path>       Load taxonomy JSON into the taxonomy table");
    eprintln!("                               (idempotent: INSERT OR IGNORE per tag)");
    eprintln!("  --recompute-families         Reconcile package_family from packages — links");
    eprintln!("                               versions to families, picks latest, inherits");
    eprintln!("                               existing tag data on first run. Idempotent.");
    eprintln!();
    eprintln!("Run:");
    eprintln!("  --taxonomy-version <v>       Version label written to each row (default: v3)");
    eprintln!("  --model <name>               Grok model id (default: grok-4.3)");
    eprintln!("  --batch-size <n>             Records per API call (default: 100)");
    eprintln!("  --rate-limit-ms <n>          Sleep between batches (default: 1000)");
    eprintln!("  --limit <n>                  Cap total records this run (e.g. for pilots)");
    eprintln!("  --only-ids <path>            Restrict to ids in JSONL or text file");
    eprintln!("  --dry-run                    Build batches, log what would be sent, no API call");
    eprintln!();
    eprintln!("Other:");
    eprintln!("  --db <path>                  Override SQLite path");
    eprintln!("  --status                     Print taxonomy + tagging stats and exit");
    eprintln!("  --summary                    Tag frequency + OOS counts + suggested_new_tag");
    eprintln!("  --show-recent <n>            Show last N tagged packages (id, tags, purpose)");
    eprintln!("  --show-suggested <tag>       Show all packages with that suggested_new_tag");
    eprintln!("  --show-tag <tag>             Show all packages tagged with that existing tag");
    eprintln!("  --help, -h                   This message");
}
