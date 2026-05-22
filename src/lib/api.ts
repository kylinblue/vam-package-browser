import { invoke } from "@tauri-apps/api/core";

export type PackageType =
  | "Scene"
  | "Look"
  | "Morph"
  | "Texture"
  | "Clothing"
  | "Hair"
  | "Plugin"
  | "Asset"
  | "Pose"
  | "Sound"
  | "SubScene"
  | "Mixed"
  | "Unknown";

export interface PackageRow {
  id: number;
  creator: string;
  package_name: string;
  version: string;
  license: string | null;
  program_version: string | null;
  description: string | null;
  package_type: PackageType;
  content_count: number;
  dep_count: number;
  file_size: number;
  file_mtime: number;
  /** Max last-modified timestamp across all entries inside the .var
   *  (i.e. when the author zipped the package). 0 = unknown / pre-v6 scan. */
  package_mtime: number;
  var_path: string;
  has_preview: boolean;
  is_favorite: boolean;
  is_hidden: boolean;
  hub_resource_id: number | null;
  hub_url: string | null;
  hub_title: string | null;
  hub_author: string | null;
  hub_category: string | null;
  hub_preview_url: string | null;
  hub_synced_at: number | null;
  hub_sync_state: string | null;
  scene_count: number;
  look_count: number;
  plugin_count: number;
  clothing_count: number;
  hair_count: number;
  pose_count: number;
  subscene_count: number;
  error: string | null;
  /** Family-level v4 tags as namespaced strings (e.g. "kind:character-look").
   *  Empty when the package has no `family_id` or the family has no tags. */
  tags: string[];

  /** Hub v14/v15 fields. All NULL until a hub sync resolves the package. */
  hub_billing_tier: string | null;
  hub_is_hub_hosted: number | null;
  hub_license: string | null;
  hub_lastmod: number | null;
  hub_external_url: string | null;
  /** "filename" | "fuzzy_title" | "slug_match" | "manual" | null. */
  hub_match_method: string | null;
}

export type SortField =
  | "name"
  | "creator"
  | "size"
  | "mtime"
  | "package_mtime"
  | "scanned";
export type SortOrder = "asc" | "desc";

export interface QueryFilter {
  search?: string;
  creator?: string;
  package_type?: PackageType;
  missing_preview?: boolean;
  favorites_only?: boolean;
  include_hidden?: boolean;
  /** File size lower bound in bytes (inclusive). */
  min_size?: number;
  /** File size upper bound in bytes (inclusive). */
  max_size?: number;
  /** File mtime lower bound, unix seconds (inclusive). */
  min_mtime?: number;
  /** File mtime upper bound, unix seconds (inclusive). */
  max_mtime?: number;
  /** Package (inner zip) mtime lower bound, unix seconds (inclusive). */
  min_package_mtime?: number;
  /** Package (inner zip) mtime upper bound, unix seconds (inclusive). */
  max_package_mtime?: number;
  sort_by?: SortField;
  sort_order?: SortOrder;
  limit?: number;
  offset?: number;
  /** Family-level v4 tag filter. Tags sharing a namespace are OR'd (any match
   *  within a facet column); across namespaces they are AND'd (every selected
   *  facet must be satisfied). Packages without a family_id can never match a
   *  non-empty tag filter. */
  tags?: string[];
  /** Hub category filter (Fetched mode primary axis). Exact-match on the
   *  canonical display string returned by the hub (e.g. "Looks"). */
  hub_category?: string;
  /** When true, restrict to packages with no hub_category (= not currently
   *  matched). Mutually exclusive with hub_category — hub_category wins
   *  if both are set. */
  hub_unmatched?: boolean;
}

export interface Namespace {
  namespace: string;
  /** Either the literal string `"any"` or a JSON array of `kind:` values that
   *  this namespace applies to (as a raw JSON string — parse if needed). */
  applies_to_json: string | null;
  cardinality: string | null;
  /** Distinct taxonomy values defined under this namespace (is_active=1). */
  value_count: number;
  /** Number of families with at least one tag in this namespace. */
  family_count: number;
}

export interface TagCount {
  tag: string;
  count: number;
}

export async function listNamespaces(): Promise<Namespace[]> {
  return invoke<Namespace[]>("list_namespaces");
}

/** Per-tag family counts, optionally restricted to one namespace. */
export async function listTagCounts(namespace?: string): Promise<TagCount[]> {
  return invoke<TagCount[]>("list_tag_counts", { namespace });
}

export interface SearchResult {
  family_id: number;
  /** `null` only when the family has no `latest_package_id` set — shouldn't
   *  happen for a tagged family but the UI should defend against it. */
  package_id: number | null;
  creator: string;
  package_name: string;
  /** The embedded text — one-line snippet of why the family matched. */
  purpose: string | null;
  /** Raw cosine score. Scale varies by model; normalize within a result set
   *  if rendering as a bar. */
  score: number;
}

