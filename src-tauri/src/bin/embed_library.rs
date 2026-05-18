//! Local embedding pipeline binary. Encodes `package_family.purpose`
//! into vector embeddings stored in `family_embeddings`. Pure-local —
//! fastembed runs ONNX models on CPU, with a one-time model download
//! to the system HuggingFace cache on first use.
//!
//! Companion to `tag_library`: tagging gives discrete filters,
//! embeddings give fuzzy semantic match.
//!
//! Common invocations:
//!   embed_library --status
//!   embed_library --embed-all
//!   embed_library --embed-all --models bge --inputs purpose --limit 50
//!   embed_library --search "fix clothing tightness"
//!   embed_library --similar-to 123
//!   embed_library --re-embed --models bge --inputs purpose

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use vam_package_browser_lib::embedding::{runner, search, storage, InputKind, ModelChoice};
use vam_package_browser_lib::index;

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    models: Option<Vec<ModelChoice>>,
    inputs: Option<Vec<InputKind>>,
    limit: Option<usize>,
    batch_size: Option<usize>,
    top_n: Option<usize>,
    embed_all: bool,
    re_embed: bool,
    search_query: Option<String>,
    compare_search: Option<String>,
    similar_to: Option<i64>,
    show_status: bool,
    integrity_check: bool,
    recover_from: Option<PathBuf>,
    recover_to: Option<PathBuf>,
    search_model: Option<ModelChoice>,
    search_input: Option<InputKind>,
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

    // --integrity-check runs against a fully read-only handle and skips
    // open_and_migrate (which would write pragmas + run v13 CREATE TABLE,
    // both unsafe on a sick DB). Dispatch early before we touch the file
    // for any other reason.
    if args.integrity_check {
        return run_integrity_check(&db_path);
    }

    // --recover-from creates a fresh recovered DB from a (likely corrupt)
    // source. Doesn't touch the live DB. Dispatched before any normal
    // open_and_migrate path.
    if let Some(src) = args.recover_from.clone() {
        let dst = args.recover_to.clone().unwrap_or_else(|| {
            let mut p = src.clone();
            let name = p
                .file_name()
                .map(|f| format!("{}.recovered", f.to_string_lossy()))
                .unwrap_or_else(|| "recovered.sqlite".to_string());
            p.set_file_name(name);
            p
        });
        return run_recovery(&src, &dst);
    }

    let conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;
    let conn = Arc::new(Mutex::new(conn));

    let models = args.models.clone().unwrap_or_else(|| ModelChoice::all().to_vec());
    let inputs = args.inputs.clone().unwrap_or_else(|| InputKind::all().to_vec());
    let batch_size = args.batch_size.unwrap_or(32);
    let top_n = args.top_n.unwrap_or(20);

    if args.show_status {
        print_status(&conn.lock())?;
        return Ok(());
    }

    if let Some(query) = args.search_query.as_deref() {
        let model = args.search_model.unwrap_or(ModelChoice::BgeSmallEnV15);
        let input = args.search_input.unwrap_or(InputKind::Purpose);
        let hits = search::search_text(&conn.lock(), model, input, query, top_n)?;
        print_hits(&hits, &format!("text '{query}'"), model, input);
        return Ok(());
    }

    if let Some(query) = args.compare_search.as_deref() {
        run_compare_search(&conn.lock(), query, top_n)?;
        return Ok(());
    }

    if let Some(family_id) = args.similar_to {
        let model = args.search_model.unwrap_or(ModelChoice::BgeSmallEnV15);
        let input = args.search_input.unwrap_or(InputKind::Purpose);
        let hits = search::search_similar_to_family(
            &conn.lock(),
            model,
            input,
            family_id,
            top_n,
        )?;
        print_hits(&hits, &format!("family_id={family_id}"), model, input);
        return Ok(());
    }

    if args.re_embed {
        // Scope the wipe to the selected variants — `--re-embed --models bge`
        // only clears BGE rows, leaving nomic intact.
        let mut total = 0usize;
        for &m in &models {
            for &k in &inputs {
                let n = storage::clear(
                    &conn.lock(),
                    Some(m.name()),
                    Some(k.name()),
                )?;
                eprintln!("[re-embed] cleared {n} rows for {} / {}", m.name(), k.name());
                total += n;
            }
        }
        eprintln!("[re-embed] {total} rows cleared total; will re-encode below");
    }

    if args.embed_all || args.re_embed {
        for &m in &models {
            for &k in &inputs {
                let stats = runner::embed_missing(&conn, m, k, args.limit, batch_size)?;
                eprintln!(
                    "[embed] {} / {} done: {} embedded ({} candidates, {} skipped, {:.1}s)",
                    stats.model,
                    stats.input_kind,
                    stats.embedded,
                    stats.candidates,
                    stats.skipped_empty,
                    stats.elapsed_secs
                );
            }
        }
        print_status(&conn.lock())?;
        return Ok(());
    }

    print_help();
    Err(anyhow!(
        "no action specified — pass --status, --embed-all, --search, --similar-to, or --re-embed"
    ))
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
            "--models" => {
                i += 1;
                let v = needs_value(&raw, i, "--models")?;
                a.models = Some(parse_models(v)?);
            }
            "--inputs" => {
                i += 1;
                let v = needs_value(&raw, i, "--inputs")?;
                a.inputs = Some(parse_inputs(v)?);
            }
            "--limit" => {
                i += 1;
                a.limit = Some(
                    needs_value(&raw, i, "--limit")?
                        .parse()
                        .context("--limit must be a number")?,
                );
            }
            "--batch-size" => {
                i += 1;
                a.batch_size = Some(
                    needs_value(&raw, i, "--batch-size")?
                        .parse()
                        .context("--batch-size must be a number")?,
                );
            }
            "--top-n" => {
                i += 1;
                a.top_n = Some(
                    needs_value(&raw, i, "--top-n")?
                        .parse()
                        .context("--top-n must be a number")?,
                );
            }
            "--embed-all" => a.embed_all = true,
            "--re-embed" => a.re_embed = true,
            "--status" => a.show_status = true,
            "--integrity-check" => a.integrity_check = true,
            "--recover-from" => {
                i += 1;
                a.recover_from = Some(PathBuf::from(needs_value(&raw, i, "--recover-from")?));
            }
            "--recover-to" => {
                i += 1;
                a.recover_to = Some(PathBuf::from(needs_value(&raw, i, "--recover-to")?));
            }
            "--search" => {
                i += 1;
                a.search_query = Some(needs_value(&raw, i, "--search")?.to_string());
            }
            "--compare-search" => {
                i += 1;
                a.compare_search = Some(needs_value(&raw, i, "--compare-search")?.to_string());
            }
            "--similar-to" => {
                i += 1;
                a.similar_to = Some(
                    needs_value(&raw, i, "--similar-to")?
                        .parse()
                        .context("--similar-to must be a family_id (integer)")?,
                );
            }
            "--model" => {
                i += 1;
                a.search_model =
                    Some(ModelChoice::parse(needs_value(&raw, i, "--model")?)?);
            }
            "--input" => {
                i += 1;
                a.search_input =
                    Some(InputKind::parse(needs_value(&raw, i, "--input")?)?);
            }
            "--help" | "-h" => a.help = true,
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    Ok(a)
}

