//! VaM Hub scrape client.
//!
//! XenForo's resource listing (`/resources/?q=...`) silently ignores the `q`
//! param, so we use the real XenForo search workflow:
//!   1. GET `/search/` — extract `_xfToken` (CSRF) from the form
//!   2. POST `/search/search` with `keywords` + `_xfToken`
//!   3. Follow the 303 redirect to `/search/<id>/?q=...`
//!   4. Parse the search results (different markup than the resource listing)
//!
//! The adult-content gate is bypassed by sending `vamhubconsent=yes`. ureq's
//! cookie store carries other XF session cookies (xf_csrf, xf_session) between
//! the GET and POST automatically.

use anyhow::{anyhow, Context, Result};
use scraper::{Html, Selector};
use serde::Serialize;
use std::io::Read;
use std::time::Duration;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0";
const BASE_URL: &str = "https://hub.virtamate.com";

/// Outcome of probing the `/resources/{slug}.{id}/download` URL.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DownloadProbe {
    /// Hub-hosted .var. `filename` is the author's canonical
    /// `Creator.PackageName.Version.var` per the CDN's content-disposition.
    Hosted { filename: String },
    /// Paid / offsite. `url` is the 301 Location target (Patreon, etc.).
    Offsite { url: String },
    /// Resource id no longer exists (404).
    NotFound,
}

