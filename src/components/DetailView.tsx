import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  getPackageDetail,
  getPackageRelationships,
  HUGE_IMAGE_BYTES,
  openExternalUrl,
  revealInFolder,
  setHubAuthor,
  setHubCategory,
  setHubPin,
  setPackageType,
  subThumbUrl,
  thumbUrl,
  vamHubAuthorSearchUrl,
  vamHubPackageSearchUrl,
  type ImageEntry,
  type PackageDetail,
  type PackageRelationships,
  type PackageRow,
  type PackageType,
  type RelatedPackage,
} from "../lib/api";

/** Canonical local PackageType list — mirrors the Rust PACKAGE_TYPE_VALUES
 *  constant in commands.rs. Used by the override dropdown. */
const PACKAGE_TYPES: readonly PackageType[] = [
  "Scene",
  "Look",
  "Morph",
  "Texture",
  "Clothing",
  "Hair",
  "Plugin",
  "Asset",
  "Pose",
  "Sound",
  "SubScene",
  "Mixed",
  "Unknown",
];
import { TagChips } from "./TagChips";

interface Props {
  packageId: number;
  thumbVersion: number;
  /** Classification source the user has picked in the toolbar. Controls
   *  which metadata sections render (Tags only in "tagged" mode; hub data
   *  will render in "fetched" mode once the hub-pivot milestone lands). */
  viewMode: "simple" | "tagged" | "fetched";
  onClose: () => void;
  onFilterByAuthor: (author: string) => void;
  onFilterByType: (type: string) => void;
  /** Open another package's detail view without dismissing this modal. Used
   *  by the dependency sidebar to navigate between related packages. */
  onOpenPackage: (id: number) => void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatDate(unix: number): string {
  return new Date(unix * 1000).toLocaleString();
}

/// Group contentList paths into top-level categories for collapsed display.
function groupContent(paths: string[]): Array<[string, string[]]> {
  const groups = new Map<string, string[]>();
  for (const p of paths) {
    const norm = p.replace(/\\/g, "/");
    const top = norm.split("/").slice(0, 3).join("/") || norm;
    if (!groups.has(top)) groups.set(top, []);
    groups.get(top)!.push(p);
  }
  return [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}

export function DetailView({
  packageId,
  thumbVersion,
  viewMode,
  onClose,
  onFilterByAuthor,
  onFilterByType,
  onOpenPackage: _onOpenPackage,
}: Props) {
  const [detail, setDetail] = useState<PackageDetail | null>(null);
  const [relationships, setRelationships] =
    useState<PackageRelationships | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedImage, setSelectedImage] = useState<string | null>(null);
  const [hoveredImage, setHoveredImage] = useState<string | null>(null);
  // Bumped after a hub pin / category override / type override succeeds to
  // re-fetch the package detail with updated fields. Avoids stale data
  // after the user performs an inline action.
  const [reloadCounter, setReloadCounter] = useState(0);

  // "Find similar" state shelved alongside the Ask UI — reactivation path
  // documented in App.tsx and TODO-semantic-search-ui.md.

  // Internal navigation history. Entry navigation (Depends on / Used by)
  // pushes onto this stack instead of bubbling up, so back/forward can walk
  // it like a browser. Resets whenever the modal is reopened with a new
  // external id (e.g., a fresh tile click in the grid).
  const [history, setHistory] = useState<number[]>([packageId]);
  const [cursor, setCursor] = useState(0);
  useEffect(() => {
    setHistory([packageId]);
    setCursor(0);
  }, [packageId]);
  const currentId = history[cursor];

  const canBack = cursor > 0;
  const canForward = cursor < history.length - 1;
  const goBack = () => canBack && setCursor((c) => c - 1);
  const goForward = () => canForward && setCursor((c) => c + 1);
  const navigateTo = (id: number) => {
    if (id === currentId) return;
    setHistory((h) => [...h.slice(0, cursor + 1), id]);
    setCursor((c) => c + 1);
  };

  const previewImage = hoveredImage ?? selectedImage;

  useEffect(() => {
    let cancelled = false;
    setDetail(null);
    setRelationships(null);
    setError(null);
    setSelectedImage(null);
    setHoveredImage(null);
    (async () => {
      try {
        // Fire detail + relationships in parallel — relationships is cheap
        // (indexed lookup) but no point waiting for it sequentially.
        const [d, r] = await Promise.all([
          getPackageDetail(currentId),
          getPackageRelationships(currentId).catch((e) => {
            console.warn("relationships fetch failed", e);
            return null;
          }),
        ]);
        if (!cancelled) {
          setDetail(d);
          setRelationships(r);
          setSelectedImage(d.preview_path ?? d.images[0]?.path ?? null);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [currentId, reloadCounter]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        goBack();
      } else if (e.altKey && e.key === "ArrowRight") {
        e.preventDefault();
        goForward();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, canBack, canForward]);

  const pkg = detail?.package;

  const tags = pkg?.tags ?? [];

  return createPortal(
    <div className="detail-backdrop" onClick={onClose}>
      <div className="detail-panel" onClick={(e) => e.stopPropagation()}>
        <div className="detail-nav">
          <button
            className="detail-nav-btn"
            onClick={goBack}
            disabled={!canBack}
            title="Back (Alt+←)"
            aria-label="Back"
          >
            ‹
          </button>
          <button
            className="detail-nav-btn"
            onClick={goForward}
            disabled={!canForward}
            title="Forward (Alt+→)"
            aria-label="Forward"
          >
            ›
          </button>
        </div>
        <button
          className="detail-close"
          onClick={onClose}
          title="Close (Esc)"
          aria-label="Close"
        >
          ×
        </button>
        {error && <div className="detail-error">Error: {error}</div>}
        {!detail && !error && <div className="detail-loading">Loading…</div>}
        {detail && pkg && (
          <>
            <div className="detail-header">
              <div className="detail-title">
                <button
                  className="detail-author-link"
                  onClick={() => onFilterByAuthor(pkg.creator)}
                  title="Filter grid by this author"
                >
                  {pkg.creator || "(no creator)"}
                </button>
                <span className="detail-name">{pkg.package_name}</span>
                <span className="detail-version">v{pkg.version}</span>
              </div>
              <div className="detail-subtitle">
                <button
                  className="detail-type-link"
                  onClick={() => onFilterByType(pkg.package_type)}
                  title="Filter grid by this type"
                >
                  {pkg.package_type}
                </button>
                <span>· {formatSize(pkg.file_size)}</span>
                <span title="When this .var was last touched on disk">
                  · file {formatDate(pkg.file_mtime)}
                </span>
                <span title="Latest timestamp inside the .var (when the author zipped it)">
                  · packaged{" "}
                  {pkg.package_mtime > 0 ? formatDate(pkg.package_mtime) : "unknown"}
                </span>
                {pkg.license && <span>· {pkg.license}</span>}
                {pkg.program_version && (
                  <span>· VaM {pkg.program_version}</span>
                )}
              </div>
            </div>

            <div className="detail-body">
              <div className="detail-main">
                {previewImage ? (
                  <img
                    className="detail-hero"
                    src={subThumbUrl(currentId, previewImage, true)}
                    alt={previewImage}
                  />
                ) : pkg.has_preview ? (
                  <img
                    className="detail-hero"
                    src={thumbUrl(currentId, currentId === packageId ? thumbVersion : 0)}
                    alt=""
                  />
                ) : (
                  <div className="detail-hero detail-hero-empty">
                    No previewable images in this package.
                  </div>
                )}
                {previewImage && (
                  <div className="detail-hero-caption" title={previewImage}>
                    {previewImage}
                  </div>
                )}
              </div>

              <aside className="detail-side">
                {pkg.description && (
                  <section>
                    <h4>Description</h4>
                    <p className="detail-description">{pkg.description}</p>
                  </section>
                )}

                {detail.instructions && (
                  <section>
                    <h4>Instructions</h4>
                    <p className="detail-description">{detail.instructions}</p>
                  </section>
                )}

                <HubInfoSection
                  pkg={pkg}
                  viewMode={viewMode}
                  onReload={() => setReloadCounter((c) => c + 1)}
                />

                {viewMode === "tagged" && tags.length > 0 && (
                  <section>
                    <h4>Tags</h4>
                    <TagChips tags={tags} />
                  </section>
                )}

                {/* SimilarSection shelved alongside the Ask UI in App.tsx —
                    backend (search_similar_families) still wired. See
                    TODO-semantic-search-ui.md and the App.tsx shelving notes
                    for reactivation. */}

                <section>
                  <h4>File</h4>
                  <div className="detail-path" title={pkg.var_path}>
                    {pkg.var_path}
                  </div>
                  <div className="detail-action-row">
                    <button
                      onClick={() => revealInFolder(pkg.var_path)}
                      className="detail-action"
                    >
                      Reveal in folder
                    </button>
                    <button
                      onClick={() =>
                        openExternalUrl(vamHubAuthorSearchUrl(pkg.creator))
                      }
                      className="detail-action"
                    >
                      Search author on VaM Hub
                    </button>
                    <button
                      onClick={() =>
                        openExternalUrl(
                          vamHubPackageSearchUrl(pkg.package_name),
                        )
                      }
                      className="detail-action"
                    >
                      Search package on VaM Hub
                    </button>
                  </div>
                </section>

                {/* VaM Hub section shelved — backend data still loads but no UI surface. */}

                {relationships && relationships.depends_on.length > 0 && (
                  <RelationshipSection
                    title="Depends on"
                    items={relationships.depends_on}
                    onOpen={navigateTo}
                  />
                )}
                {relationships && relationships.used_by.length > 0 && (
                  <RelationshipSection
                    title="Used by"
                    items={relationships.used_by}
                    onOpen={navigateTo}
                  />
                )}

                <section>
                  <h4>Contents ({detail.content_list.length})</h4>
                  <ContentList paths={detail.content_list} />
                </section>
              </aside>
            </div>

            {detail.images.length > 0 && (
              <GallerySection
                images={detail.images}
                packageId={currentId}
                selectedImage={selectedImage}
                onSelect={setSelectedImage}
                onHover={setHoveredImage}
              />
            )}
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}

// SimilarSection definition shelved. See git history for the implementation
// to revive; the relevant Tauri command is `search_similar_families` in
// commands.rs.

/** Canonical hub category list, mirrors HubCategoryChips. Kept inline
 *  rather than imported because the picker UI only needs the labels —
 *  if/when the list grows beyond ~25 entries, hoist to lib/. */
const HUB_CATEGORIES: readonly string[] = [
  "Scenes",
  "Looks",
  "Clothing",
  "Hairstyles",
  "Morphs",
  "Poses",
  "Mocap + Animation",
  "Textures",
  "Environments",
  "Lighting + HDRI",
  "Assets + Accessories",
  "Audio",
  "Plugins + Scripts",
  "Toolkits + Templates",
  "Comics + Storytelling",
  "Voxta Content",
  "Demo + Lite",
  "Guides",
  "Other",
];

function HubInfoSection({
  pkg,
  viewMode,
  onReload,
}: {
  pkg: PackageRow;
  viewMode: "simple" | "tagged" | "fetched";
  onReload: () => void;
}) {
  const isFetched = viewMode === "fetched";
  const isMatched = pkg.hub_resource_id != null;
  const isOffsite = pkg.hub_is_hub_hosted === 0;
  const tier = pkg.hub_billing_tier ?? "free";
  const tierLabel =
    tier === "paid-early-access"
      ? "Paid (Early Access)"
      : tier === "paid"
        ? "Paid"
        : "Free";

  // The "Override category/type" button consolidates two backend axes
  // under one slot: in Fetched mode it sets hub_category (the axis driving
  // the toolbar's HubCategoryChips); in Simple/Tagged it sets the local
  // heuristic package_type (the axis driving TypeChips). One button, two
  // commands, picked per current view.
  const classifyLabel = isFetched ? "Override category…" : "Override type…";
  const classifyOptions: readonly string[] = isFetched
    ? HUB_CATEGORIES
    : PACKAGE_TYPES;
  const classifyCurrent = isFetched
    ? pkg.hub_category ?? "Scenes"
    : pkg.package_type;

  // Inline action state. Kept local to the section so reopening DetailView
  // resets everything cleanly (the section unmounts with the modal).
  const [showPin, setShowPin] = useState(false);
  const [pinUrl, setPinUrl] = useState("");
  const [busy, setBusy] = useState<"pin" | "classify" | "author" | null>(null);
  const [feedback, setFeedback] = useState<{
    kind: "ok" | "error";
    text: string;
  } | null>(null);
  const [showClassify, setShowClassify] = useState(false);
  const [classifyDraft, setClassifyDraft] = useState<string>(classifyCurrent);
  const [showAuthor, setShowAuthor] = useState(false);
  const [authorDraft, setAuthorDraft] = useState(pkg.hub_author ?? "");

  // Keep the dropdown's initial selection in sync with the current axis
  // when the user flips viewMode while the section is mounted (e.g.
  // opening DetailView in Simple then switching to Fetched).
  useEffect(() => {
    setClassifyDraft(classifyCurrent);
    setShowClassify(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isFetched, pkg.package_type, pkg.hub_category]);

  async function handlePin() {
    if (!pinUrl.trim() || busy) return;
    setBusy("pin");
    setFeedback(null);
    try {
      const report = await setHubPin([pkg.id], pinUrl);
      if (!report.any_succeeded) {
        const r = report.results[0];
        setFeedback({
          kind: "error",
          text: `Pin failed: ${r?.status ?? "unknown"}${r?.detail ? ` — ${r.detail}` : ""}`,
        });
        return;
      }
      const r = report.results[0];
      const sib = report.siblings_updated;
      const auth = report.authors_updated;
      // Toast wording avoids "propagation" jargon per the agreed copy.
      let msg = r.method === "override" ? "Overrode hub pin." : "Linked to hub.";
      if (sib + auth > 0) {
        msg += ` The match will be applied to ${sib + auth} related row${
          sib + auth === 1 ? "" : "s"
        } over the next few minutes — each one is verified against the hub at the configured sync rate.`;
      } else {
        msg += " Metadata fills in on the next hub sync.";
      }
      setFeedback({ kind: "ok", text: msg });
      setShowPin(false);
      setPinUrl("");
      onReload();
    } catch (e) {
      setFeedback({ kind: "error", text: `Pin error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function handleClassify() {
    if (!classifyDraft || busy) return;
    setBusy("classify");
    setFeedback(null);
    try {
      let msg: string;
      if (isFetched) {
        const report = await setHubCategory([pkg.id], classifyDraft);
        const sib = report.siblings_updated;
        msg =
          sib > 0
            ? `Updated category for ${report.directly_updated} package${
                report.directly_updated === 1 ? "" : "s"
              } and ${sib} sibling version${sib === 1 ? "" : "s"}. Auto-sync will keep this override.`
            : "Updated category. Auto-sync will keep this override.";
      } else {
        const report = await setPackageType(
          [pkg.id],
          classifyDraft as PackageType,
        );
        const sib = report.siblings_updated;
        msg =
          sib > 0
            ? `Set type to ${classifyDraft}. ${sib} sibling version${
                sib === 1 ? "" : "s"
              } updated. Scanner will preserve this on rescan.`
            : `Set type to ${classifyDraft}. Scanner will preserve this on rescan.`;
      }
      setFeedback({ kind: "ok", text: msg });
      setShowClassify(false);
      onReload();
    } catch (e) {
      setFeedback({
        kind: "error",
        text: `${isFetched ? "Category" : "Type"} override error: ${e}`,
      });
    } finally {
      setBusy(null);
    }
  }

  async function handleAuthor() {
    if (!authorDraft.trim() || busy) return;
    setBusy("author");
    setFeedback(null);
    try {
      const report = await setHubAuthor([pkg.id], authorDraft);
      const others = report.authors_updated;
      const msg =
        others > 0
          ? `Updated author. ${others} other package${
              others === 1 ? "" : "s"
            } by ${pkg.creator || "this creator"} picked up the same override. Auto-sync will keep it.`
          : "Updated author. Auto-sync will keep this override.";
      setFeedback({ kind: "ok", text: msg });
      setShowAuthor(false);
      onReload();
    } catch (e) {
      setFeedback({ kind: "error", text: `Author error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="detail-hub-info">
      <h4>{isFetched ? "Hub" : "Overrides"}</h4>
      {isFetched &&
        (isMatched ? (
          <>
            <div className="detail-hub-title">
              {pkg.hub_title ?? "(metadata pending)"}
            </div>
            <div className="detail-hub-meta">
              <span
                className={`hub-tier-badge hub-tier-${tier.replace(/[^a-z-]/g, "")}`}
              >
                {tierLabel}
              </span>
              {pkg.hub_category && (
                <span className="hub-tier-badge hub-tier-category">
                  {pkg.hub_category}
                </span>
              )}
              {pkg.hub_license && (
                <span className="hub-tier-badge hub-tier-license">
                  {pkg.hub_license}
                </span>
              )}
              {pkg.hub_match_method && (
                <span
                  className={`hub-tier-badge hub-tier-method hub-tier-method-${pkg.hub_match_method}`}
                  title={methodTooltip(pkg.hub_match_method)}
                >
                  via {pkg.hub_match_method}
                </span>
              )}
            </div>
          </>
        ) : (
          <div className="detail-hub-empty">
            Not linked to a hub resource. Pin a URL below to establish the
            link; metadata fills in on the next sync.
          </div>
        ))}

      <div className="detail-action-row">
        {isFetched && pkg.hub_url && (
          <button
            type="button"
            className="detail-action"
            onClick={() => openExternalUrl(pkg.hub_url!)}
          >
            Open on hub
          </button>
        )}
        {isFetched && isOffsite && pkg.hub_external_url && (
          <button
            type="button"
            className="detail-action"
            onClick={() => openExternalUrl(pkg.hub_external_url!)}
            title={pkg.hub_external_url}
          >
            Buy at source ↗
          </button>
        )}
        {isFetched && (
          <button
            type="button"
            className="detail-action"
            onClick={() => {
              setShowPin((v) => !v);
              setShowClassify(false);
              setShowAuthor(false);
              setFeedback(null);
            }}
          >
            {isMatched ? "Pin to different URL…" : "Pin to hub URL…"}
          </button>
        )}
        <button
          type="button"
          className="detail-action"
          onClick={() => {
            setShowClassify((v) => !v);
            setShowPin(false);
            setShowAuthor(false);
            setFeedback(null);
          }}
          title={
            isFetched
              ? "Override the hub_category for this package — protected from auto-sync overwrites"
              : "Override the local package_type — kept across rescans, propagates to sibling versions"
          }
        >
          {classifyLabel}
        </button>
        <button
          type="button"
          className="detail-action"
          onClick={() => {
            setShowAuthor((v) => !v);
            setShowPin(false);
            setShowClassify(false);
            setFeedback(null);
          }}
          title={`Set the canonical hub author for ${pkg.creator || "this creator"}. Propagates to every other package by the same creator and is protected from auto-sync overwrites.`}
        >
          Override author…
        </button>
      </div>

      {showPin && (
        <div className="detail-hub-pin-row">
          <input
            type="text"
            value={pinUrl}
            onChange={(e) => setPinUrl(e.target.value)}
            placeholder="https://hub.virtamate.com/resources/…  or  37103"
            disabled={busy === "pin"}
            onKeyDown={(e) => {
              if (e.key === "Enter") handlePin();
              if (e.key === "Escape") setShowPin(false);
            }}
            autoFocus
          />
          <button
            type="button"
            className="detail-action detail-action-primary"
            onClick={handlePin}
            disabled={!pinUrl.trim() || busy === "pin"}
          >
            {busy === "pin" ? "Pinning…" : "Pin"}
          </button>
          <button
            type="button"
            className="detail-action"
            onClick={() => {
              setShowPin(false);
              setPinUrl("");
            }}
            disabled={busy === "pin"}
          >
            Cancel
          </button>
        </div>
      )}

      {showClassify && (
        <div className="detail-hub-pin-row">
          <select
            value={classifyDraft}
            onChange={(e) => setClassifyDraft(e.target.value)}
            disabled={busy === "classify"}
          >
            {classifyOptions.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="detail-action detail-action-primary"
            onClick={handleClassify}
            disabled={busy === "classify"}
          >
            {busy === "classify" ? "Applying…" : "Apply"}
          </button>
          <button
            type="button"
            className="detail-action"
            onClick={() => setShowClassify(false)}
            disabled={busy === "classify"}
          >
            Cancel
          </button>
        </div>
      )}

      {showAuthor && (
        <div className="detail-hub-pin-row">
          <input
            type="text"
            value={authorDraft}
            onChange={(e) => setAuthorDraft(e.target.value)}
            placeholder={`Canonical hub author for ${pkg.creator || "this creator"}`}
            disabled={busy === "author"}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleAuthor();
              if (e.key === "Escape") setShowAuthor(false);
            }}
            autoFocus
          />
          <button
            type="button"
            className="detail-action detail-action-primary"
            onClick={handleAuthor}
            disabled={!authorDraft.trim() || busy === "author"}
          >
            {busy === "author" ? "Applying…" : "Apply"}
          </button>
          <button
            type="button"
            className="detail-action"
            onClick={() => setShowAuthor(false)}
            disabled={busy === "author"}
          >
            Cancel
          </button>
        </div>
      )}

      {feedback && (
        <div className={`detail-hub-feedback detail-hub-feedback-${feedback.kind}`}>
          {feedback.text}
        </div>
      )}
    </section>
  );
}

function methodTooltip(method: string): string {
  switch (method) {
    case "filename":
      return "Matched by exact .var filename on the hub CDN.";
    case "fuzzy_title":
      return "Matched by fuzzy title search (paid-fallback path).";
    case "manual":
      return "You pinned this package to a hub URL. Auto-sync will not overwrite this match.";
    case "override":
      return "You overrode a prior auto-match by pinning a different hub URL. Auto-sync will not overwrite this.";
    case "inherit":
      return "Inherited from a sibling version's hub match. Will be verified on the next hub sync.";
    default:
      return `Match method: ${method}`;
  }
}

const GALLERY_INITIAL_CAP = 60;

function GallerySection({
  images,
  packageId,
  selectedImage,
  onSelect,
  onHover,
}: {
  images: ImageEntry[];
  packageId: number;
  selectedImage: string | null;
  onSelect: (path: string) => void;
  onHover: (path: string | null) => void;
}) {
  const [showAll, setShowAll] = useState(images.length <= GALLERY_INITIAL_CAP);
  const visible = showAll ? images : images.slice(0, GALLERY_INITIAL_CAP);
  return (
    <div
      className="detail-gallery"
      onMouseLeave={() => onHover(null)}
    >
      <div className="detail-gallery-header">
        Images in package ({images.length})
        {!showAll && (
          <button
            type="button"
            className="detail-gallery-more"
            onClick={() => setShowAll(true)}
          >
            Show all {images.length}
          </button>
        )}
      </div>
      <div className="detail-gallery-grid">
        {visible.map((img) => (
          <ImageThumb
            key={img.path}
            img={img}
            packageId={packageId}
            selected={img.path === selectedImage}
            onClick={() => onSelect(img.path)}
            onHover={() => onHover(img.path)}
          />
        ))}
      </div>
    </div>
  );
}

function ImageThumb({
  img,
  packageId,
  selected,
  onClick,
  onHover,
}: {
  img: ImageEntry;
  packageId: number;
  selected: boolean;
  onClick: () => void;
  onHover: () => void;
}) {
  const ref = useRef<HTMLButtonElement>(null);
  const [visible, setVisible] = useState(false);
  const [failed, setFailed] = useState(false);

  const isHuge = img.size > HUGE_IMAGE_BYTES;

  // Explicit intersection observer — only mount the <img> (and fire its
  // protocol request) when the thumb is actually scrolled into view. Skipped
  // for huge images (we never auto-load them).
  useEffect(() => {
    if (!ref.current || visible || isHuge) return;
    const obs = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setVisible(true);
          obs.disconnect();
        }
      },
      { rootMargin: "80px" },
    );
    obs.observe(ref.current);
    return () => obs.disconnect();
  }, [visible, isHuge]);

  return (
    <button
      ref={ref}
      className={`detail-gallery-item ${selected ? "selected" : ""} ${isHuge ? "huge" : ""}`}
      onClick={onClick}
      onMouseEnter={onHover}
      onFocus={onHover}
      title={`${img.path}\n${formatSize(img.size)}${isHuge ? "\n(huge — click to view in hero)" : ""}`}
    >
      {isHuge ? (
        <span className="detail-gallery-huge">
          <span className="detail-gallery-huge-icon">⛰</span>
          <span className="detail-gallery-huge-size">{formatSize(img.size)}</span>
          <span className="detail-gallery-huge-hint">click to view</span>
        </span>
      ) : (
        <>
          {visible && !failed && (
            <img
              src={subThumbUrl(packageId, img.path)}
              alt=""
              onError={() => setFailed(true)}
            />
          )}
          {visible && failed && <span className="detail-gallery-fail">×</span>}
        </>
      )}
    </button>
  );
}

const RELATIONSHIP_INITIAL_CAP = 20;

function RelationshipSection({
  title,
  items,
  onOpen,
}: {
  title: string;
  items: RelatedPackage[];
  onOpen: (id: number) => void;
}) {
  const [showAll, setShowAll] = useState(items.length <= RELATIONSHIP_INITIAL_CAP);
  const visible = showAll ? items : items.slice(0, RELATIONSHIP_INITIAL_CAP);
  const missing = items.filter((i) => i.id === null).length;
  return (
    <section>
      <h4>
        {title} ({items.length}
        {missing > 0 ? `, ${missing} not installed` : ""})
      </h4>
      <ul className="detail-rel-list">
        {visible.map((it) => (
          <RelatedRow key={it.raw_dep_key + ":" + (it.id ?? "x")} item={it} onOpen={onOpen} />
        ))}
      </ul>
      {!showAll && (
        <button
          type="button"
          className="detail-rel-more"
          onClick={() => setShowAll(true)}
        >
          Show all {items.length}
        </button>
      )}
    </section>
  );
}

function RelatedRow({
  item,
  onOpen,
}: {
  item: RelatedPackage;
  onOpen: (id: number) => void;
}) {
  if (item.id === null) {
    // Missing: render greyed out, no click action. The raw key still tells
    // the user what to install if they want to fill the gap.
    return (
      <li className="detail-rel-row missing" title="Not installed locally">
        <span className="detail-rel-key">{item.raw_dep_key}</span>
        <span className="detail-rel-badge">not installed</span>
      </li>
    );
  }
  return (
    <li className="detail-rel-row">
      <button
        type="button"
        className="detail-rel-link"
        onClick={() => onOpen(item.id!)}
        title={`Open ${item.creator}.${item.package_name}.${item.version}`}
      >
        <span className="detail-rel-author">{item.creator}</span>
        <span className="detail-rel-sep">·</span>
        <span className="detail-rel-name">{item.package_name}</span>
        <span className="detail-rel-version">v{item.version}</span>
        {item.package_type && (
          <span className="detail-rel-type">{item.package_type}</span>
        )}
      </button>
    </li>
  );
}

function ContentList({ paths }: { paths: string[] }) {
  const groups = groupContent(paths);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  if (paths.length === 0) {
    return <div className="detail-deps-empty">(empty)</div>;
  }
  return (
    <ul className="detail-content-list">
      {groups.map(([prefix, items]) => {
        const isOpen = expanded.has(prefix);
        const toggle = () => {
          const next = new Set(expanded);
          if (isOpen) next.delete(prefix);
          else next.add(prefix);
          setExpanded(next);
        };
        return (
          <li key={prefix}>
            <button className="detail-content-group" onClick={toggle}>
              <span>{isOpen ? "▾" : "▸"}</span>
              <span className="detail-content-prefix">{prefix}/</span>
              <span className="detail-content-n">{items.length}</span>
            </button>
            {isOpen && (
              <ul className="detail-content-items">
                {items.map((p) => (
                  <li key={p} title={p}>
                    {p.slice(prefix.length).replace(/^\//, "")}
                  </li>
                ))}
              </ul>
            )}
          </li>
        );
      })}
    </ul>
  );
}
