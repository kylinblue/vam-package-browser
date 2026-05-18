//! Per-package JSONL record builder. Reads a .var file (read-only), extracts
//! meta.json, summarizes the contentList, and emits the JSON record shape
//! that Grok consumes. Shared by both `export_sample` (sample dumps) and
//! `tag_library` (production tagging).

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use zip::ZipArchive;

use crate::meta;

#[derive(Debug, Serialize)]
pub struct SampleRecord {
    pub id: i64,
    pub var_filename: String,
    pub creator: String,
    pub package_name: String,
    pub version: String,
    pub package_type: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub content_summary: ContentSummary,
}

#[derive(Debug, Serialize)]
pub struct ContentSummary {
    pub total_files: usize,
    pub by_prefix: Vec<PrefixBucket>,
    pub notable_files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PrefixBucket {
    pub prefix: String,
    pub count: usize,
    pub sample_files: Vec<String>,
}

/// Build a single JSONL record from a package's id + on-disk var path. Opens
/// the .var read-only, parses meta.json, and summarizes contentList.
pub fn build_record(id: i64, var_path: &str) -> Result<SampleRecord> {
    let path = Path::new(var_path);
    let var_filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let (meta_data, content_list) = read_var(var_path)?;
    let summary = summarize_content(&content_list);
    let version = parse_version_from_filename(&var_filename).unwrap_or_default();
    let package_type = meta::classify(&content_list).as_str().to_string();

    Ok(SampleRecord {
        id,
        var_filename,
        creator: meta_data.creator_name,
        package_name: meta_data.package_name,
        version,
        package_type,
        description: meta_data.description.filter(|s| !s.trim().is_empty()),
        instructions: meta_data.instructions.filter(|s| !s.trim().is_empty()),
        content_summary: summary,
    })
}

fn read_var(var_path: &str) -> Result<(meta::PackageMeta, Vec<String>)> {
    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(var_path)
        .with_context(|| format!("open .var read-only: {var_path}"))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("read zip central dir: {var_path}"))?;

    let mut entries = Vec::with_capacity(zip.len());
    let mut meta_idx: Option<usize> = None;
    for i in 0..zip.len() {
        let e = zip.by_index_raw(i)?;
        let name = e.name().to_string();
        if meta_idx.is_none() && name.eq_ignore_ascii_case("meta.json") {
            meta_idx = Some(i);
        }
        entries.push(name);
    }
    let i = meta_idx.ok_or_else(|| anyhow!("no meta.json"))?;
    let mut entry = zip.by_index(i)?;

    const MAX: u64 = 4 * 1024 * 1024;
    if entry.size() > MAX {
        return Err(anyhow!("meta.json too large: {} bytes", entry.size()));
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    let trimmed = if buf.len() >= 3 && &buf[..3] == [0xEF, 0xBB, 0xBF] {
        &buf[3..]
    } else {
        &buf[..]
    };
    let meta_data = meta::parse_meta(trimmed)?;
    Ok((meta_data, entries))
}

/// Summarize a contentList into top-N prefix buckets + distinctive filenames.
/// Filters out ZIP directory entries (trailing `/`) so they don't pollute
/// counts or sample lists. Two-tier notable_files: primary content-bearing
/// extensions first, then `.cs` as fallback.
fn summarize_content(content_list: &[String]) -> ContentSummary {
    let content_list: Vec<String> = content_list
        .iter()
        .filter(|p| !p.replace('\\', "/").ends_with('/'))
        .cloned()
        .collect();
    let total_files = content_list.len();

    let mut groups: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for p in &content_list {
        let normalized = p.replace('\\', "/");
        if normalized.eq_ignore_ascii_case("meta.json") {
            continue;
        }
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            continue;
        }
        let take_count = (segments.len() - 1).min(3);
        let prefix = segments[..take_count].join("/");
        let entry = groups.entry(prefix).or_insert_with(|| (0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 2 {
            let basename = segments.last().copied().unwrap_or("").to_string();
            if !basename.is_empty() {
                entry.1.push(basename);
            }
        }
    }

    let mut by_prefix: Vec<PrefixBucket> = groups
        .into_iter()
        .map(|(prefix, (count, sample_files))| PrefixBucket {
            prefix,
            count,
            sample_files,
        })
        .collect();
    by_prefix.sort_by(|a, b| b.count.cmp(&a.count));
    by_prefix.truncate(8);

    let primary_exts: &[&str] = &[".cslist", ".vap", ".vam", ".assetbundle", ".json"];
    let fallback_exts: &[&str] = &[".cs"];
    let mut notable: Vec<String> = Vec::new();
    for exts in [primary_exts, fallback_exts] {
        for p in &content_list {
            if notable.len() >= 20 {
                break;
            }
            let normalized = p.replace('\\', "/");
            let lower = normalized.to_lowercase();
            if lower == "meta.json" || lower.contains("/morphs/") {
                continue;
            }
            if !exts.iter().any(|e| lower.ends_with(e)) {
                continue;
            }
            let basename = normalized.rsplit('/').next().unwrap_or("").to_string();
            if basename.is_empty() || notable.contains(&basename) {
                continue;
            }
            notable.push(basename);
        }
        if notable.len() >= 20 {
            break;
        }
    }

    ContentSummary {
        total_files,
        by_prefix,
        notable_files: notable,
    }
}

fn parse_version_from_filename(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".var")?;
    let last_dot = stem.rfind('.')?;
    Some(stem[last_dot + 1..].to_string())
}
