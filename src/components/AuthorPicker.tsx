import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { CreatorCount } from "../lib/api";

interface Props {
  creators: CreatorCount[];
  value: string;
  onChange: (creator: string) => void;
  placeholder?: string;
}

const DROPDOWN_MAX_HEIGHT = 320;

export function AuthorPicker({ creators, value, onChange, placeholder }: Props) {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const [highlighted, setHighlighted] = useState(0);
  const [dropPos, setDropPos] = useState<{ x: number; y: number; w: number } | null>(null);

  const wrapRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => {
    if (!filter) return creators;
    const lower = filter.toLowerCase();
    return creators.filter((c) => c.creator.toLowerCase().includes(lower));
  }, [creators, filter]);

  // Reset highlight when filter changes (always start at top of new list).
  useEffect(() => setHighlighted(0), [filter, open]);

  // Position the dropdown right below the input.
  useEffect(() => {
    if (!open || !inputRef.current) return;
    const place = () => {
      if (!inputRef.current) return;
      const r = inputRef.current.getBoundingClientRect();
      // Flip up if there's not enough room below.
      const spaceBelow = window.innerHeight - r.bottom - 8;
      const top =
        spaceBelow >= 160 || r.top < DROPDOWN_MAX_HEIGHT + 8
          ? r.bottom + 4
          : r.top - Math.min(DROPDOWN_MAX_HEIGHT, r.top - 8) - 4;
      setDropPos({ x: r.left, y: top, w: Math.max(220, r.width) });
    };
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  // Close on outside mousedown.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (wrapRef.current?.contains(t)) return;
      if (dropdownRef.current?.contains(t)) return;
      setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [open]);

  // Scroll highlighted item into view as user arrows through.
  useEffect(() => {
    if (!open || !dropdownRef.current) return;
    const el = dropdownRef.current.querySelector<HTMLElement>(
      `[data-idx="${highlighted}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [highlighted, open]);

  const select = (creator: string) => {
    onChange(creator);
    setOpen(false);
    setFilter("");
    inputRef.current?.blur();
  };

  const clear = () => {
    onChange("");
    setFilter("");
    inputRef.current?.focus();
    setOpen(true);
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) setOpen(true);
      setHighlighted((h) => Math.min(h + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlighted((h) => Math.max(h - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (filtered[highlighted]) select(filtered[highlighted].creator);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
      setFilter("");
    } else if (e.key === "Backspace" && !filter && value) {
      // First backspace when input is empty clears the selected author.
      e.preventDefault();
      clear();
    }
  };

  // Show selected value when not actively filtering.
  const displayValue = open ? filter : value;

  return (
    <div ref={wrapRef} className="author-picker">
      <input
        ref={inputRef}
        type="text"
        className={`author-picker-input ${value && !open ? "has-value" : ""}`}
        value={displayValue}
        onChange={(e) => {
          setFilter(e.target.value);
          if (!open) setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={onKey}
        placeholder={placeholder ?? "Author"}
      />
      {value && (
        <button
          type="button"
          className="author-picker-clear"
          onMouseDown={(e) => {
            // mousedown (not click) so we close-and-clear before onBlur fires.
            e.preventDefault();
            clear();
          }}
          title="Clear author filter"
          tabIndex={-1}
        >
          ×
        </button>
      )}
      {open &&
        dropPos &&
        createPortal(
          <div
            ref={dropdownRef}
            className="author-picker-dropdown"
            style={{
              position: "fixed",
              left: dropPos.x,
              top: dropPos.y,
              width: dropPos.w,
              maxHeight: DROPDOWN_MAX_HEIGHT,
            }}
          >
            {filtered.length === 0 ? (
              <div className="author-picker-empty">No matches</div>
            ) : (
              filtered.map((c, i) => (
                <button
                  key={c.creator}
                  type="button"
                  data-idx={i}
                  className={`author-picker-item ${
                    i === highlighted ? "highlighted" : ""
                  } ${c.creator === value ? "selected" : ""}`}
                  onMouseDown={(e) => {
                    e.preventDefault(); // keep focus on input
                    select(c.creator);
                  }}
                  onMouseEnter={() => setHighlighted(i)}
                >
                  <span className="author-picker-name">{c.creator}</span>
                  <span className="author-picker-count">{c.count}</span>
                </button>
              ))
            )}
          </div>,
          document.body,
        )}
    </div>
  );
}