fn parse_models(s: &str) -> Result<Vec<ModelChoice>> {
    s.split(',')
        .map(|p| ModelChoice::parse(p.trim()))
        .collect()
}

fn parse_inputs(s: &str) -> Result<Vec<InputKind>> {
    s.split(',').map(|p| InputKind::parse(p.trim())).collect()
}

fn needs_value<'a>(raw: &'a [String], i: usize, flag: &str) -> Result<&'a str> {
    raw.get(i)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn default_db_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("com.github.kylinblue.vam-package-browser")
        .join("index.sqlite")
}

fn print_status(conn: &rusqlite::Connection) -> Result<()> {
    let variants = runner::status(conn)?;
    let eligible = variants.first().map(|v| v.total_eligible).unwrap_or(0);

    let family_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM package_family", [], |r| r.get(0))?;
    let family_with_purpose: i64 = eligible;

    println!("Families:");
    println!("  total:                  {family_total}");
    println!("  with non-empty purpose: {family_with_purpose}  (the embedding-eligible denominator)");
    println!();
    println!("Embeddings by (model, input_kind):");
    for v in &variants {
        let pct = if v.total_eligible > 0 {
            (v.embedded as f64) / (v.total_eligible as f64) * 100.0
        } else {
            0.0
        };
        println!(
            "  {:>22} / {:<18}  {:>5} / {:<5}  ({:>5.1}%)",
            v.model, v.input_kind, v.embedded, v.total_eligible, pct
        );
    }
    Ok(())
}

