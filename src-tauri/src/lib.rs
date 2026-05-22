mod commands;
pub mod deps;
pub mod embedding;
mod hub;
pub mod index;
pub mod meta;
pub mod propagation;
mod scanner;
pub mod tagging;
pub mod thumbnails;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::{Condvar, Mutex};
use tauri::http::{Request, Response};
use tauri::{Manager, UriSchemeContext, Wry};

/// Counting semaphore — caps concurrent sub-thumbnail generations. WebView2
/// fires up to 6 simultaneous requests to a single HTTP origin; if we
/// serialized them all behind a single mutex, the browser saw each request
/// stalled for the cumulative gen time of everything in front. 4 permits gives
/// real 4-way parallelism (4 cores doing image decode + resize + WebP encode)
/// without piling up RAM from too many concurrent decode buffers.
struct Semaphore {
    permits: Mutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    fn new(initial: usize) -> Self {
        Self { permits: Mutex::new(initial), cond: Condvar::new() }
    }
    fn acquire(&self) -> SemaphoreGuard<'_> {
        let mut count = self.permits.lock();
        while *count == 0 {
            self.cond.wait(&mut count);
        }
        *count -= 1;
        SemaphoreGuard { sem: self }
    }
}

struct SemaphoreGuard<'a> {
    sem: &'a Semaphore,
}

impl Drop for SemaphoreGuard<'_> {
    fn drop(&mut self) {
        let mut count = self.sem.permits.lock();
        *count += 1;
        self.sem.cond.notify_one();
    }
}

static SUB_THUMB_GEN: Lazy<Semaphore> = Lazy::new(|| {
    // Use ~50% of available cores. Each permit can hold ~120MB of decode/encode
    // buffers at peak (worst case: 4K JPG → RGB8 → resize → WebP), so leave
    // headroom for the rest of the system and the WebView2 process.
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let permits = (n / 2).max(1);
    eprintln!("sub-thumb gen: {permits} permits ({n} cores detected)");
    Semaphore::new(permits)
});

pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub data_dir: PathBuf,
    /// Set to `true` to request that an in-flight hub sync stops at the next
    /// rate-limit checkpoint. Cleared by the sync task at start.
    pub hub_sync_cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Set while a sync is running. Survives frontend HMR reloads so the
    /// reloaded UI can detect "sync is continuing in the background".
    pub hub_sync_running: Arc<std::sync::atomic::AtomicBool>,
}

impl AppState {
    pub fn thumbs_dir(&self) -> PathBuf {
        self.data_dir.join("thumbs")
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .register_uri_scheme_protocol("thumb", thumb_protocol)
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .expect("app_data_dir resolves on desktop");
            std::fs::create_dir_all(&data_dir).expect("create app data dir");

            let db_path = data_dir.join("index.sqlite");
            let conn = index::open_and_migrate(&db_path)
                .expect("open & migrate sqlite index");

            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                data_dir,
                hub_sync_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                hub_sync_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            });

            // Embedding model warm-up shelved alongside the Ask UI — pulling
            // ~250 MB of model weights into RAM at every launch isn't worth it
            // while semantic search is disabled. To reactivate: uncomment the
            // std::thread::spawn block below. See TODO-semantic-search-ui.md
            // for context.
            //
            // std::thread::spawn(|| {
            //     use embedding::{model::encode_batch, ModelChoice};
            //     let dummy = ["warmup".to_string()];
            //     if let Err(e) = encode_batch(ModelChoice::NomicEmbedTextV15, &dummy, None) {
            //         eprintln!("semantic-search model warm-up failed: {e:#}");
            //     }
            // });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_library,
            commands::query_packages,
            commands::count_packages,
            commands::list_creators,
            commands::list_creators_with_counts,
            commands::list_type_counts,
            commands::list_hub_categories,
            commands::list_namespaces,
            commands::list_tag_counts,
            commands::search_families,
            commands::search_similar_families,
            commands::get_packages_by_ids,
            commands::open_external_url,
            commands::get_settings,
            commands::set_addon_root,
            commands::set_favorite,
            commands::set_hidden,
            commands::reveal_in_folder,
            commands::get_package_detail,
            commands::get_package_relationships,
            commands::resolve_dependencies,
            commands::generate_thumbnails,
            commands::start_hub_sync,
            commands::stop_hub_sync,
            commands::hub_catalog_refresh,
            commands::hub_status,
            commands::hub_sync_active,
            commands::hub_debug_dump,
            commands::hub_debug_search,
            commands::hub_debug_fetch,
            commands::set_hub_pin,
            commands::set_hub_category,
            commands::set_hub_author,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// URL patterns:
