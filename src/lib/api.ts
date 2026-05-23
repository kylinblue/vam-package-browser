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
  /** "filename" | "fuzzy_title" | "slug_match" | "manual" | "override" | "inherit" | null. */
  hub_match_method: string | null;
  /** User-override lock flags (0/1). When 1, the corresponding field
   *  resists auto-sync overwrites — sync writers honor these via CASE
   *  expressions; the scanner does the same for package_type_manual. */
  hub_category_manual: number;
  hub_author_manual: number;
  package_type_manual: number;
  /** Pristine pre-override snapshots. Populated by set_* on first
   *  override; restored by clear_override. NULL when never overridden
   *  or after a restore. UI shows "X (was Y)" when both are present. */
  hub_category_original: string | null;
  hub_author_original: string | null;
  package_type_original: string | null;
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

export interface Settings {
  addon_root: string | null;
  /** Where the real .var files live post-setup. Null pre-setup. */
  managed_root: string | null;
  /** True once the one-time visibility-presets setup migration finished. */
  setup_complete: boolean;
  /** Unix seconds when setup completed. */
  setup_completed_at: number | null;
}

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

export async function setAddonRoot(path: string): Promise<void> {
  return invoke("set_addon_root", { path });
}

// --- Visibility-presets setup wizard ---------------------------------------

export interface SetupState {
  setup_complete: boolean;
  addon_root: string | null;
  managed_root: string | null;
  managed_volume_id: number | null;
  setup_completed_at: number | null;
  /** True when managed_root is set but setup_complete is false AND at least
   *  one package row already points under managed_root — a previous run was
   *  interrupted. UI should offer to resume rather than start fresh. */
  migration_in_progress: boolean;
}

export interface ProbeCheck {
  name: string;
  ok: boolean;
  detail: string;
}

export interface ProbeResult {
  addon_root: string;
  managed_root: string;
  /** True iff every check in `checks` passed and begin_migration would proceed. */
  ok: boolean;
  /** Per-check status in validation order; first failure is the most likely
   *  "fix me first" message. */
  checks: ProbeCheck[];
  /** First failed check's detail message, hoisted for easy UI binding. */
  diagnostic: string | null;
}

export interface MigrationProgress {
  moved: number;
  total: number;
  /** Basename of the current/most-recent file moved in the just-finished batch. */
  current: string | null;
}

export interface MigrationError {
  path: string;
  reason: string;
}

export interface MigrationResult {
  moved: number;
  /** .var files in addon_root not in the DB at migration time (Hub downloads
   *  added between scans). Migrated alongside the indexed ones. */
  leftover_moved: number;
  errors: MigrationError[];
  elapsed_ms: number;
}

/** Current setup state. Read this on app launch to decide whether to show
 *  the wizard, a resume banner, or normal UI. */
export async function getSetupState(): Promise<SetupState> {
  return invoke<SetupState>("get_setup_state");
}

/** Run every pre-commit validation against a proposed managed path.
 *  Cheap (no FS writes outside a tiny throwaway hardlink probe). Re-call on
 *  every path change for live status updates. */
export async function probeManagedPath(
  managedPath: string,
): Promise<ProbeResult> {
  return invoke<ProbeResult>("probe_managed_path", { managedPath });
}

/** Execute the one-time migration. Backend emits `migration.progress` events
 *  during the run; subscribe via @tauri-apps/api/event before calling. */
export async function beginMigration(
  managedPath: string,
): Promise<MigrationResult> {
  return invoke<MigrationResult>("begin_migration", { managedPath });
}

export interface RevertResult {
  /** .var entries moved back from managed_root to addon_root. */
  moved: number;
  /** active_folder_state entries cleared (hardlinks unlinked / junctions removed). */
  active_cleared: number;
  /** Stale packages rows pruned because their var_path no longer
   *  resolves to anything on disk. */
  orphans_pruned: number;
  errors: MigrationError[];
  elapsed_ms: number;
}

/** Undo a setup migration cleanly. Unloads the active folder, moves
 *  every entry back from managed_root → addon_root preserving relative
 *  path, prunes orphan packages rows, and resets setup settings. Same
 *  `migration.progress` event stream as forward migration. */
export async function revertSetup(): Promise<RevertResult> {
  return invoke<RevertResult>("revert_setup");
}

// --- Visibility-presets load / unload --------------------------------------

/** What the user wants visible — author seeds + explicit package ids.
 *  Authors resolve dynamically against `packages.creator` (case-
 *  insensitive); explicit ids bypass the `is_hidden` filter. */
export interface SeedSpec {
  creators: string[];
  package_ids: number[];
}

export interface LoadError {
  package_id: number;
  path: string;
  reason: string;
}