/** Natural-language semantic search over family embeddings. */
export async function searchFamilies(
  query: string,
  topN?: number,
): Promise<SearchResult[]> {
  return invoke<SearchResult[]>("search_families", { query, topN });
}

/** "Find similar to this" — anchor by a package_id (we look up its family
 *  internally so the frontend doesn't have to track family ids). */
export async function searchSimilarFamilies(
  packageId: number,
  topN?: number,
): Promise<SearchResult[]> {
  return invoke<SearchResult[]>("search_similar_families", {
    packageId,
    topN,
  });
}

/** Fetch PackageRow data for a set of ids, preserving the caller's order.
 *  Used to materialize semantic-search hits as proper grid rows. */
export async function getPackagesByIds(ids: number[]): Promise<PackageRow[]> {
  return invoke<PackageRow[]>("get_packages_by_ids", { ids });
}

export interface TypeCount {
  package_type: PackageType;
  count: number;
}

export interface ScanProgress {
  scanned: number;
  total: number;
  current_path: string | null;
  errors: number;
}

export interface ScanResult {
  scanned: number;
  errors: number;
  elapsed_ms: number;
}

export async function scanLibrary(
  addonRoot: string,
  limit: number | null,
): Promise<ScanResult> {
  return invoke<ScanResult>("scan_library", { addonRoot, limit });
}

export async function queryPackages(
  filter: QueryFilter,
): Promise<PackageRow[]> {
  return invoke<PackageRow[]>("query_packages", { filter });
}

export async function countPackages(
  filter: QueryFilter,
): Promise<number> {
  return invoke<number>("count_packages", { filter });
}

export async function listCreators(): Promise<string[]> {
  return invoke<string[]>("list_creators");
}

export interface CreatorCount {
  creator: string;
  count: number;
}

export async function listCreatorsWithCounts(): Promise<CreatorCount[]> {
  return invoke<CreatorCount[]>("list_creators_with_counts");
}

export async function openExternalUrl(url: string): Promise<void> {
  return invoke("open_external_url", { url });
}

/// Hub search scoped to a creator's posts. Uses XF's *post-redirect* URL
/// shape (`c[users]=…&o=relevance`) — not the POST form-field names used in
/// hub.rs, which are XF-internal and don't pre-fill the form on GET.
export function vamHubAuthorSearchUrl(creator: string): string {
  const u = encodeURIComponent(creator);
  return `https://hub.virtamate.com/search/?c[users]=${u}&o=relevance`;
}

/// Hub resource-search by package keyword. `_` → space because VAM package
/// names use snake_case while hub titles use spaces, and the keyword search
/// matches on title tokens. `q` + `t=resource` is XF's GET-side URL shape.
export function vamHubPackageSearchUrl(packageName: string): string {
  const q = encodeURIComponent(packageName.replace(/_/g, " "));
  return `https://hub.virtamate.com/search/?q=${q}&t=resource&o=relevance`;
}

export async function listTypeCounts(): Promise<TypeCount[]> {
  return invoke<TypeCount[]>("list_type_counts");
}

export interface HubCategoryCount {
  hub_category: string;
  count: number;
}

export async function listHubCategories(): Promise<HubCategoryCount[]> {
  return invoke<HubCategoryCount[]>("list_hub_categories");
}

/** Count of non-hidden packages with no hub_category (= not currently matched).
 *  Drives the "(unidentified)" virtual chip alongside listHubCategories. */
export async function countHubUnidentified(): Promise<number> {
  return invoke<number>("count_hub_unidentified");
}

export async function setFavorite(id: number, value: boolean): Promise<void> {
  return invoke("set_favorite", { id, value });
}

export async function setHidden(id: number, value: boolean): Promise<void> {
  return invoke("set_hidden", { id, value });
}

export async function revealInFolder(path: string): Promise<void> {
  return invoke("reveal_in_folder", { path });
}

export async function getSettings(): Promise<{ addon_root: string | null }> {
  return invoke("get_settings");
}

export async function setAddonRoot(path: string): Promise<void> {
  return invoke("set_addon_root", { path });
}

export interface ThumbProgress {
  id: number;
  ok: boolean;
  done: number;
  total: number;
  error: string | null;
}

export interface ThumbGenSummary {
  considered: number;
  generated: number;
  already_fresh: number;
  errors: number;
  elapsed_ms: number;
}

export async function generateThumbnails(): Promise<ThumbGenSummary> {
  return invoke<ThumbGenSummary>("generate_thumbnails");
}

/// Returns the URL the webview should fetch to render the thumb for a package.
/// On Windows our custom `thumb://` protocol is exposed as `http://thumb.localhost/...`.
/// Append a version query param to bust the in-memory image cache when a fresh
/// thumb is generated for that package.
export function thumbUrl(packageId: number, version: number): string {
  return `http://thumb.localhost/${packageId}?v=${version}`;
}