///   `/<package_id>`                      → primary thumb (pre-generated cached WebP)
///   `/<package_id>/img/<base64_entry>`   → per-image: stream source bytes from the .var
///                                           directly to the browser. No decode/resize/encode
///                                           on the Rust side; browser handles scaling. Trades
///                                           browser RAM for vastly less per-request work.
fn thumb_protocol(
    ctx: UriSchemeContext<'_, Wry>,
    req: Request<Vec<u8>>,
) -> Response<Cow<'static, [u8]>> {
    let app = ctx.app_handle();
    let state = app.state::<AppState>();
    let thumbs_dir = state.thumbs_dir();

    let full_path = req.uri().path();
    let path = full_path.split('?').next().unwrap_or(full_path);
    let trimmed = path.trim_start_matches('/');
    let mut segments = trimmed.splitn(3, '/');

    let id_str = segments.next().unwrap_or("");
    let Ok(id) = id_str.parse::<i64>() else {
        return bad_request();
    };

    // Default cap protects the gallery grid from streaming huge texture maps
    // (which would pile up browser RAM). `?big=1` opts in — used by the hero
    // image when the user explicitly clicks a "huge" placeholder.
    let allow_huge = req
        .uri()
        .query()
        .map(|q| q.split('&').any(|kv| kv == "big=1"))
        .unwrap_or(false);
    let max_pull_bytes: u64 = if allow_huge {
        500 * 1024 * 1024 // 500MB — well above any realistic single VaM image
    } else {
        50 * 1024 * 1024
    };

    match (segments.next(), segments.next()) {
        // Sub-image: pull source bytes directly (no thumbnail generation).
        (Some("img"), Some(b64_entry)) => {
            let entry_path = match base64_decode(b64_entry) {
                Some(e) => e,
                None => return bad_request(),
            };
            // Light permit to avoid 100 concurrent disk reads from the same .var.
            let _permit = SUB_THUMB_GEN.acquire();
            let var_path = {
                let conn = state.db.lock();
                conn.query_row(
                    "SELECT var_path FROM packages WHERE id = ?1",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            };
            let Some(var_path) = var_path else {
                return not_found();
            };
            match read_zip_entry(&var_path, &entry_path, max_pull_bytes) {
                Ok(bytes) => Response::builder()
                    .status(200)
                    .header("Content-Type", content_type_for(&entry_path))
                    .header("Cache-Control", "max-age=300")
                    .body(Cow::Owned(bytes))
                    .unwrap(),
                Err(e) => {
                    eprintln!("pull-display failed for {id}/{entry_path}: {e:#}");
                    not_found()
                }
            }
        }
        // Primary thumb: pre-generated cached WebP.
        (None, None) => match std::fs::read(thumbnails::thumb_path(&thumbs_dir, id)) {
            Ok(bytes) => Response::builder()
                .status(200)
                .header("Content-Type", "image/webp")
                .header("Cache-Control", "no-cache")
                .body(Cow::Owned(bytes))
                .unwrap(),
            Err(_) => not_found(),
        },
        _ => bad_request(),
    }
}

fn content_type_for(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        // .jpg / .jpeg / unknown
        "image/jpeg"
    }
}

fn read_zip_entry(
    var_path: &str,
    entry_path: &str,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(var_path)
        .map_err(|e| anyhow::anyhow!("open {var_path}: {e}"))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| anyhow::anyhow!("read zip {var_path}: {e}"))?;
    let mut entry = zip
        .by_name(entry_path)
        .map_err(|e| anyhow::anyhow!("zip entry {entry_path}: {e}"))?;
    if entry.size() > max_bytes {
        anyhow::bail!(
            "source image too large ({} bytes, cap {}) — skipped",
            entry.size(),
            max_bytes
        );
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn bad_request() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(400)
        .body(Cow::Borrowed(&[][..]))
        .unwrap()
}

fn not_found() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(404)
        .body(Cow::Borrowed(&[][..]))
        .unwrap()
}

/// URL-safe base64 (RFC 4648 §5, `-_`, padding optional) decode → UTF-8 string.
/// We use this for in-zip entry paths in protocol URLs; non-UTF-8 entries don't
/// exist in VaM packages.
fn base64_decode(input: &str) -> Option<String> {
    const URL_TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut lookup = [0xFFu8; 256];
    for (i, &b) in URL_TABLE.iter().enumerate() {
        lookup[b as usize] = i as u8;
    }
    let input = input.trim_end_matches('=');
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        let v = lookup[b as usize];
        if v == 0xFF {
            return None;
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    String::from_utf8(out).ok()
}
