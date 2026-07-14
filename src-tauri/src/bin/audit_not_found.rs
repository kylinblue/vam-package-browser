//! Audit tool for hub-sync false negatives (TODO-hub-sync.md §1).
//!
//! Pulls a stratified sample of `hub_sync_state = 'not_found'` package
//! families (proportional by `package_type`, one row per family), gathers
//! hub-side evidence for each through the SAME `hub::HubClient` the sync
//! uses, and writes an evidence CSV for hand-labeling.
//!
//! Evidence tiers per sampled row:
//!   1. slug-catalog exact-norm candidates (cached `hub_resources`)
//!   2. XF per-author search (cached per creator)
//!   3. XF per-author keyword search (full name, then longest token)
//!   4. XF GLOBAL keyword search (no author filter) — the "human proxy":
//!      catches creator-alias misses the per-author searches can't see
//!   5. alias-listing search — for creators with confirmed alias pairs
//!      already in the DB (filename/slug_match rows where hub_author
//!      normalizes differently from the local creator)
//!
//! Promising candidates are HEAD-probed; a Hosted CDN filename is compared
//! against the local (creator, package) both byte-lowercase (what the sync's
//! `norm_key` does today) and punctuation-stripped (`normalize_compare`) so
//! the CSV distinguishes "sync should have caught this" from "only a looser
//! normalizer would catch this".
//!
//! Read-only on the DB (opened with SQLITE_OPEN_READ_ONLY) — no lock needed
//! per the CLAUDE.md database access protocol.
//!
//! Usage: audit_not_found [--db PATH] [--out PATH] [--sample N]
//!                        [--rate-ms MS] [--max-probes N] [--seed N]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use vam_package_browser_lib::hub;

struct Args {
    db: PathBuf,
    out: PathBuf,
    sample: usize,
    rate_ms: u64,
    max_probes: usize,
    seed: u64,
}

fn default_db_path() -> PathBuf {
    vam_package_browser_lib::paths::default_db_path()
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        db: default_db_path(),
        out: PathBuf::from("docs/audits/hub-sync-notfound-evidence.csv"),
        sample: 50,
        rate_ms: 700,
        max_probes: 6,
        seed: 20260714,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => args.db = PathBuf::from(it.next().ok_or_else(|| anyhow!("--db needs a value"))?),
            "--out" => args.out = PathBuf::from(it.next().ok_or_else(|| anyhow!("--out needs a value"))?),
            "--sample" => args.sample = it.next().ok_or_else(|| anyhow!("--sample needs a value"))?.parse()?,
            "--rate-ms" => args.rate_ms = it.next().ok_or_else(|| anyhow!("--rate-ms needs a value"))?.parse()?,
            "--max-probes" => args.max_probes = it.next().ok_or_else(|| anyhow!("--max-probes needs a value"))?.parse()?,
            "--seed" => args.seed = it.next().ok_or_else(|| anyhow!("--seed needs a value"))?.parse()?,
            other => return Err(anyhow!("unknown arg: {other}")),
        }
    }
    Ok(args)
}

#[derive(Debug, Clone)]
struct Sampled {
    id: i64,
    creator: String,
    package_name: String,
    package_type: String,
}

/// Mirrors commands.rs `parse_var_filename` (private there).
fn parse_var_filename(name: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".var").unwrap_or(name);
    let mut parts = stem.rsplitn(3, '.');
    let version = parts.next()?.to_string();
    let package = parts.next()?.to_string();
    let creator = parts.next()?.to_string();
    if creator.is_empty() || package.is_empty() || version.is_empty() {
        return None;
    }
    Some((creator, package, version))
}

/// Mirrors commands.rs `longest_token` (private there).
fn longest_token(name: &str) -> Option<String> {
    let tokens: Vec<&str> = name.split(|c: char| c == '_' || c == '-' || c == '.').collect();
    if tokens.len() <= 1 {
        return None;
    }
    tokens
        .into_iter()
        .filter(|t| t.len() >= 3)
        .max_by_key(|t| t.len())
        .map(|t| t.to_string())
}

fn seeded_hash(seed: u64, id: i64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    id.hash(&mut h);
    h.finish()
}