fn print_hits(
    hits: &[search::SearchHit],
    label: &str,
    model: ModelChoice,
    input: InputKind,
) {
    println!(
        "Top {} by cosine for {} (model={}, input={}):",
        hits.len(),
        label,
        model.name(),
        input.name()
    );
    if hits.is_empty() {
        println!("  (no results)");
        return;
    }
    for (rank, h) in hits.iter().enumerate() {
        let purpose = h
            .purpose
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        println!(
            "  {:>2}. [{:>6.4}] family_id={:<5} {}.{}",
            rank + 1,
            h.score,
            h.family_id,
            h.creator,
            h.package_name
        );
        if !purpose.is_empty() {
            println!("       {purpose}");
        }
    }
}

/// Row-by-rowid copy for tables whose btree has localized damage.
/// Returns `(rows_copied, rows_failed)`. We:
///   1. Read MIN(rowid)..MAX(rowid) once — this is a cheap two-page
///      lookup, doesn't traverse the whole tree.
///   2. For each rowid in that range, run a single-row INSERT...SELECT.
///      Bad rowids surface as a malformed-DB error on that one stmt and
///      we increment the failure counter; good rowids copy normally.
///
/// This works because SQLite's malformed-page errors are scoped to the
/// specific page being read, not the whole table — picking rows by
/// rowid via the btree's interior walk skips around the broken sections.
fn copy_rowids_one_by_one(
    target: &Connection,
    table: &str,
) -> Result<(usize, usize)> {
    let (min_id, max_id): (Option<i64>, Option<i64>) = target.query_row(
        &format!("SELECT MIN(rowid), MAX(rowid) FROM src.{table}"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (min_id, max_id) = match (min_id, max_id) {
        (Some(a), Some(b)) => (a, b),
        _ => return Ok((0, 0)),
    };

    let copy_sql = format!(
        "INSERT OR IGNORE INTO main.{table} SELECT * FROM src.{table} WHERE rowid = ?1"
    );
    let mut stmt = target.prepare(&copy_sql)?;
    let mut ok = 0usize;
    let mut fail = 0usize;
    // Wrap the whole loop in a transaction so insert overhead is one
    // commit, not 4000.
    let tx = target.unchecked_transaction()?;
    for rid in min_id..=max_id {
        match stmt.execute([rid]) {
            Ok(n) => ok += n,
            Err(_) => fail += 1,
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok((ok, fail))
}

/// Salvage from a corrupt source DB into a fresh target. Strategy:
///   1. Refuse if target already exists — never silently overwrite.
///   2. Create the target via `index::open_and_migrate` so it gets the
///      full canonical schema (v1-v12 baseline + v13 family_embeddings).
///      All indexes are built from scratch on clean data.
///   3. Open source URI as `mode=ro&immutable=1` so SQLite never tries
///      to replay the (potentially corrupt) WAL. Attach onto the target
///      connection as schema `src`.
///   4. Copy data table-by-table. The corrupt indexes in source don't
///      block this because a `SELECT *` against a sequential table scan
///      doesn't traverse indexes.
///   5. Skip tables the scanner/tagger rebuilds naturally:
///      - package_dependencies: btree IS one of the corrupt trees; the
///        scanner re-emits it on next scan anyway.
///      - package_dep_links: 0 rows in source; rebuilt by scanner.
///      - family_tags / package_tags: 0 rows in source; rebuilt by
///        re-tagging.
///      - tagging_runs: empty stub row; not worth carrying.
///   6. Print before/after row counts so the user can sanity-check.
fn run_recovery(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    if !source.exists() {
        return Err(anyhow!("source DB not found: {}", source.display()));
    }
    if target.exists() {
        return Err(anyhow!(
            "target DB already exists; refusing to overwrite: {}\n(delete it first if you really want to redo)",
            target.display()
        ));
    }

    println!("recovery:");
    println!("  source: {}", source.display());
    println!("  target: {}", target.display());
    println!();

    // 1. Build a clean target with the canonical schema.
    let target_conn = index::open_and_migrate(target)
        .with_context(|| format!("create + migrate target {}", target.display()))?;

    // 2. Attach source read-only-immutable.
    let src_uri = format!(
        "file:{}?mode=ro&immutable=1",
        source.display().to_string().replace('\\', "/")
    );
    target_conn
        .execute(&format!("ATTACH DATABASE '{src_uri}' AS src"), [])
        .with_context(|| "attach source as 'src'")?;

    // 3. Disable FKs during bulk copy so we don't have to perfectly
    //    order tables — we verify with `PRAGMA foreign_key_check` after
    //    copy, which catches anything we left dangling.
    target_conn.pragma_update(None, "foreign_keys", "OFF")?;

    // 4. Tables to salvage. Order picked to satisfy FKs even with
    //    foreign_keys=ON (defensive): `package_family` before `packages`
    //    (packages.family_id references it), `taxonomy` standalone,
    //    `app_settings` standalone.
    let tables: &[&str] = &[
        "app_settings",
        "taxonomy",
        "package_family",
        "packages",
        // The rest are either zero-row in source or rebuilt by scanner
        // / tagger (package_dependencies, package_dep_links, family_tags,
        // package_tags, tagging_runs).
    ];

    println!("copying tables:");
    let mut copied_any = false;
    for tbl in tables {
        // src_count: how many rows exist in the source view (pre-WAL).
        let src_count: Result<i64, _> =
            target_conn.query_row(&format!("SELECT COUNT(*) FROM src.{tbl}"), [], |r| r.get(0));
        let src_n = match src_count {
            Ok(n) => n,
            Err(e) => {
                println!("  {tbl:<22} SKIP (count failed in source: {e})");
                continue;
            }
        };

        // First try: bulk copy in one statement. Fast when the table
        // btree is fully traversable.
        let bulk_sql = format!("INSERT OR IGNORE INTO main.{tbl} SELECT * FROM src.{tbl}");
        match target_conn.execute(&bulk_sql, []) {
            Ok(inserted) => {
                println!("  {tbl:<22} src_rows={src_n:<6} copied={inserted}  (bulk)");
                copied_any = true;
                continue;
            }
            Err(e) => {
                println!(
                    "  {tbl:<22} src_rows={src_n:<6} bulk failed: {e}; trying row-by-row fallback",
                );
            }
        }

        // Fallback: row-by-row by rowid. Each row is its own statement,
        // so a single corrupt row only loses that row instead of the
        // entire table. Slower (~thousand-row tables take a few seconds)
        // but tolerates localized page-level corruption.
        match copy_rowids_one_by_one(&target_conn, tbl) {
            Ok((ok, fail)) => {
                println!(
                    "  {tbl:<22} src_rows={src_n:<6} copied={ok} failed={fail}  (rowid scan)"
                );
                if ok > 0 {
                    copied_any = true;
                }
            }
            Err(e) => {
                println!("  {tbl:<22} src_rows={src_n:<6} RECOVERY FAILED: {e}");
            }
        }
    }

    if !copied_any {
        return Err(anyhow!("no tables copied — source is unreadable"));
    }

    // 5. Re-enable FKs and verify nothing dangles.
    target_conn.pragma_update(None, "foreign_keys", "ON")?;
    println!();
    println!("foreign_key_check:");
    match target_conn.prepare("PRAGMA foreign_key_check") {
        Ok(mut stmt) => {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, i64>(1).unwrap_or(-1),
                    r.get::<_, String>(2).unwrap_or_default(),
                    r.get::<_, i64>(3).unwrap_or(-1),
                ))
            });
            let mut any = false;
            match rows {
                Ok(it) => {
                    for row in it {
                        any = true;
                        match row {
                            Ok((tbl, rowid, parent, fkid)) => {
                                println!("  VIOLATION: table={tbl} rowid={rowid} parent={parent} fkid={fkid}")
                            }
                            Err(e) => println!("  ROW ERR: {e}"),
                        }
                    }
                    if !any {
                        println!("  (no foreign key violations)");
                    }
                }
                Err(e) => println!("  query_map err: {e}"),
            }
        }
        Err(e) => println!("  prepare err: {e}"),
    };

    // 6. Detach + close cleanly so the file is finalized.
    target_conn
        .execute("DETACH DATABASE src", [])
        .with_context(|| "detach src")?;

    println!();
    println!("final target row counts:");
    let probe_tables: &[&str] = &[
        "app_settings",
        "packages",
        "package_dependencies",
        "package_dep_links",
        "package_family",
        "family_tags",
        "package_tags",
        "taxonomy",
        "tagging_runs",
        "family_embeddings",
    ];
    for t in probe_tables {
        match target_conn.query_row::<i64, _, _>(
            &format!("SELECT COUNT(*) FROM {t}"),
            [],
            |r| r.get(0),
        ) {
            Ok(n) => println!("  {t:<22} {n}"),
            Err(e) => println!("  {t:<22} ERROR: {e}"),
        }
    }

    // 5. Run target integrity_check to verify the recovered DB is clean.
    println!();
    println!("target integrity_check:");
    match target_conn.prepare("PRAGMA integrity_check") {
        Ok(mut stmt) => {
            let rows = stmt.query_map([], |r| r.get::<_, String>(0));
            match rows {
                Ok(it) => {
                    for row in it {
                        match row {
                            Ok(s) => println!("  {s}"),
                            Err(e) => println!("  ROW ERR: {e}"),
                        }
                    }
                }
                Err(e) => println!("  query_map err: {e}"),
            }
        }
        Err(e) => println!("  prepare err: {e}"),
    }

    println!();
    println!(
        "DONE. Target is at: {}\n(Review row counts above; live DB has not been touched.)",
        target.display()
    );
    Ok(())
}