export interface LoadResult {
  /** Packages newly hardlinked into the active folder in this call. */
  added: number;
  /** Packages whose hardlink was removed from the active folder. */
  removed: number;
  /** Packages already correctly materialized — no FS touch.
   *  `kept + added == |closure(seeds)|` on success. */
  kept: number;
  /** Per-package errors that didn't abort the sync (destination
   *  occupied, source missing, etc.). */
  errors: LoadError[];
  elapsed_ms: number;
}

export interface VerifyResult {
  total: number;
  ok: number;
  missing_in_active: number[];
  inode_mismatch: number[];
  missing_in_managed: number[];
}

/** Reconcile the active folder to be exactly closure(seeds). Adds
 *  hardlinks for packages newly in target; removes hardlinks for
 *  packages newly out. Idempotent — re-calling with the same seeds
 *  is a no-op. */
export async function loadVisibility(seeds: SeedSpec): Promise<LoadResult> {
  return invoke<LoadResult>("load_visibility", { seeds });
}

/** Empty the active folder. Removes every hardlink we placed; unmanaged
 *  files in the folder are left alone. */
export async function unloadAll(): Promise<LoadResult> {
  return invoke<LoadResult>("unload_all");
}

/** Read-only health check on `active_folder_state` vs the filesystem.
 *  Reports stale rows. Caller decides whether to fix via `loadVisibility`
 *  (which is self-healing on re-call). */
export async function verifyActiveFolder(): Promise<VerifyResult> {
  return invoke<VerifyResult>("verify_active_folder");
}

export interface UnresolvedDep {
  src_package_id: number;
  raw_dep_key: string;
}

export interface ClosurePreview {
  /** Packages pulled in by author seeds (intersected with closure). */
  from_authors: number;
  /** Packages from explicit package seeds NOT already covered by authors. */
  from_packages: number;
  /** Packages added only via transitive dep resolution, not directly seeded. */
  from_deps: number;
  /** Total resolved ids in the closure. */
  total: number;
  /** All package ids in the closure (sorted). */
  package_ids: number[];
  /** Dep keys that referenced a non-installed package, paired with the
   *  closure-resident package that referenced them. */
  unresolved: UnresolvedDep[];
}

export interface LoadPlan {
  /** Closure breakdown (counts + ids + unresolved). */
  preview: ClosurePreview;
  /** Count of rows currently in active_folder_state. */
  currently_loaded: number;
  /** Packages that would be newly hardlinked on commit. */
  will_add: number;
  /** Packages that would be unlinked (in current but not in target). */
  will_remove: number;
  /** Packages already correctly materialized — no FS touch. */
  will_keep: number;
}

/** Dry-run for the load-visibility modal: closure + diff against the
 *  current active folder. Cheap (pure SQL). */
export async function computeLoadPlan(seeds: SeedSpec): Promise<LoadPlan> {
  return invoke<LoadPlan>("compute_load_plan", { seeds });
}

// --- Named-preset CRUD -----------------------------------------------------

export interface PresetSummary {
  id: number;
  name: string;
  description: string | null;
  /** Number of distinct author seeds attached to this preset. */
  creator_count: number;
  /** Number of explicit package-id seeds attached to this preset. */
  package_count: number;
  /** Unix seconds. */
  created_at: number;
  updated_at: number;
}

export interface Preset {
  summary: PresetSummary;
  /** Full seed spec — ready to feed back into computeLoadPlan or
   *  loadVisibility for "load this preset" flows. */
  seeds: SeedSpec;
}

/** All presets, most-recently-updated first. */
export async function listPresets(): Promise<PresetSummary[]> {
  return invoke<PresetSummary[]>("list_presets");
}

export async function getPreset(id: number): Promise<Preset> {
  return invoke<Preset>("get_preset", { id });
}

/** Create a new named preset. Returns the new row id. Fails if `name`
 *  is empty or duplicates an existing preset (UNIQUE constraint). */
export async function createPreset(
  name: string,
  seeds: SeedSpec,
  description?: string,
): Promise<number> {
  return invoke<number>("create_preset", { name, description, seeds });
}

export async function deletePreset(id: number): Promise<void> {
  return invoke("delete_preset", { id });
}

/** Rename a preset and/or update its description. Either side can be
 *  omitted to leave it unchanged. Bumps `updated_at`. */
export async function renamePreset(
  id: number,
  name?: string,
  description?: string,
): Promise<void> {
  return invoke("rename_preset", { id, name, description });
}

/** Distinct creators across the supplied package ids. Powers the
 *  LoadVisibilityModal "Seed by author" toggle: turning a per-package
 *  selection into a creator-based SeedSpec auto-includes future
 *  packages by the same authors on subsequent loads. */
export async function listCreatorsForPackages(
  packageIds: number[],
): Promise<string[]> {
  return invoke<string[]>("list_creators_for_packages", { packageIds });
}