/// A single resource entry pulled from the hub's sitemap.
#[derive(Debug, Clone)]
pub struct SitemapEntry {
    pub resource_id: i64,
    pub slug: String,
    /// `lastmod` from the sitemap XML, in unix seconds.
    pub lastmod: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HubMatch {
    pub resource_id: i64,
    pub url: String,
    pub title: String,
    pub author: String,
    /// Content category with the billing-tier prefix stripped (e.g. "Looks",
    /// "Scenes", not "Paid Looks"). Authoritative single-axis classification.
    pub category: Option<String>,
    /// Billing tier extracted from the silver-label prefix:
    /// `None` (= Free), `"paid-early-access"`, or `"paid"`. Kept as a separate
    /// field so the UI can use category alone as the primary chip axis while
    /// surfacing billing as a secondary signal in the detail view.
    pub billing_tier: Option<String>,
    /// `true` when the result row shows the green "Hub-Hosted VAR" label.
    /// Means the .var is downloadable directly from the hub; absence + paid
    /// tier typically means the resource lives behind an external paywall.
    pub is_hub_hosted: bool,
    /// License label as displayed on the row. Examples: "CC BY", "CC BY-SA",
    /// "CC BY-NC", "CC BY-NC-SA", "PC" (Paid Content), "FC". Optional — not
    /// always rendered.
    pub license: Option<String>,
    pub preview_url: Option<String>,
    pub tagline: Option<String>,
    pub updated_at: Option<i64>,
}

pub struct HubClient {
    agent: ureq::Agent,
}

impl HubClient {
    pub fn new() -> Self {
        // Pre-load the consent cookie into the agent's jar so every request
        // carries it automatically (along with any XF session cookies set
        // along the way — important for CSRF validation).
        let mut jar = cookie_store::CookieStore::default();
        let base = url::Url::parse(BASE_URL).expect("BASE_URL parses");
        if let Ok(c) = cookie_store::Cookie::parse(
            "vamhubconsent=yes; Path=/; Domain=hub.virtamate.com",
            &base,
        ) {
            let _ = jar.insert(c, &base);
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(30))
            .timeout_connect(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .redirects(5)
            .cookie_store(jar)
            .build();
        Self { agent }
    }

    /// Per-creator resource search. Sends `c[users]={creator}` so the result
    /// set covers only that author's published resources. Empty keyword.
    pub fn search_resources_by_user(&self, creator: &str) -> Result<Vec<HubMatch>> {
        self.perform_search(
            &[("keywords", ""), ("c[users]", creator)],
            &format!("c[users]={creator}"),
            None,
        )
    }

    /// Targeted single-resource lookup: `c[users]={creator} & keywords={kw}`.
    /// Used by Phase B2 (keyword fallback) to catch resources that XF's
    /// per-author listing doesn't return — typically older resources where
    /// the default search ranking hides them in deep tails.
    ///
    /// `max_pages` caps pagination. `None` = walk all pages (used when the
    /// keyword is specific enough to expect a small targeted result set).
    /// `Some(N)` = stop after page N (used for fallback tokens like
    /// "Morphs" or "Character" that return 50-80+ rows of which only the
    /// top relevance-sorted ones are likely candidates).
    pub fn search_resources_for_user_keyword(
        &self,
        creator: &str,
        keyword: &str,
        max_pages: Option<u32>,
    ) -> Result<Vec<HubMatch>> {
        self.perform_search(
            &[("keywords", keyword), ("c[users]", creator)],
            &format!("c[users]={creator}&q={keyword}"),
            max_pages,
        )
    }

    /// Shared XF search workflow:
    ///   1. GET /search/ for the CSRF token.
    ///   2. POST /search/search with `_xfToken` + `type=resource` + provided fields.
    ///   3. Follow 303 → /search/{id}/?... and parse the result page.
    ///   4. Walk pagination if pagenav reports more pages.
    ///
    /// `type=resource` (XF's internal type name) restricts results to the
    /// Resource Manager. Without it, results mix resources, members, and
    /// media in the same 20-row page (Cowork recon 2026-05-17).
    fn perform_search(
        &self,
        form_fields: &[(&str, &str)],
        diag_label: &str,
        max_pages: Option<u32>,
    ) -> Result<Vec<HubMatch>> {
        let form_url = format!("{BASE_URL}/search/");
        let form_html = self
            .agent
            .get(&form_url)
            .call()
            .with_context(|| format!("GET {form_url}"))?
            .into_string()
            .with_context(|| "read /search/ body")?;
        if is_gate_page(&form_html) {
            return Err(anyhow!("gate: hub returned the age-gate page"));
        }
        let token = extract_csrf_token(&form_html)
            .ok_or_else(|| anyhow!("no _xfToken in /search/ form"))?;

        // Assemble: _xfToken + type=resource + caller fields.
        let mut all_fields: Vec<(&str, &str)> = Vec::with_capacity(form_fields.len() + 2);
        all_fields.push(("_xfToken", token.as_str()));
        all_fields.push(("type", "resource"));
        all_fields.extend_from_slice(form_fields);

        let post_url = format!("{BASE_URL}/search/search");
        let resp = self
            .agent
            .post(&post_url)
            .send_form(&all_fields)
            .with_context(|| format!("POST {post_url}"))?;
        let status = resp.status();
        if status != 200 {
            return Err(anyhow!("http {status} for search POST"));
        }
        let final_url = resp.get_url().to_string();
        let body = resp.into_string().context("read search results body")?;
        if is_gate_page(&body) {
            return Err(anyhow!("gate: hub returned the age-gate page"));
        }

        let mut out = parse_search_results(&body)?;
        let last_page = parse_last_page(&body);
        eprintln!(
            "hub search {diag_label}: page 1 returned {} rows, last_page={:?}",
            out.len(),
            last_page,
        );

        let last_page = last_page.unwrap_or(1);
        let walk_to = match max_pages {
            Some(cap) => last_page.min(cap),
            None => last_page,
        };
        if walk_to < last_page {
            eprintln!(
                "hub search {diag_label}: capping pagination at page {walk_to} (last_page={last_page})"
            );
        }
        if walk_to > 1 {
            for page in 2..=walk_to {
                let sep = if final_url.contains('?') { '&' } else { '?' };
                let page_url = format!("{final_url}{sep}page={page}");
                let page_body = self
                    .agent
                    .get(&page_url)
                    .call()
                    .with_context(|| format!("GET {page_url}"))?
                    .into_string()
                    .with_context(|| format!("read page {page} body"))?;
                if is_gate_page(&page_body) {
                    return Err(anyhow!(
                        "gate: hub returned the age-gate page on page {page}"
                    ));
                }
                let mut more = parse_search_results(&page_body)?;
                eprintln!(
                    "hub search {diag_label}: page {page} returned {} more rows",
                    more.len()
                );
                out.append(&mut more);
            }
        }
        Ok(out)
    }

    /// HEAD the `/resources/{slug}.{id}/download` URL and read
    /// Content-Disposition / Location to find out what kind of resource this
    /// is and how to identify it. Three outcomes:
    ///
    /// - `Hosted { filename }`: hub-hosted .var — `filename` is the canonical
    ///   `Creator.PackageName.Version.var` as the author uploaded it. The CDN
    ///   sets `content-disposition: filename="..."` on the final 200 after a
    ///   303 → CDN redirect.
    /// - `Offsite { url }`: paid resource — hub returned 301 with `Location:`
    ///   pointing at the external pay site (Patreon, etc.). curl/ureq follow
    ///   the redirect; we capture the URL by reading the *first* response's
    ///   header before the redirect happens.
    /// - `NotFound`: 404 — resource_id no longer exists.
    pub fn head_download(&self, slug: &str, resource_id: i64) -> Result<DownloadProbe> {
        // Issue HEAD with redirect-following DISABLED so we can read the
        // hub's initial response (which carries the offsite Location header
        // for paid resources). For hub-hosted, the first hop is a 303 to a
        // CDN URL — we then HEAD that CDN URL separately to read its
        // content-disposition.
        let url = format!("{BASE_URL}/resources/{slug}.{resource_id}/download");

        // ureq's `request` (not `head`) gives us a build-it-up handle; .send_string("")
        // with no body and method=HEAD avoids GET overhead. But ureq 2.x doesn't
        // expose a clean HEAD; the agent-level `.head(url)` does.
        let resp = match self
            .agent_no_redirect()
            .head(&url)
            .call()
        {
            Ok(r) => r,
            Err(ureq::Error::Status(404, _)) => return Ok(DownloadProbe::NotFound),
            Err(e) => return Err(anyhow!("HEAD {url}: {e}")),
        };

        let status = resp.status();
        let location = resp.header("location").map(str::to_string);
        let cd = resp.header("content-disposition").map(str::to_string);

        match (status, location, cd) {
            // 303 → CDN. Follow it and read content-disposition from the CDN.
            (303, Some(cdn_url), _) => {
                let cdn_resp = self
                    .agent
                    .head(&cdn_url)
                    .call()
                    .with_context(|| format!("HEAD CDN {cdn_url}"))?;
                let cdn_cd = cdn_resp
                    .header("content-disposition")
                    .map(str::to_string);
                let filename = cdn_cd
                    .as_deref()
                    .and_then(parse_content_disposition_filename);
                match filename {
                    Some(name) => Ok(DownloadProbe::Hosted { filename: name }),
                    None => Err(anyhow!(
                        "303 → CDN but no content-disposition filename on CDN response"
                    )),
                }
            }
            // 301 → offsite paywall. Capture the location URL.
            (301, Some(offsite), _) => Ok(DownloadProbe::Offsite { url: offsite }),
            // 200 directly with content-disposition (some hub-hosted may
            // serve inline without CDN redirect — defensive).
            (200, _, Some(cd_value)) => {
                match parse_content_disposition_filename(&cd_value) {
                    Some(name) => Ok(DownloadProbe::Hosted { filename: name }),
                    None => Err(anyhow!("200 with malformed content-disposition: {cd_value}")),
                }
            }
            // 200 with no content-disposition and no redirect — observed on
            // some older resources (e.g. weebuvr-ina-muscle-normals.11078).
            // Likely a server-side quirk: the download endpoint serves an
            // intermediate HTML page or treats the request as a page view
            // rather than an attachment. We can't extract a filename, so
            // treat it as if the resource is non-matchable from our angle.
            // Logging — not erroring — lets the sync continue past these.
            (200, _, None) => {
                eprintln!(
                    "HEAD 200 with no content-disposition for {url} — treating as NotFound"
                );
                Ok(DownloadProbe::NotFound)
            }
            (404, _, _) => Ok(DownloadProbe::NotFound),
            (s, _, _) => Err(anyhow!("unexpected HEAD status {s} for {url}")),
        }
    }

    /// Sibling of the main agent with redirect-following disabled, so we can
    /// read the hub's initial redirect status/Location without auto-following.
    /// Shares the same cookie jar implicitly via a fresh agent built with the
    /// same consent cookie — we don't need XF session continuity here.
    fn agent_no_redirect(&self) -> ureq::Agent {
        let mut jar = cookie_store::CookieStore::default();
        let base = url::Url::parse(BASE_URL).expect("BASE_URL parses");
        if let Ok(c) = cookie_store::Cookie::parse(
            "vamhubconsent=yes; Path=/; Domain=hub.virtamate.com",
            &base,
        ) {
            let _ = jar.insert(c, &base);
        }
        ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(30))
            .timeout_connect(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .redirects(0)
            .cookie_store(jar)
            .build()
    }

    /// Generic GET helper used by the devtools debug commands. Returns
    /// (status, final_url_after_redirects, body). Body capped at ~5 MB so
    /// a misdirected URL can't blow up memory.
    pub fn debug_get(&self, url: &str) -> Result<(u16, String, String)> {
        let resp = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let final_url = resp.get_url().to_string();
        // Cap to keep us out of trouble.
        const MAX: usize = 5 * 1024 * 1024;
        let mut buf = Vec::with_capacity(64 * 1024);
        let mut reader = resp.into_reader();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 { break; }
            if buf.len() + n > MAX { break; }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();
        Ok((status, final_url, body))
    }

    /// Fetch the full sitemap-derived resource catalog. Returns one entry per
    /// resource URL across all child sitemaps. ~30k+ entries; ~8 HTTP requests
    /// total. The hub publishes the sitemap explicitly so this is the
    /// blessed bulk-discovery path.
    pub fn fetch_sitemap_catalog(&self) -> Result<Vec<SitemapEntry>> {
        let index_url = format!("{BASE_URL}/sitemap.xml");
        let index_xml = self
            .agent
            .get(&index_url)
            .call()
            .with_context(|| format!("GET {index_url}"))?
            .into_string()
            .context("read sitemap index body")?;
        let child_urls = parse_sitemap_index(&index_xml)?;

        let mut out = Vec::new();
        for child in child_urls {
            let xml = self
                .agent
                .get(&child)
                .call()
                .with_context(|| format!("GET {child}"))?
                .into_string()
                .with_context(|| format!("read sitemap body {child}"))?;
            let mut entries = parse_sitemap_resources(&xml)?;
            out.append(&mut entries);
        }
        Ok(out)
    }

    /// Download an image (PNG/JPG/WebP) by URL.
    pub fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        if resp.status() != 200 {
            return Err(anyhow!("http {} for {url}", resp.status()));
        }
        // Cap at 25 MB — preview icons are tiny in practice (<100 KB).
        const MAX: usize = 25 * 1024 * 1024;
        let mut bytes = Vec::with_capacity(256 * 1024);
        let mut reader = resp.into_reader();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if bytes.len() + n > MAX {
                return Err(anyhow!("response > {MAX} bytes"));
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        Ok(bytes)
    }

    /// Fetch and parse a single resource page by (slug, resource_id). Used by
    /// the slug-match tier in `sync_one_creator` (commands.rs): when a local
    /// package's normalized name matches an entry in the cached
    /// `hub_resources` catalog but the XF per-author search missed it, this
    /// gives us a HubMatch shaped identically to what `parse_search_results`
    /// produces — so downstream HEAD-probe + filename_map + persist logic
    /// treats slug-match candidates uniformly.
    ///
    /// Caller is expected to HEAD-probe the resulting HubMatch's
    /// `/resources/{slug}.{id}/download` to verify the CDN filename matches
    /// the local (Creator, Package). The page fetch here only provides the
    /// enrichment metadata (title, author, category, billing, license, icon).
    pub fn fetch_resource_page(&self, slug: &str, resource_id: i64) -> Result<HubMatch> {
        let url = format!("{BASE_URL}/resources/{slug}.{resource_id}/");
        let resp = self
            .agent
            .get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if status == 404 {
            return Err(anyhow!("resource {resource_id} not found (404)"));
        }
        if status != 200 {
            return Err(anyhow!("unexpected status {status} for {url}"));
        }
        // Cap body at 5 MB — resource pages run ~100-200 KB in practice, but
        // a runaway response shouldn't OOM us.
        const MAX: usize = 5 * 1024 * 1024;
        let mut buf = Vec::with_capacity(128 * 1024);
        let mut reader = resp.into_reader();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            if buf.len() + n > MAX {
                return Err(anyhow!("resource page > {MAX} bytes"));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();
        parse_resource_page(&body, resource_id, slug)
    }
}

fn is_gate_page(html: &str) -> bool {
    html.contains("Adult Content Warning")
        && html.contains("vamhubconsent")
        && !html.contains("class=\"p-body")
}

fn extract_csrf_token(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"input[name="_xfToken"]"#).ok()?;
    doc.select(&sel)
        .next()?
        .value()
        .attr("value")
        .map(String::from)
}

/// Parses a XF search results page. Results may include forum threads (link
/// starts with `/threads/`) AND resource pages (link starts with `/resources/`);
/// we keep only the latter.
fn parse_search_results(html: &str) -> Result<Vec<HubMatch>> {
    let doc = Html::parse_document(html);

    let item_sel = Selector::parse("li.block-row.searchItemContainer")
        .map_err(|e| anyhow!("selector: {e:?}"))?;
    let resource_link_sel =
        Selector::parse(r#"h3.contentRow-title a[href^="/resources/"]"#)
            .map_err(|e| anyhow!("{e:?}"))?;
    let author_sel = Selector::parse(".contentRow-minor a.username").map_err(|e| anyhow!("{e:?}"))?;
    let icon_img_sel = Selector::parse(".searchResultIcon img, .contentRow-figure img")
        .map_err(|e| anyhow!("{e:?}"))?;
    // Three label slots per row, per Cowork recon 2026-05-17:
    //   - silver = content category with optional billing-tier prefix
    //   - green  = "Hub-Hosted VAR" badge (presence-only signal)
    //   - third  = license code (CC BY / CC BY-NC / PC / FC / ...)
    // The third slot has variable class names ("cclicenseccby", "label--..."),
    // so we pick it by elimination: any .label that isn't silver or green.
    let silver_label_sel =
        Selector::parse(".label.label--silver").map_err(|e| anyhow!("{e:?}"))?;
    let green_label_sel =
        Selector::parse(".label.label--green").map_err(|e| anyhow!("{e:?}"))?;
    let any_label_sel = Selector::parse(".label").map_err(|e| anyhow!("{e:?}"))?;
    let title_em_sel = Selector::parse("h3.contentRow-title a em.textHighlight")
        .map_err(|e| anyhow!("{e:?}"))?;

    let mut out = Vec::new();
    for item in doc.select(&item_sel) {
        let Some(link) = item.select(&resource_link_sel).next() else { continue };
        let href = link.value().attr("href").unwrap_or("");
        let Some(rid) = extract_resource_id_from_url(href) else { continue };

        // Title: prefer the highlighted <em> inside the link; fall back to
        // link text minus any label prefix.
        let title = item
            .select(&title_em_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let raw: String = link.text().collect();
                raw.trim().to_string()
            });
        if title.is_empty() {
            continue;
        }

        let url = format!("{BASE_URL}{href}");

        let author = item
            .select(&author_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        // Silver label carries the category — and, when paid, a "Paid " or
        // "Paid Early-Access " prefix that we strip into a separate field.
        let silver_text = item
            .select(&silver_label_sel)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty());
        let (billing_tier, category) = match silver_text {
            Some(s) => {
                let (tier, cat) = strip_billing_prefix(&s);
                (tier, Some(cat.to_string()))
            }
            None => (None, None),
        };

        // Green "Hub-Hosted VAR" label is presence-only; we don't read its text.
        let is_hub_hosted = item.select(&green_label_sel).next().is_some();

        // Third label slot (license). Skip silver and green; first remaining
        // .label is the license code if present.
        let license = item
            .select(&any_label_sel)
            .find(|el| {
                let classes = el.value().attr("class").unwrap_or("");
                !classes.contains("label--silver") && !classes.contains("label--green")
            })
            .map(|el| el.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty());

        let preview_url = item
            .select(&icon_img_sel)
            .next()
            .and_then(|el| el.value().attr("src"))
            .map(String::from);

        out.push(HubMatch {
            resource_id: rid,
            url,
            title,
            author,
            category,
            billing_tier,
            is_hub_hosted,
            license,
            preview_url,
            tagline: None,
            updated_at: None,
        });
    }
    Ok(out)
}

/// Parse a single `/resources/{slug}.{id}/` page into a `HubMatch`. Used by
/// the slug-match tier when the cached `hub_resources` sitemap catalog
/// surfaces a candidate that XF's per-author search missed.
///
/// The page DOM differs from search results:
///   - title sits in `h1.p-title-value` with the silver-label category
///     prepended inline (we slice it off)
///   - author + license + hub-hosted indicator live under `div.p-description`
///   - the resource icon is `.resource-header-avatar img`
///
/// Returns `Err` if the page is the age gate (caller should treat the
/// whole sync as gated, same protocol as `search_resources_by_user`).
fn parse_resource_page(html: &str, resource_id: i64, slug: &str) -> Result<HubMatch> {
    if is_gate_page(html) {
        return Err(anyhow!("gate: resource page returned the age gate"));
    }
    let doc = Html::parse_document(html);

    let title_sel =
        Selector::parse("h1.p-title-value").map_err(|e| anyhow!("title sel: {e:?}"))?;
    let title_node = doc
        .select(&title_sel)
        .next()
        .ok_or_else(|| anyhow!("no h1.p-title-value on resource page (slug={slug})"))?;

    // Silver label inside the title carries the category, with optional
    // billing-tier prefix. Strip identically to the search-result parser.
    let silver_sel = Selector::parse(".label.label--silver")
        .map_err(|e| anyhow!("silver sel: {e:?}"))?;
    let silver_text = title_node
        .select(&silver_sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());
    let (billing_tier, category) = match silver_text.as_deref() {
        Some(s) => {
            let (tier, cat) = strip_billing_prefix(s);
            (tier, Some(cat.to_string()))
        }
        None => (None, None),
    };

    // Title = direct text children of h1 only (excludes the silver label's
    // content, which lives inside a child <span>). This avoids the fragile
    // strip-prefix-of-concatenated-text approach.
    let title: String = title_node
        .children()
        .filter_map(|n| {
            if let scraper::Node::Text(t) = n.value() {
                Some(t.text.to_string())
            } else {
                None
            }
        })
        .collect();
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(anyhow!("empty title on resource page (slug={slug})"));
    }

    // Author + labels live inside the description block. Scoping selectors
    // to `.p-description` prevents picking up green/license labels from
    // the recommendation strip further down the page.
    let desc_sel =
        Selector::parse(".p-description").map_err(|e| anyhow!("desc sel: {e:?}"))?;
    let desc = doc.select(&desc_sel).next();

    let author_sel =
        Selector::parse("a.username").map_err(|e| anyhow!("author sel: {e:?}"))?;
    let author = desc
        .and_then(|d| d.select(&author_sel).next())
        .map(|el| el.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    let green_sel =
        Selector::parse(".label.label--green").map_err(|e| anyhow!("green sel: {e:?}"))?;
    let is_hub_hosted = desc
        .map(|d| d.select(&green_sel).next().is_some())
        .unwrap_or(false);

    // License: real-page markup wraps the license label in a wiki link.
    // Match either form to be robust to future template tweaks.
    let license_sel = Selector::parse(
        r#"a[href*="license_help"] .label, .label[class*="cclicense"]"#,
    )
    .map_err(|e| anyhow!("license sel: {e:?}"))?;
    let license = desc
        .and_then(|d| d.select(&license_sel).next())
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());

    let preview_sel = Selector::parse(".resource-header-avatar img")
        .map_err(|e| anyhow!("preview sel: {e:?}"))?;
    let preview_url = doc
        .select(&preview_sel)
        .next()
        .and_then(|el| el.value().attr("src"))
        .map(String::from);

    Ok(HubMatch {
        resource_id,
        url: format!("{BASE_URL}/resources/{slug}.{resource_id}/"),
        title,
        author,
        category,
        billing_tier,
        is_hub_hosted,
        license,
        preview_url,
        tagline: None,
        updated_at: None,
    })
}

/// Pull the billing-tier prefix off a silver-label string. Examples:
///   "Looks"                  → (None,                "Looks")
///   "Paid Looks"             → (Some("paid"),        "Looks")
///   "Paid Early-Access Looks"→ (Some("paid-early-access"), "Looks")
/// Returns canonical lowercase-kebab tier names so they map cleanly to a
/// future enum / chip filter value.
fn strip_billing_prefix(s: &str) -> (Option<String>, &str) {
    // Order matters: longest prefix first so "Paid Early-Access" doesn't get
    // partially matched as "Paid ".
    if let Some(rest) = s.strip_prefix("Paid Early-Access ") {
        return (Some("paid-early-access".to_string()), rest.trim());
    }
    if let Some(rest) = s.strip_prefix("Paid ") {
        return (Some("paid".to_string()), rest.trim());
    }
    (None, s.trim())
}

fn extract_resource_id_from_url(href: &str) -> Option<i64> {
    let trimmed = href.trim_matches('/');
    let last = trimmed.rsplit('/').next()?;
    let id_part = last.rsplit('.').next()?;
    id_part.parse::<i64>().ok()
}

/// Pull `(slug, resource_id)` from a hub resource URL like
/// `/resources/forest-assetbundle.37103/`.
pub fn extract_slug_and_id_from_url(href: &str) -> Option<(String, i64)> {
    let trimmed = href.trim_matches('/');
    let last = trimmed.rsplit('/').next()?;
    // `forest-assetbundle.37103` — split on the LAST `.` so slugs with
    // embedded dots (rare but possible) don't break us.
    let dot = last.rfind('.')?;
    let slug = &last[..dot];
    let id_part = &last[dot + 1..];
    let id = id_part.parse::<i64>().ok()?;
    Some((slug.to_string(), id))
}

/// Look at a search-results page and return the largest page number visible
/// in the page-nav block. Returns `None` if no pagenav (single-page result).
fn parse_last_page(html: &str) -> Option<u32> {
    let doc = Html::parse_document(html);
    let nav_sel = Selector::parse(".pageNavWrapper .pageNav-main a").ok()?;
    doc.select(&nav_sel)
        .filter_map(|el| el.text().collect::<String>().trim().parse::<u32>().ok())
        .max()
}

/// Parse an RFC-6266 Content-Disposition header value and pull `filename`
/// (the basic, non-extended form — XF/CDN don't use filename*=UTF-8'').
/// Handles both quoted and unquoted forms.
fn parse_content_disposition_filename(value: &str) -> Option<String> {
    // Walk parameter list. Expected shape:
    //   attachment; filename="X.var"
    //   filename="X.var"    (no disposition type — observed on the CDN response)
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            let v = rest.trim();
            let unquoted = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(v);
            if !unquoted.is_empty() {
                return Some(unquoted.to_string());
            }
        }
    }
    None
}