/// Read-only integrity probe in two passes:
///   - Pass A (`mode=ro&immutable=1`): WAL is ignored. Shows the
///     **on-disk state of the main file alone** — useful for
///     characterizing corruption that's pre-WAL.
///   - Pass B (`mode=ro` only): WAL is consulted normally. Shows
///     **the live view** that a normal opener would see, which is
///     the state any recovery has to deal with.
///
/// Both passes are non-destructive. Pass B may touch the SHM
/// (wal-index) but never the main file or the WAL itself. Each pass
/// runs in isolation so a hard error in one still lets the other
/// produce useful output.
fn run_integrity_check(db_path: &std::path::Path) -> Result<()> {
    let path_str = db_path.display().to_string().replace('\\', "/");

    println!("========================================================");
    println!("PASS A: main file only (immutable=1, WAL ignored)");
    println!("========================================================");
    let uri_a = format!("file:{path_str}?mode=ro&immutable=1");
    integrity_pass(&uri_a, /*include_schema_dump*/ true);

    println!();
    println!("========================================================");
    println!("PASS B: live view (WAL applied in-memory, no writes)");
    println!("========================================================");
    let uri_b = format!("file:{path_str}?mode=ro");
    integrity_pass(&uri_b, /*include_schema_dump*/ false);

    Ok(())
}

