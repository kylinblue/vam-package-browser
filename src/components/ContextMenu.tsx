import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";

export interface MenuItem {
  label: string;
  onClick: () => void;
  destructive?: boolean;
  divider?: boolean;
  disabled?: boolean;
}

interface Props {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}

const MENU_WIDTH = 220;

export function ContextMenu({ x, y, items, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onPointerDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const onContext = (e: MouseEvent) => {
      // Closing on another contextmenu means the next right-click reopens for a
      // different tile cleanly instead of stacking menus.
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener("mousedown", onPointerDown);
    window.addEventListener("keydown", onKey);
    window.addEventListener("contextmenu", onContext, true);
    return () => {
      window.removeEventListener("mousedown", onPointerDown);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("contextmenu", onContext, true);
    };
  }, [onClose]);

  // Clamp to viewport so the menu doesn't render off-screen near the right edge.
  const left = Math.min(x, window.innerWidth - MENU_WIDTH - 8);
  const top = Math.min(y, window.innerHeight - items.length * 28 - 8);

  // Portal to document.body so the menu escapes any ancestor with `transform`
  // (the virtualizer's translateY on .grid-row becomes the containing block for
  // position:fixed descendants, which would clip the menu behind sibling rows).
  return createPortal(
    <div
      ref={ref}
      className="context-menu"
      style={{ left, top, width: MENU_WIDTH }}
      role="menu"
    >
      {items.map((item, i) =>
        item.divider ? (
          <div key={i} className="context-menu-divider" />
        ) : (
          <button
            type="button"
            key={i}
            className={`context-menu-item ${item.destructive ? "destructive" : ""}`}
            disabled={item.disabled}
            onClick={() => {
              item.onClick();
              onClose();
            }}
            role="menuitem"
          >
            {item.label}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}