fn csv_field(s: &str) -> String {
    let needs = s.contains(',') || s.contains('"') || s.contains('\n');
    if needs {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let conn = rusqlite::Connection::open_with_flags(
        &args.db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .with_context(|| format!("open read-only {}", args.db.display()))?;

    // ── Stage 1: stratified sample ────────────────────────────────────────
    // One representative row per not_found family (creator+package_name),
    // grouped by package_type, allocated proportionally over the sample
    // budget (each non-empty type gets at least 1).
    let mut families: BTreeMap<String, Vec<Sampled>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT MIN(id), creator, package_name, COALESCE(package_type,'Unknown')
             FROM packages
             WHERE hub_sync_state = 'not_found'
             GROUP BY creator, package_name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Sampled {
                id: r.get(0)?,
                creator: r.get(1)?,
                package_name: r.get(2)?,
                package_type: r.get(3)?,
            })
        })?;
        for row in rows {
            let row = row?;
            families.entry(row.package_type.clone()).or_default().push(row);
        }
    }
    let total_families: usize = families.values().map(Vec::len).sum();
    if total_families == 0 {
        return Err(anyhow!("no not_found families in the DB"));
    }
    eprintln!(
        "not_found families: {total_families} across {} package types",
        families.len()
    );

    let mut sample: Vec<Sampled> = Vec::new();
    for (ptype, mut rows) in families {
        let share = ((rows.len() as f64 / total_families as f64) * args.sample as f64)
            .round()
            .max(1.0) as usize;
        // Deterministic pseudo-random order via seeded hash on id.
        rows.sort_by_key(|r| seeded_hash(args.seed, r.id));
        let take = share.min(rows.len());
        eprintln!("  {ptype}: {} families -> sampling {take}", rows.len());
        sample.extend(rows.into_iter().take(take));
    }
    sample.sort_by(|a, b| (a.creator.to_lowercase(), a.package_name.to_lowercase())
        .cmp(&(b.creator.to_lowercase(), b.package_name.to_lowercase())));
    eprintln!("total sampled: {}", sample.len());

    // ── Stage 2: local caches from the DB ────────────────────────────────
    // Slug catalog: normalized slug -> [(resource_id, slug)].
    let mut slug_index: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT resource_id, slug FROM hub_resources")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, slug) = row?;
            slug_index
                .entry(hub::normalize_compare(&slug))
                .or_default()
                .push((id, slug));
        }
    }

    // Alias map: local creator -> confirmed hub author names that normalize
    // differently (mined from filename/slug_match rows).
    let mut alias_map: HashMap<String, HashSet<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT creator, hub_author FROM packages
             WHERE hub_sync_state = 'matched'
               AND hub_match_method IN ('filename','slug_match')
               AND hub_author IS NOT NULL AND hub_author <> ''",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (creator, author) = row?;
            if hub::normalize_compare(&creator) != hub::normalize_compare(&author) {
                alias_map.entry(creator).or_default().insert(author);
            }
        }
    }
    eprintln!("alias map: {} creators with confirmed aliases", alias_map.len());

    // ── Stage 3: hub evidence per sampled row ─────────────────────────────
    let client = hub::HubClient::new();
    // Per-creator caches so shared searches don't refetch.
    let mut author_listing_cache: HashMap<String, Vec<hub::HubMatch>> = HashMap::new();

    let mut out = String::new();
    out.push_str(
        "id,creator,package_name,package_type,known_aliases,\
         slug_exact_hits,author_rows,kw_rows,global_rows,alias_rows,probes_done,\
         verified_kind,verified_url,verified_cdn_filename,verified_via,\
         best_fuzzy_score,best_fuzzy_title,best_fuzzy_author,best_fuzzy_url,\
         top_candidates,matchable,miss_mode,notes\n",
    );

    for (i, s) in sample.iter().enumerate() {
        eprintln!(
            "[{}/{}] {}.{} ({})",
            i + 1,
            sample.len(),
            s.creator,
            s.package_name,
            s.package_type
        );
        let pkg_norm = hub::normalize_compare(&s.package_name);
        let target_lower = format!(
            "{}|{}",
            s.creator.to_lowercase(),
            s.package_name.to_lowercase()
        );
        let target_norm = format!(
            "{}|{}",
            hub::normalize_compare(&s.creator),
            pkg_norm
        );

        // Tier 1: slug catalog.
        let slug_hits: Vec<(i64, String)> = slug_index
            .get(&pkg_norm)
            .cloned()
            .unwrap_or_default();

        // Tier 2: per-author listing (cached).
        let author_rows = match author_listing_cache.entry(s.creator.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let r = client
                    .search_resources_by_user(&s.creator)
                    .unwrap_or_else(|err| {
                        eprintln!("  author search failed: {err:#}");
                        Vec::new()
                    });
                sleep_ms(args.rate_ms);
                e.insert(r).clone()
            }
        };

        // Tier 3: per-author keyword search (full name, then longest token).
        let mut kw_rows = client
            .search_resources_for_user_keyword(&s.creator, &s.package_name, Some(2))
            .unwrap_or_else(|err| {
                eprintln!("  kw search failed: {err:#}");
                Vec::new()
            });
        sleep_ms(args.rate_ms);
        if kw_rows.is_empty() {
            if let Some(tok) = longest_token(&s.package_name) {
                kw_rows = client
                    .search_resources_for_user_keyword(&s.creator, &tok, Some(1))
                    .unwrap_or_default();
                sleep_ms(args.rate_ms);
            }
        }

        // Tier 4: global keyword search (the human proxy).
        let global_kw = s.package_name.replace(['_', '-'], " ");
        let global_rows = client
            .search_resources_global(&global_kw, Some(1))
            .unwrap_or_else(|err| {
                eprintln!("  global search failed: {err:#}");
                Vec::new()
            });
        sleep_ms(args.rate_ms);

        // Tier 5: alias listings.
        let aliases: Vec<String> = alias_map
            .get(&s.creator)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        let mut alias_rows: Vec<hub::HubMatch> = Vec::new();
        for alias in &aliases {
            let r = match author_listing_cache.entry(alias.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let r = client
                        .search_resources_by_user(alias)
                        .unwrap_or_default();
                    sleep_ms(args.rate_ms);
                    e.insert(r).clone()
                }
            };
            alias_rows.extend(r);
        }

        // Candidate pool for HEAD probing, ranked: title-norm exact, then
        // title contains pkg / pkg contains title, then slug hits (via
        // resource-page fetch), then everything else with fuzzy score > 0.
        let mut pool: Vec<hub::HubMatch> = Vec::new();
        let mut seen_ids: HashSet<i64> = HashSet::new();
        let push_all = |v: &[hub::HubMatch], pool: &mut Vec<hub::HubMatch>, seen: &mut HashSet<i64>| {
            for hm in v {
                if seen.insert(hm.resource_id) {
                    pool.push(hm.clone());
                }
            }
        };
        push_all(&kw_rows, &mut pool, &mut seen_ids);
        push_all(&author_rows, &mut pool, &mut seen_ids);
        push_all(&alias_rows, &mut pool, &mut seen_ids);
        push_all(&global_rows, &mut pool, &mut seen_ids);
        // Slug hits not already in the pool: fetch their resource page so
        // they carry title/author for ranking (bounded).
        for (rid, slug) in slug_hits.iter().take(3) {
            if seen_ids.contains(rid) {
                continue;
            }
            match client.fetch_resource_page(slug, *rid) {
                Ok(hm) => {
                    seen_ids.insert(*rid);
                    pool.push(hm);
                }
                Err(e) => eprintln!("  slug page fetch failed for {slug}.{rid}: {e:#}"),
            }
            sleep_ms(args.rate_ms);
        }

        let rank = |hm: &hub::HubMatch| -> u32 {
            let t = hub::normalize_compare(&hm.title);
            if t == pkg_norm {
                0
            } else if !pkg_norm.is_empty() && (t.contains(&pkg_norm) || pkg_norm.contains(&t)) {
                1
            } else if hub::score_match(&s.creator, &s.package_name, hm) > 0 {
                2
            } else {
                3
            }
        };
        pool.sort_by_key(rank);
        // Rank-3 candidates have no textual affinity at all — probing them
        // wastes the budget on noise rows from generic global searches.
        pool.retain(|hm| rank(hm) < 3);

        let mut probes_done = 0usize;
        let mut verified_kind = "none";
        let mut verified_url = String::new();
        let mut verified_cdn = String::new();
        let mut verified_via = String::new();
        let mut best_fuzzy: Option<(u32, hub::HubMatch)> = None;

        for hm in pool.iter().take(args.max_probes) {
            let Some((slug, rid)) = hub::extract_slug_and_id_from_url(&hm.url) else {
                continue;
            };
            probes_done += 1;
            match client.head_download(&slug, rid) {
                Ok(hub::DownloadProbe::Hosted { filename }) => {
                    if let Some((c, p, _v)) = parse_var_filename(&filename) {
                        let got_lower = format!("{}|{}", c.to_lowercase(), p.to_lowercase());
                        let got_norm = format!(
                            "{}|{}",
                            hub::normalize_compare(&c),
                            hub::normalize_compare(&p)
                        );
                        if got_lower == target_lower {
                            verified_kind = "exact_lower";
                        } else if got_norm == target_norm {
                            verified_kind = "norm_only";
                        } else if hub::normalize_compare(&p) == pkg_norm {
                            // Same package name, different creator string —
                            // alias/mirror upload.
                            verified_kind = "pkg_norm_other_creator";
                        } else {
                            continue;
                        }
                        verified_url = hm.url.clone();
                        verified_cdn = filename;
                        verified_via = if alias_rows.iter().any(|a| a.resource_id == rid) {
                            "alias".into()
                        } else if kw_rows.iter().any(|a| a.resource_id == rid) {
                            "kw".into()
                        } else if author_rows.iter().any(|a| a.resource_id == rid) {
                            "author".into()
                        } else if global_rows.iter().any(|a| a.resource_id == rid) {
                            "global".into()
                        } else {
                            "slug".into()
                        };
                        break;
                    }
                }
                Ok(hub::DownloadProbe::Offsite { .. }) => {
                    let score = hub::score_match(&s.creator, &s.package_name, hm);
                    if score > 0
                        && best_fuzzy.as_ref().map(|(bs, _)| score > *bs).unwrap_or(true)
                    {
                        best_fuzzy = Some((score, hm.clone()));
                    }
                }
                Ok(hub::DownloadProbe::NotFound) => {}
                Err(e) => eprintln!("  HEAD failed for {slug}.{rid}: {e:#}"),
            }
            sleep_ms(args.rate_ms);
        }

        let top: Vec<String> = pool
            .iter()
            .take(3)
            .map(|hm| format!("{} by {} <{}>", hm.title, hm.author, hm.url))
            .collect();

        let (bf_score, bf_title, bf_author, bf_url) = match &best_fuzzy {
            Some((sc, hm)) => (
                sc.to_string(),
                hm.title.clone(),
                hm.author.clone(),
                hm.url.clone(),
            ),
            None => (String::new(), String::new(), String::new(), String::new()),
        };

        let line = [
            s.id.to_string(),
            s.creator.clone(),
            s.package_name.clone(),
            s.package_type.clone(),
            aliases.join("; "),
            slug_hits
                .iter()
                .map(|(id, sl)| format!("{sl}.{id}"))
                .collect::<Vec<_>>()
                .join("; "),
            author_rows.len().to_string(),
            kw_rows.len().to_string(),
            global_rows.len().to_string(),
            alias_rows.len().to_string(),
            probes_done.to_string(),
            verified_kind.to_string(),
            verified_url,
            verified_cdn,
            verified_via,
            bf_score,
            bf_title,
            bf_author,
            bf_url,
            top.join(" || "),
            String::new(), // matchable (hand label)
            String::new(), // miss_mode (hand label)
            String::new(), // notes
        ]
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
        out.push_str(&line);
        out.push('\n');

        // Flush incrementally so a crash mid-run keeps partial evidence.
        if let Some(parent) = args.out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::File::create(&args.out)
            .and_then(|mut f| f.write_all(out.as_bytes()))
            .with_context(|| format!("write {}", args.out.display()))?;
    }

    eprintln!("done. evidence CSV at {}", args.out.display());
    Ok(())
}