/// Open the URI read-only and run user_version + quick_check +
/// integrity_check + row counts. Failures are reported per-section so
/// we always get whatever output is still producible.
fn integrity_pass(uri: &str, include_schema_dump: bool) {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = match Connection::open_with_flags(uri, flags) {
        Ok(c) => c,
        Err(e) => {
            println!("OPEN FAILED ({uri}): {e}");
            return;
        }
    };

    match conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0)) {
        Ok(v) => println!("user_version: {v}"),
        Err(e) => println!("user_version: ERROR — {e}"),
    }

    println!();
    println!("quick_check:");
    match run_check_pragma(&conn, "quick_check") {
        Ok(lines) => {
            for l in &lines {
                println!("  {l}");
            }
            if lines.len() == 1 && lines[0] == "ok" {
                println!("  (no corruption detected by quick_check)");
            }
        }
        Err(e) => println!("  ERROR running quick_check: {e}"),
    }

    println!();
    println!("integrity_check:");
    match run_check_pragma(&conn, "integrity_check") {
        Ok(lines) => {
            for l in &lines {
                println!("  {l}");
            }
            if lines.len() == 1 && lines[0] == "ok" {
                println!("  (no corruption detected by integrity_check)");
            }
        }
        Err(e) => println!("  ERROR running integrity_check: {e}"),
    }

    if include_schema_dump {
        println!();
        println!("schema with rootpages:");
        match conn.prepare(
            "SELECT type, name, tbl_name, rootpage FROM sqlite_master
             WHERE type IN ('table','index')
             ORDER BY rootpage",
        ) {
            Ok(mut stmt) => {
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                });
                match rows {
                    Ok(it) => {
                        for row in it {
                            match row {
                                Ok((kind, name, tbl, page)) => println!(
                                    "  rootpage={page:<5} {kind:<5} {name}  (tbl={tbl})"
                                ),
                                Err(e) => println!("  ROW ERR: {e}"),
                            }
                        }
                    }
                    Err(e) => println!("  query_map err: {e}"),
                }
            }
            Err(e) => println!("  prepare err: {e}"),
        }
    }

    println!();
    println!("row counts:");
    let tables = vec![
        "app_settings",
        "packages",
        "package_dependencies",
        "package_dep_links",
        "package_family",
        "family_tags",
        "package_tags",
        "taxonomy",
        "tagging_runs",
    ];
    for t in tables {
        match conn.query_row::<i64, _, _>(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)) {
            Ok(n) => println!("  {t:<22} {n}"),
            Err(e) => println!("  {t:<22} ERROR: {e}"),
        }
    }

    // Recovery-value probes: what tagging-stage data survives in this
    // view? Mostly here to gauge re-tagging cost — if `purpose` text
    // survived, re-tagging skips the per-family LLM summary step.
    println!();
    println!("salvage probes (package_family columns):");
    let probes = vec![
        ("families with purpose",
         "SELECT COUNT(*) FROM package_family WHERE purpose IS NOT NULL AND TRIM(purpose) <> ''"),
        ("families with tagging_state='done'",
         "SELECT COUNT(*) FROM package_family WHERE tagging_state='done'"),
        ("families with non-NULL tagged_at",
         "SELECT COUNT(*) FROM package_family WHERE tagged_at IS NOT NULL"),
        ("families with taxonomy_version='v4'",
         "SELECT COUNT(*) FROM package_family WHERE taxonomy_version='v4'"),
        ("packages.purpose non-empty (legacy v3 column)",
         "SELECT COUNT(*) FROM packages WHERE purpose IS NOT NULL AND TRIM(purpose) <> ''"),
        ("packages.tagging_state='done' (legacy v3)",
         "SELECT COUNT(*) FROM packages WHERE tagging_state='done'"),
    ];
    for (label, sql) in probes {
        match conn.query_row::<i64, _, _>(sql, [], |r| r.get(0)) {
            Ok(n) => println!("  {label:<48} {n}"),
            Err(e) => println!("  {label:<48} ERROR: {e}"),
        }
    }

    println!();
    println!("tagging_runs entries:");
    match conn.prepare(
        "SELECT id, started_at, completed_at, taxonomy_version, model, total, succeeded, failed
         FROM tagging_runs ORDER BY id",
    ) {
        Ok(mut stmt) => {
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            });
            match rows {
                Ok(it) => {
                    for row in it {
                        match row {
                            Ok((id, started, completed, ver, model, total, succ, fail)) => {
                                println!(
                                    "  id={id} started={started} completed={completed:?} ver={ver} model={model} total={total} succ={succ} fail={fail}"
                                );
                            }
                            Err(e) => println!("  ROW ERR: {e}"),
                        }
                    }
                }
                Err(e) => println!("  query_map err: {e}"),
            }
        }
        Err(e) => println!("  prepare err: {e}"),
    };
}

