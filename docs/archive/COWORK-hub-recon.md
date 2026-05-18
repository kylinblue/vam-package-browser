## Cowork brief — VaM Hub recon for category-driven UI pivot

**Goal:** ground-truth the structure of `https://hub.virtamate.com` so we can
build a UI keyed off `hub_category` instead of unreliable heuristic/LLM-derived
package types. Author-entered categories are the only authoritative
classification axis we have left.

**Constraint — content sensitivity:** the hub hosts adult content. The user
will pre-select 2-3 *safe* resource pages and a category index page and pass
those URLs to you directly. **Do not navigate freely.** Only open URLs the
user provides. If a page lands on something off-topic, close and report back.

**Background — what we already know:**

- The age gate is solved: `Cookie: vamhubconsent=yes` bypasses it.
- The site is XenForo. We already scrape the resource search at
  `/search/search` (CSRF flow: GET `/search/`, extract `_xfToken`, POST,
  follow redirect to `/search/<id>/?q=...`).
- Our existing scraper grabs per-result row: `resource_id`, `url`, `title`,
  `author`, `category` (the `.label` element on the result card),
  `preview_url`. See [src-tauri/src/hub.rs](src-tauri/src/hub.rs).
- We have ~4,000 local `.var` packages. About a third are from major authors
  (LDR, paledriver, VaMChan) who definitely publish on the hub; the rest is a
  long tail of indie / orphan packages that may or may not have hub matches.
- `hub_category` is what we want to anchor the new UI on. The user described
  it as "manually entered by the author when they upload" — i.e. a curated
  field, not a heuristic.

**Recon questions, in priority order:**

### 1. Canonical category list

Open the hub's resource browse / category index (URL the user will paste).
Report:

- The full list of top-level resource categories with their slugs/IDs.
  Example shape: `Looks (Female)` → `/resources/categories/looks-female.5/`.
- Is there sub-category nesting? (E.g. `Looks → Looks (Female)` vs flat.)
- Roughly how many top-level categories total? (We expect ~15-25.)
- Are the category names stable URL-friendly strings, or display labels with
  punctuation/parens? (Affects how we store them.)

### 2. Per-resource page metadata

For each safe resource page URL the user gives you, capture:

- The category as shown on the resource page itself (header crumb area or
  sidebar). Does it match exactly what the search-result `.label` shows? Any
  difference in casing/punctuation?
- The author name and link to their profile.
- Any **author-applied tags** (XenForo lets uploaders add free-form tags
  separately from category). Look for a "Tags:" row or a tag chip cluster.
  These would be a *secondary* signal — record what they look like.
- Resource version (latest), download count, rating, last-update timestamp.
  Just confirm presence/format, no need to scrape values.
- Whether the page has a clear "this is paid content" marker (some authors
  link to Patreon; the user said paid content is still listed and useful for
  the user-to-package mapping).

### 3. Search result row markup (sanity check)

We already parse this, but a quick re-confirmation would catch any drift:

- On a search-results page (URL user provides), inspect one result row.
  Confirm the `.label` element still holds the category and that
  `h3.contentRow-title a` is the title link with the resource_id in the
  href.
- Note any new CSS classes or restructuring since our scraper was written.

### 4. Pagination

- For a category index with many resources: how does pagination work?
  XenForo conventions are `/page-2`, `/page-3`, ... Confirm.
- Is there a stable "sort by recently updated" / "sort alphabetical" URL
  param we should be using for full-catalog enumeration?

### 5. Rate-limit / politeness signals

- Robots.txt rules? (User-Agent specific?)
- Any explicit rate-limit response codes you encounter (429, 503)?
- Our scraper defaults to 5000 ms between requests. Confirm that feels
  reasonable; report if pages load fast enough that we could tighten without
  being rude.

### 6. Gate-page sanity

- With `vamhubconsent=yes` set, do you ever hit a gate page on a normal
  resource view? Just confirm the cookie path is sufficient, no JS needed.

**What to report back:**

A short markdown summary keyed to the numbered questions. For each, a
sentence or two plus an example/URL where relevant. No need to capture full
HTML — just the answers and the selectors/structure we'd need.

**What's out of scope:**

- Don't fetch every category or enumerate the full catalog. We just need
  structural truth, not data harvest.
- Don't try to bypass paywalls or access locked content.
- Don't follow links to off-site Patreon/Discord/etc.

**Companion files for context:**

- [src-tauri/src/hub.rs](src-tauri/src/hub.rs) — current scraper, including
  the search flow + selectors we already use.
- [src-tauri/src/commands.rs](src-tauri/src/commands.rs) — search for
  `start_hub_sync` for the existing sync orchestrator.
- [src-tauri/src/index.rs](src-tauri/src/index.rs) — the `hub_*` columns on
  `packages` are where category data lives.

Once you report back, we'll plan the UI pivot: category chips as the primary
filter axis, sync UI un-shelved, and the detail view enriched with hub
title/author/url. The recon answers will tell us whether the existing scrape
is good enough or needs a richer per-resource-page step.