/** Every package id marked as favorite (is_favorite = 1). Powers the
 *  modal's "Load favorites" quick action. */
export async function listFavoritePackageIds(): Promise<number[]> {
  return invoke<number[]>("list_favorite_package_ids");
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
  /** Per-rayon-worker snapshot. Length equals the `workers` config of the
   *  current sync (1-16). Each entry tracks what its thread is doing
   *  right now — useful for a side strip of mini progress bars so the
   *  user can see all parallel work, not just the global aggregate. */
  workers: WorkerSlot[];
}

export interface WorkerSlot {
  slot: number;
  /** Empty string when slot is idle. */
  creator: string;
  /** "idle" | "pin" (B1 broad) | "shortcut" (L≤2) | "fallback" (B2). */
  phase: string;
  /** Locals processed (matched / failed) for the creator on this slot. */
  done: number;
  /** Total locals belonging to the creator on this slot. */
  total: number;
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

// ─── Hub pin / category override ──────────────────────────────────────────

/** Outcome of a single package's pin attempt. Backend returns one entry
 *  per requested package id, even when most fail in the same way (e.g.
 *  URL parse error — every id gets a UrlParseError result so the UI can
 *  show a uniform per-row table). */
export type PinStatus =
  | "ok"
  | "url_parse_error"
  | "not_found"
  | "probe_failed"
  | "package_missing"
  | "db_error";

export interface PinResult {
  package_id: number;
  /** "manual" or "override" on success, null on any failure. */
  method: "manual" | "override" | null;
  status: PinStatus;
  /** Short human-readable context for non-ok rows (e.g. error message
   *  from the HEAD probe). Frontend may surface in the toast. */
  detail: string | null;
}

export interface PinReport {
  results: PinResult[];
  /** Aggregate propagation counts across all per-package writes. */
  siblings_updated: number;
  authors_updated: number;
  /** Convenience flag for the toast — true iff ≥1 row was pinned. */
  any_succeeded: boolean;
}

/** Manually pin one or more local packages to a hub resource. Accepts any
 *  of: full URL, /resources/<slug>.<id>/ path (with or without subpath /
 *  query), bare `<slug>.<id>`, or bare numeric id.
 *
 *  Backend validates with a single HEAD probe (resource-level, not
 *  per-package) before any DB write. On success each package gets
 *  hub_resource_id + hub_url + hub_match_method ('manual' if no prior
 *  match, 'override' otherwise), and `propagate_hub_match` fires from
 *  that row. Other hub_* metadata is filled in by the next hub-sync. */
export async function setHubPin(
  packageIds: number[],
  hubUrl: string,
): Promise<PinReport> {
  return invoke<PinReport>("set_hub_pin", { packageIds, hubUrl });
}

export interface CategoryReport {
  /** Rows the caller explicitly selected that got their category set. */
  directly_updated: number;
  /** Sibling rows (same creator+package_name) updated via propagation —
   *  unconditional propagation in this case, since the user explicitly
   *  declared a category for the package family. */
  siblings_updated: number;
}

/** Bulk-override hub_category for selected packages. Sets the
 *  `hub_category_manual` flag so subsequent hub-syncs leave the override
 *  alone. Does NOT touch hub_match_method — this isn't a re-pin. */
export async function setHubCategory(
  packageIds: number[],
  category: string,
): Promise<CategoryReport> {
  return invoke<CategoryReport>("set_hub_category", { packageIds, category });
}

export interface AuthorReport {
  /** Rows the caller explicitly selected. */
  directly_updated: number;
  /** Other rows by the same creator(s) reached via author-wide
   *  propagation. Disjoint from `directly_updated`. */
  authors_updated: number;
}

/** Bulk-override `hub_author` for selected packages AND every other
 *  package by the same creator(s). Sets `hub_author_manual = 1` on every
 *  touched row so subsequent hub-syncs leave the override alone — useful
 *  when the hub's displayed author name differs from how the user wants
 *  the creator identified. */
export async function setHubAuthor(
  packageIds: number[],
  hubAuthor: string,
): Promise<AuthorReport> {
  return invoke<AuthorReport>("set_hub_author", { packageIds, hubAuthor });
}

export interface PackageTypeReport {
  /** Rows the caller explicitly selected. */
  directly_updated: number;
  /** Sibling versions (same creator + package_name) that picked up the
   *  override via propagation. Disjoint from `directly_updated`. */
  siblings_updated: number;
}

/** Bulk-override the local heuristic `package_type` for selected
 *  packages + their version-siblings. Sets `package_type_manual = 1`
 *  so the scanner leaves the override alone on rescan. Useful when a
 *  package's contentList spans categories and the scanner labels it
 *  "Mixed" but the user knows it's effectively a Scene / Look / Plugin. */
export async function setPackageType(
  packageIds: number[],
  packageType: PackageType,
): Promise<PackageTypeReport> {
  return invoke<PackageTypeReport>("set_package_type", {
    packageIds,
    packageType,
  });
}

export type OverrideField = "category" | "author" | "type" | "pin";

export interface ClearOverrideReport {
  rows_updated: number;
}

/** Release a user-override. `field` selects which lock to clear:
 *   - "category" → hub_category_manual = 0 (across version siblings).
 *     Leaves hub_category value alone; next sync may overwrite.
 *   - "author" → hub_author_manual = 0 (across every package by the
 *     affected creator). Leaves hub_author value alone.
 *   - "type" → package_type_manual = 0 (across version siblings).
 *     Leaves package_type alone; next scan may reclassify.
 *   - "pin" → full unpin on the selected rows only (does NOT cascade
 *     to siblings — they may have independent pins). Preserves
 *     hub_author / hub_category if their _manual flags are set. */
export async function clearOverride(
  packageIds: number[],
  field: OverrideField,
): Promise<ClearOverrideReport> {
  return invoke<ClearOverrideReport>("clear_override", { packageIds, field });
}

// ===== Classifier (tagger + embedder) =================================

export interface TaggingStatus {
  has_api_key: boolean;
  api_key_length: number;
  taxonomy_seeded: boolean;
  taxonomy_active: number;
  families_total: number;
  families_pending: number;
  families_done: number;
  families_failed: number;
  taxonomy_version: string;
}

export interface EmbeddingStatus {
  families_with_purpose: number;
  families_missing_embedding: number;
  families_embedded: number;
  model: string;
  input_kind: string;
}

export interface TaggingProgress {
  batches: number;
  records_sent: number;
  records_done: number;
  records_failed: number;
  prompt_tokens: number;
  completion_tokens: number;
  /** "running" | "completed" | "cancelled" | "failed" */
  state: string;
  error: string | null;
}

export interface EmbeddingProgress {
  candidates: number;
  embedded: number;
  skipped_empty: number;
  state: string;
  error: string | null;
}

export interface TaggingRunOptions {
  /** Default "v4" — taxonomy_version label written to each row. */
  taxonomy_version?: string;
  /** Default "grok-4.3". */
  model?: string;
  /** Default 100 records per Grok call. */
  batch_size?: number;
  /** Default 1000 ms sleep between batches. */
  rate_limit_ms?: number;
  /** Cap on rows processed this run (UI: "Tag first N"). */
  limit?: number;
  /** Restrict to specific family ids (advanced — usually unset from the UI). */
  only_ids?: number[];
  /** No API calls; just log what would be sent. */
  dry_run?: boolean;
}

export interface TaggingRunSummary {
  batches: number;
  records_sent: number;
  records_done: number;
  records_failed: number;
  prompt_tokens: number;
  completion_tokens: number;
  cancelled: boolean;
}

export interface EmbeddingRunOptions {
  limit?: number;
  batch_size?: number;
}

export interface EmbeddingRunSummary {
  model: string;
  input_kind: string;
  candidates: number;
  embedded: number;
  skipped_empty: number;
  elapsed_secs: number;
  cancelled: boolean;
}

export async function taggingStatus(): Promise<TaggingStatus> {
  return invoke<TaggingStatus>("tagging_status");
}

export async function embeddingStatus(): Promise<EmbeddingStatus> {
  return invoke<EmbeddingStatus>("embedding_status");
}

export async function setXaiApiKey(key: string): Promise<void> {
  return invoke("set_xai_api_key", { key });
}

export async function clearXaiApiKey(): Promise<void> {
  return invoke("clear_xai_api_key");
}

export async function taggingActive(): Promise<boolean> {
  return invoke<boolean>("tagging_active");
}

export async function embeddingActive(): Promise<boolean> {
  return invoke<boolean>("embedding_active");
}

export async function startTaggingRun(
  options: TaggingRunOptions = {},
): Promise<TaggingRunSummary> {
  return invoke<TaggingRunSummary>("start_tagging_run", { options });
}

export async function stopTaggingRun(): Promise<void> {
  return invoke("stop_tagging_run");
}

export async function startEmbeddingRun(
  options: EmbeddingRunOptions = {},
): Promise<EmbeddingRunSummary> {
  return invoke<EmbeddingRunSummary>("start_embedding_run", { options });
}

export async function stopEmbeddingRun(): Promise<void> {
  return invoke("stop_embedding_run");
}

export interface RecomputeFamiliesSummary {
  families_before: number;
  families_after: number;
  families_added: number;
  packages_linked_this_run: number;
  families_with_latest: number;
  families_inheriting_tags: number;
  family_tag_rows_added: number;
}

export async function recomputeFamilies(): Promise<RecomputeFamiliesSummary> {
  return invoke<RecomputeFamiliesSummary>("recompute_families");
}
