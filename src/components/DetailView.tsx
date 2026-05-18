import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  getPackageDetail,
  getPackageRelationships,
  HUGE_IMAGE_BYTES,
  openExternalUrl,
  revealInFolder,
  subThumbUrl,
  thumbUrl,
  vamHubAuthorSearchUrl,
  vamHubPackageSearchUrl,
  type ImageEntry,
  type PackageDetail,
  type PackageRelationships,
  type PackageRow,
  type RelatedPackage,
} from "../lib/api";
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
  }, [currentId]);

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

                {viewMode === "fetched" && pkg.hub_resource_id && (
                  <HubInfoSection pkg={pkg} />
                )}

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

function HubInfoSection({ pkg }: { pkg: PackageRow }) {
  const isOffsite = pkg.hub_is_hub_hosted === 0;
  const tier = pkg.hub_billing_tier ?? "free";
  const tierLabel = tier === "paid-early-access" ? "Paid (Early Access)"
    : tier === "paid" ? "Paid"
    : "Free";
  return (
    <section className="detail-hub-info">
      <h4>Hub</h4>
      <div className="detail-hub-title">{pkg.hub_title}</div>
      <div className="detail-hub-meta">
        <span className={`hub-tier-badge hub-tier-${tier.replace(/[^a-z-]/g, "")}`}>
          {tierLabel}
        </span>
        {pkg.hub_category && (
          <span className="hub-tier-badge hub-tier-category">{pkg.hub_category}</span>
        )}
        {pkg.hub_license && (
          <span className="hub-tier-badge hub-tier-license">{pkg.hub_license}</span>
        )}
        {pkg.hub_match_method && (
          <span
            className="hub-tier-badge hub-tier-method"
            title={`Match method: ${pkg.hub_match_method}`}
          >
            via {pkg.hub_match_method}
          </span>
        )}
      </div>
      <div className="detail-action-row">
        {pkg.hub_url && (
          <button
            type="button"
            className="detail-action"
            onClick={() => openExternalUrl(pkg.hub_url!)}
          >
            Open on hub
          </button>
        )}
        {isOffsite && pkg.hub_external_url && (
          <button
            type="button"
            className="detail-action"
            onClick={() => openExternalUrl(pkg.hub_external_url!)}
            title={pkg.hub_external_url}
          >
            Buy at source ↗
          </button>
        )}
      </div>
    </section>
  );
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
