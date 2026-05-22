import { useEffect } from "react";

export type ToastKind = "ok" | "error";

export interface ToastMessage {
  kind: ToastKind;
  text: string;
}

interface Props {
  toast: ToastMessage | null;
  onDismiss: () => void;
}

/// App-level toast. Lives outside the DetailView / SelectionActionBar
/// tree on purpose: child components that trigger async actions tend to
/// unmount or re-render during the refetch that follows a successful
/// action, so any feedback state owned by them flashes out of existence
/// before the user can read it. Putting the toast at the app root sidesteps
/// that — it survives whatever the rest of the UI does mid-action.
///
/// One toast at a time. New messages replace the current one rather than
/// stacking. Auto-dismisses after 6s; user can dismiss manually with ×.
export function Toast({ toast, onDismiss }: Props) {
  // Auto-dismiss timer. Re-arms whenever `toast` changes (a new message
  // resets the countdown), and cancels cleanly when the toast clears.
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(onDismiss, 6000);
    return () => window.clearTimeout(id);
  }, [toast, onDismiss]);

  if (!toast) return null;
  return (
    <div className={`toast toast-${toast.kind}`} role="status" aria-live="polite">
      <span className="toast-text">{toast.text}</span>
      <button
        type="button"
        className="toast-dismiss"
        onClick={onDismiss}
        aria-label="Dismiss"
        title="Dismiss"
      >
        ×
      </button>
    </div>
  );
}