/// Parse a sitemap index (root sitemap.xml) — `<sitemapindex><sitemap><loc>...`.
/// Returns the list of child sitemap URLs.
fn parse_sitemap_index(xml: &str) -> Result<Vec<String>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut in_loc = false;
    loop {
        match reader.read_event() {
            Err(e) => return Err(anyhow!("xml parse: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"loc" => {
                in_loc = true;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"loc" => {
                in_loc = false;
            }
            Ok(Event::Text(e)) if in_loc => {
                let text = e.unescape().map_err(|err| anyhow!("xml unescape: {err}"))?;
                let url = text.trim().to_string();
                if !url.is_empty() {
                    out.push(url);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Parse a child sitemap XML — `<urlset><url><loc>...</loc><lastmod>...</lastmod></url>`.
/// Filters to entries whose `loc` is a resource page (URL pattern
/// `/resources/{slug}.{id}/`), skipping thread/category/other URLs.
fn parse_sitemap_resources(xml: &str) -> Result<Vec<SitemapEntry>> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut cur_loc: Option<String> = None;
    let mut cur_lastmod: Option<i64> = None;
    enum Field {
        None,
        Loc,
        Lastmod,
    }
    let mut field = Field::None;

    loop {
        match reader.read_event() {
            Err(e) => return Err(anyhow!("xml parse: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"loc" => field = Field::Loc,
                b"lastmod" => field = Field::Lastmod,
                b"url" => {
                    cur_loc = None;
                    cur_lastmod = None;
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"loc" | b"lastmod" => field = Field::None,
                b"url" => {
                    // Emit if this <url> was a resource page and we got both
                    // loc + a parseable lastmod.
                    if let (Some(loc), Some(lm)) = (cur_loc.as_deref(), cur_lastmod) {
                        if is_resource_url(loc) {
                            // Strip the base prefix to extract slug+id from
                            // the URL path, regardless of scheme/host.
                            if let Some(path) = path_from_url(loc) {
                                if let Some((slug, id)) =
                                    extract_slug_and_id_from_url(&path)
                                {
                                    out.push(SitemapEntry { resource_id: id, slug, lastmod: lm });
                                }
                            }
                        }
                    }
                    cur_loc = None;
                    cur_lastmod = None;
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| anyhow!("xml unescape: {err}"))?;
                let s = text.trim();
                match field {
                    Field::Loc if !s.is_empty() => cur_loc = Some(s.to_string()),
                    Field::Lastmod if !s.is_empty() => {
                        cur_lastmod = parse_iso8601_to_unix(s);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn is_resource_url(loc: &str) -> bool {
    let needle = "/resources/";
    let Some(idx) = loc.find(needle) else { return false };
    let after = &loc[idx + needle.len()..];
    // Skip category index pages (/resources/categories/...) and the root
    // (/resources/ or /resources/?).
    if after.starts_with("categories/") {
        return false;
    }
    if after.is_empty() || after.starts_with('?') {
        return false;
    }
    // Must look like {slug}.{digits}/ — accept anything else only if the
    // extracted id parses.
    true
}

fn path_from_url(url: &str) -> Option<String> {
    if url.starts_with('/') {
        return Some(url.to_string());
    }
    let parsed = url::Url::parse(url).ok()?;
    Some(parsed.path().to_string())
}

/// Parse a sitemap-style ISO 8601 timestamp (e.g. "2020-05-11T16:40:26+00:00")
/// into unix seconds. Lenient — only the date+time portion is required;
/// timezone offset is honored when present, otherwise treated as UTC.
fn parse_iso8601_to_unix(s: &str) -> Option<i64> {
    // Parse without pulling in a date crate. Expected form:
    //   YYYY-MM-DDTHH:MM:SS[+HH:MM|-HH:MM|Z]
    let bytes = s.as_bytes();
    if bytes.len() < 19 { return None; }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    let h: u32 = s.get(11..13)?.parse().ok()?;
    let mi: u32 = s.get(14..16)?.parse().ok()?;
    let se: u32 = s.get(17..19)?.parse().ok()?;
    let mut tz_off: i64 = 0;
    if bytes.len() > 19 {
        let tail = &s[19..];
        if tail != "Z" {
            // tail like "+00:00" or "-07:00"
            let sign = match tail.chars().next() {
                Some('+') => 1,
                Some('-') => -1,
                _ => 0,
            };
            if sign != 0 && tail.len() >= 6 {
                let hh: i64 = tail.get(1..3)?.parse().ok()?;
                let mm: i64 = tail.get(4..6)?.parse().ok()?;
                tz_off = sign * (hh * 3600 + mm * 60);
            }
        }
    }
    let unix_utc = days_from_civil(y, mo, d) * 86_400
        + h as i64 * 3600
        + mi as i64 * 60
        + se as i64;
    Some(unix_utc - tz_off)
}

/// Days since 1970-01-01 for a civil date, via Howard Hinnant's algorithm.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m as i64 - 3 } else { m as i64 + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + doe - 719468
}

/// Score how well a search hit matches a (creator, package_name) pair. Returns
/// 0 if author isn't a clear match. Higher is better.
///
/// Normalization strips non-alphanumeric and lowercases everything, so
/// "AcidBubbles" matches "Acid Bubbles" or "acid-bubbles".
/// Score how well a (search hit's) title + author matches a local
/// (creator, package_name) pair. Returns 0 unless BOTH:
///   - author matches (exact or substring, normalized)
///   - title has actual textual evidence aligning with package_name
///
/// Bare author-match alone is intentionally worth 0 — many creators have
/// generic-title bundle resources on the hub (e.g. "Skynet's Custom Looks
/// Pack"), and rewarding author-only would collapse every unmatched local
/// onto the first such resource (the Skynet sync bug discovered 2026-05-18).
pub fn score_match(creator: &str, package_name: &str, hit: &HubMatch) -> u32 {
    let creator_n = normalize_compare(creator);
    let author_n = normalize_compare(&hit.author);
    let pkg_n = normalize_compare(package_name);
    let title_n = normalize_compare(&hit.title);

    let author_match = creator_n == author_n
        || (!creator_n.is_empty() && author_n.contains(&creator_n))
        || (!author_n.is_empty() && creator_n.contains(&author_n));
    if !author_match {
        return 0;
    }
    if title_n.is_empty() || pkg_n.is_empty() {
        return 0;
    }

    if title_n == pkg_n {
        return 200;
    }
    if title_n.contains(&pkg_n) {
        return 100;
    }
    if pkg_n.contains(&title_n) {
        return 60;
    }

    // No token-overlap fallback. A bare 1-token overlap (e.g. "Ashley" or
    // "Overwatch" or "Bundle") used to score 10 and would win when nothing
    // better matched, causing distinct packages to collapse onto one hub
    // resource (Skynet sync bug, 2026-05-18). False negatives from
    // dropping this branch are preferable to misleading false-positive
    // pins — a missing match is honest, a wrong match isn't.
    0
}

/// Lowercase + drop non-alphanumeric. Shared with `commands.rs` for the
/// slug-match tier (which normalizes local `package_name` and cached
/// `hub_resources.slug` the same way before comparing).
pub fn normalize_compare(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed but structurally faithful fixture of a real free/hub-hosted
    /// resource page (modeled after /resources/collider-editor.183/).
    const FIXTURE_FREE_HOSTED: &str = r##"<!DOCTYPE html>
<html><body>
<div class="resource-view-header">
  <div class="resource-header-avatar">
    <img src="https://cdn.example/data/resource_icons/0/183.jpg" alt="Collider Editor">
  </div>
  <div class="p-title-description">
    <div class="p-title">
      <h1 class="p-title-value">
        <span class="label label--silver" dir="auto">Plugins + Scripts</span><span class="label-append">&nbsp;</span>Collider Editor
      </h1>
    </div>
    <div class="p-description">
      <ul class="listInline listInline--bullet">
        <li><a href="/members/acid-bubbles.18/" class="username">Acid Bubbles</a></li>
        <li><span class="label label--green">Hub-Hosted VAR</span></li>
        <li><a href="https://hub.virtamate.com/wiki/license_help/"><span class="label cclicenseccbysa">CC BY-SA</span></a></li>
      </ul>
    </div>
  </div>
</div>
<div class="resource-recommendations">
  <!-- Recommendation strip; must NOT bleed into our scoped selectors. -->
  <span class="label label--green">Hub-Hosted VAR</span>
  <a class="username" href="/members/somebody.99/">Somebody</a>
</div>
</body></html>"##;

    /// Fixture for a paid resource (silver label = "Paid Looks") to confirm
    /// the billing-tier prefix gets stripped into a separate field, and that
    /// absent green-label means is_hub_hosted = false.
    const FIXTURE_PAID: &str = r##"<!DOCTYPE html>
<html><body>
<div class="resource-header-avatar">
  <img src="https://cdn.example/data/resource_icons/12/12345.jpg" alt="Some Look">
</div>
<div class="p-title-description">
  <div class="p-title">
    <h1 class="p-title-value">
      <span class="label label--silver">Paid Looks</span><span class="label-append">&nbsp;</span>Some Look
    </h1>
  </div>
  <div class="p-description">
    <a href="/members/some-author.99/" class="username">SomeAuthor</a>
  </div>
</div>
</body></html>"##;

    #[test]
    fn parses_free_hosted_resource_page() {
        let hm = parse_resource_page(FIXTURE_FREE_HOSTED, 183, "collider-editor").unwrap();
        assert_eq!(hm.resource_id, 183);
        assert_eq!(hm.title, "Collider Editor");
        assert_eq!(hm.author, "Acid Bubbles");
        assert_eq!(hm.category.as_deref(), Some("Plugins + Scripts"));
        assert_eq!(hm.billing_tier, None);
        assert!(hm.is_hub_hosted);
        assert_eq!(hm.license.as_deref(), Some("CC BY-SA"));
        assert!(hm
            .preview_url
            .as_deref()
            .unwrap()
            .contains("/data/resource_icons/0/183.jpg"));
        assert_eq!(
            hm.url,
            "https://hub.virtamate.com/resources/collider-editor.183/"
        );
    }

    #[test]
    fn parses_paid_resource_page() {
        let hm = parse_resource_page(FIXTURE_PAID, 12345, "some-look").unwrap();
        assert_eq!(hm.title, "Some Look");
        assert_eq!(hm.author, "SomeAuthor");
        assert_eq!(hm.category.as_deref(), Some("Looks"));
        assert_eq!(hm.billing_tier.as_deref(), Some("paid"));
        assert!(!hm.is_hub_hosted);
        assert!(hm.license.is_none());
    }

    #[test]
    fn rejects_gate_page() {
        let gate = "<html><body>Adult Content Warning vamhubconsent</body></html>";
        let res = parse_resource_page(gate, 1, "x");
        let err = res.expect_err("gate page must be rejected");
        assert!(format!("{err}").contains("gate"), "got: {err}");
    }
}