/// Threshold above which the gallery shows a "huge image, click to view"
/// placeholder instead of streaming the source bytes. Matches the backend's
/// default pull-display cap; opt-in `allowHuge` bypasses both.
export const HUGE_IMAGE_BYTES = 50 * 1024 * 1024;

/// Per-image (sub) thumbnail URL: the Rust protocol handler streams the source
/// bytes from the zip entry directly to the browser (no thumbnail generation).
/// Pass `allowHuge=true` to lift the backend's 50MB cap (used for the hero,
/// where the user has explicitly chosen to view a single image).
export function subThumbUrl(
  packageId: number,
  entryPath: string,
  allowHuge: boolean = false,
): string {
  // UTF-8 → bytes → URL-safe base64 (RFC 4648 §5). btoa() only accepts Latin1,
  // so paths with non-ASCII characters (Japanese / Korean / accented filenames)
  // need explicit UTF-8 encoding first.
  const utf8 = new TextEncoder().encode(entryPath);
  let binary = "";
  for (const b of utf8) binary += String.fromCharCode(b);
  const b64 = btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  const suffix = allowHuge ? "?big=1" : "";
  return `http://thumb.localhost/${packageId}/img/${b64}${suffix}`;
}

export interface ImageEntry {
  path: string;
  size: number;
}

export interface HubSyncOptions {
  creator?: string;
  package_name?: string;
  only_missing?: boolean;
  pull_preview_for_no_thumb?: boolean;
  rate_limit_ms?: number;
  /** Number of creator-level workers running in parallel. Default 3. */
  workers?: number;
}

export interface HubSyncProgress {
  done: number;
  total: number;
  matched: number;
  not_found: number;
  failed: number;
  previews_pulled: number;
  current: string;
  current_status: string;
}

export interface HubSyncSummary {
  considered: number;
  matched: number;
  not_found: number;
  failed: number;
  previews_pulled: number;
  elapsed_ms: number;
  gated: boolean;
}

export async function startHubSync(options: HubSyncOptions = {}): Promise<HubSyncSummary> {
  return invoke<HubSyncSummary>("start_hub_sync", { options });
}

export async function stopHubSync(): Promise<void> {
  return invoke("stop_hub_sync");
}

export interface HubCatalogRefreshSummary {
  total_fetched: number;
  elapsed_ms: number;
}

export async function hubCatalogRefresh(): Promise<HubCatalogRefreshSummary> {
  return invoke<HubCatalogRefreshSummary>("hub_catalog_refresh");
}

export interface HubStatus {
  catalog_rows: number;
  catalog_latest_fetched_at: number | null;
  catalog_latest_lastmod: number | null;
  total_packages: number;
  matched: number;
  matched_by_filename: number;
  matched_by_fuzzy_title: number;
  matched_by_slug_match: number;
  not_found: number;
  failed: number;
  never_synced: number;
  /** `[category, count]` pairs for matched packages, sorted by count desc. */
  top_categories: [string, number][];
  /** `[billing_tier, count]` pairs for matched packages. */
  by_billing_tier: [string, number][];
}

export async function hubStatus(): Promise<HubStatus> {
  return invoke<HubStatus>("hub_status");
}

/** True while a hub sync is running in the Rust backend. Survives frontend
 *  HMR reloads — useful for resuming the UI's "running" state after a refresh. */
export async function hubSyncActive(): Promise<boolean> {
  return invoke<boolean>("hub_sync_active");
}

export interface HubSyncLog {
  level: "info" | "warn" | "error" | string;
  message: string;
  /** Unix seconds. */
  timestamp: number;
}

export interface PackageDetail {
  package: PackageRow;
  content_list: string[];
  dependencies: string[];
  instructions: string | null;
  images: ImageEntry[];
  preview_path: string | null;
}

export async function getPackageDetail(id: number): Promise<PackageDetail> {
  return invoke<PackageDetail>("get_package_detail", { id });
}

/** One row in the depends-on or used-by side of a relationships query.
 *  `id == null` means the dep didn't resolve to a local package — the user
 *  doesn't have this dependency installed. */
export interface RelatedPackage {
  id: number | null;
  raw_dep_key: string;
  creator: string | null;
  package_name: string | null;
  version: string | null;
  package_type: PackageType | null;
  has_preview: boolean;
  is_hidden: boolean;
}

export interface PackageRelationships {
  depends_on: RelatedPackage[];
  used_by: RelatedPackage[];
}

export async function getPackageRelationships(
  id: number,
): Promise<PackageRelationships> {
  return invoke<PackageRelationships>("get_package_relationships", { id });
}

/** Rebuild the resolved dep-link table from the current raw deps. Cheap; runs
 *  inside one DB transaction. The scanner already calls this at the end of
 *  every scan — surfacing it as a command for the dev/maintenance case. */
export async function resolveDependencies(): Promise<void> {
  return invoke("resolve_dependencies");
}