/// PRAGMA quick_check / integrity_check return either a single row
/// containing the literal "ok", or N rows each describing a fault.
/// Collect all rows so we can render them in one block.
fn run_check_pragma(conn: &Connection, name: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {name}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Run the same query against all 4 (model, input_kind) variants and
/// render a rank-matrix so a reader can see at a glance:
///   - which families ALL variants agree on (consensus signal)
///   - which families only one variant surfaces (model/input bias)
///   - rank position differences (e.g. a hit at #1 in nomic but #14 in bge
///     hints at a vocabulary mismatch worth investigating)
///
/// Score is included alongside rank because rank alone hides confidence —
/// a #1 at 0.62 vs #1 at 0.85 are different qualitative claims.
fn run_compare_search(
    conn: &Connection,
    query: &str,
    top_n: usize,
) -> Result<()> {
    use std::collections::HashMap;

    let variants = [
        (ModelChoice::BgeSmallEnV15, InputKind::Purpose),
        (ModelChoice::BgeSmallEnV15, InputKind::PurposeWithTags),
        (ModelChoice::NomicEmbedTextV15, InputKind::Purpose),
        (ModelChoice::NomicEmbedTextV15, InputKind::PurposeWithTags),
    ];
    let col_labels = ["bge/p", "bge/p+t", "nom/p", "nom/p+t"];

    println!("COMPARISON: \"{query}\"  (top {top_n} per variant)");
    println!();

    // Run all four searches, collect SearchHits per variant.
    let mut all_hits: Vec<Vec<search::SearchHit>> = Vec::with_capacity(4);
    for (model, input) in variants.iter() {
        let hits = search::search_text(conn, *model, *input, query, top_n)?;
        all_hits.push(hits);
    }

    // Headline: top-3 per variant for quick eyeballing.
    println!("HEADLINE (top 3 per variant):");
    for (i, (model, input)) in variants.iter().enumerate() {
        println!("  [{}] {} / {}", col_labels[i], model.name(), input.name());
        for (rank, h) in all_hits[i].iter().take(3).enumerate() {
            println!(
                "      {}. [{:.4}] family_id={:<5} {}.{}",
                rank + 1, h.score, h.family_id, h.creator, h.package_name,
            );
        }
    }

    // Build union: family_id -> [rank, score] per variant slot (None if
    // outside that variant's top-N).
    let mut union: HashMap<i64, [Option<(usize, f32)>; 4]> = HashMap::new();
    let mut name_lookup: HashMap<i64, String> = HashMap::new();
    for (vi, hits) in all_hits.iter().enumerate() {
        for (rank, h) in hits.iter().enumerate() {
            union.entry(h.family_id).or_insert([None; 4])[vi] = Some((rank + 1, h.score));
            name_lookup
                .entry(h.family_id)
                .or_insert_with(|| format!("{}.{}", h.creator, h.package_name));
        }
    }

    // Sort: most variants agreeing first, then best (lowest) rank first,
    // then family_id for stability.
    let mut rows: Vec<(i64, [Option<(usize, f32)>; 4])> = union.into_iter().collect();
    rows.sort_by(|a, b| {
        let count = |slots: &[Option<(usize, f32)>; 4]| slots.iter().filter(|x| x.is_some()).count();
        let best_rank = |slots: &[Option<(usize, f32)>; 4]| {
            slots
                .iter()
                .filter_map(|x| x.as_ref().map(|(r, _)| *r))
                .min()
                .unwrap_or(usize::MAX)
        };
        count(&b.1)
            .cmp(&count(&a.1))
            .then(best_rank(&a.1).cmp(&best_rank(&b.1)))
            .then(a.0.cmp(&b.0))
    });

    println!();
    println!(
        "RANK MATRIX (union of all variants' top-{top_n}, sorted by consensus then best rank):"
    );
    println!();
    println!(
        "  {:<8}  {:<46}  {:>8}  {:>8}  {:>8}  {:>8}   in",
        "fam_id", "package", col_labels[0], col_labels[1], col_labels[2], col_labels[3]
    );
    println!(
        "  {}  {}  {}  {}  {}  {}  ----",
        "-".repeat(8),
        "-".repeat(46),
        "-".repeat(8),
        "-".repeat(8),
        "-".repeat(8),
        "-".repeat(8),
    );
    for (fid, ranks) in &rows {
        let name = name_lookup
            .get(fid)
            .map(|s| truncate(s, 46))
            .unwrap_or_else(|| "?".to_string());
        let cells: Vec<String> = ranks
            .iter()
            .map(|r| match r {
                Some((rank, score)) => format!("#{} {:.2}", rank, score),
                None => "-".to_string(),
            })
            .collect();
        let present = ranks.iter().filter(|x| x.is_some()).count();
        println!(
            "  {:<8}  {:<46}  {:>8}  {:>8}  {:>8}  {:>8}   {}/4",
            fid, name, cells[0], cells[1], cells[2], cells[3], present
        );
    }

    // Footer: simple aggregate stats so the reader doesn't have to count.
    let total_unique = rows.len();
    let by_count = |n: usize| rows.iter().filter(|(_, s)| s.iter().filter(|x| x.is_some()).count() == n).count();
    println!();
    println!(
        "summary: {total_unique} unique families across all variants  ({} 4/4, {} 3/4, {} 2/4, {} 1/4)",
        by_count(4), by_count(3), by_count(2), by_count(1)
    );
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn print_help() {
    eprintln!(
        r#"embed_library — local embedding pipeline over package_family.purpose

Usage:
  embed_library --status
  embed_library --integrity-check                     # read-only DB sanity probe; skips migrations
  embed_library --recover-from <src> [--recover-to <dst>]   # salvage a corrupt DB into a fresh file (no live-DB writes)
  embed_library --embed-all [--models <list>] [--inputs <list>] [--limit N] [--batch-size N]
  embed_library --re-embed [--models <list>] [--inputs <list>]    # clear + redo selected variants
  embed_library --search "<query>" [--model <m>] [--input <k>] [--top-n N]
  embed_library --compare-search "<query>" [--top-n N]  # same query, all 4 variants, side-by-side
  embed_library --similar-to <family_id> [--model <m>] [--input <k>] [--top-n N]

Variants (model x input_kind, 4 combinations by default):
  --models     comma-separated: bge, nomic            (default: both)
  --inputs     comma-separated: purpose, purpose-with-tags  (default: both)

Search/similarity pick a single variant:
  --model      bge | nomic                            (default: bge)
  --input      purpose | purpose-with-tags            (default: purpose)

Other:
  --db <path>          override SQLite index path (default: %APPDATA%\com.github.kylinblue.vam-package-browser\index.sqlite)
  --batch-size N       encoder batch size  (default: 32)
  --top-n N            hits to return       (default: 20)
  --limit N            cap encode pass at N families  (for smoke tests)
  --help, -h           this message
"#
    );
}
