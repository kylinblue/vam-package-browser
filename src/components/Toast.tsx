import { useEffect } from "react";

export type ToastKind = "ok" | "error";

/// Optional one-click undo on a toast. The label is shown on the button;
/// `onRevert` fires once when clicked, then the toast auto-dismisses
/// (whatever feedback the revert produces should come back through the
/// normal handleActionResult path as a fresh toast).
export interface ToastRevertAction {
  label?: string;
  onRevert: () => void;
}

export interface ToastMessage {
  /// Set by the App-level pushToast. Lets us dismiss a specific toast
  /// without colliding with others in the stack when the user is fast.
  id?: number;
  kind: ToastKind;
  text: string;
  /// When present, renders a "Revert" button. Reserved for actions where
  /// a clean undo path exists — currently the four override / pin set_*
  /// commands, which all reverse cleanly via clear_override.
  revert?: ToastRevertAction;
}

const AUTO_DISMISS_MS = 10_000;

interface StackProps {
  toasts: ToastMessage[];
  onDismiss: (id: number) => void;
}

/// App-level toast STACK. Lives outside the DetailView /
/// SelectionActionBar tree on purpose: child components that trigger
/// async actions tend to unmount or re-render during the refetch that
/// follows a successful action, so any feedback state owned by them
/// flashes out of existence before the user can read it. Putting the
/// toast stack at the app root sidesteps that — it survives whatever
/// the rest of the UI does mid-action.
///
/// Multiple toasts coexist (newest at top) so a user mashing override
/// actions can still see + revert each one — previously new toasts
/// replaced the current one and the older feedback was lost. Each
/// toast carries its own auto-dismiss timer (10s).
export function ToastStack({ toasts, onDismiss }: StackProps) {
  if (toasts.length === 0) return null;
  return (
    <div className="toast-stack">
      {toasts.map((t) => (
        <ToastItem
          key={t.id ?? `${t.kind}-${t.text}`}
          toast={t}
          onDismiss={() => t.id !== undefined && onDismiss(t.id)}
        />
      ))}
    </div>
  );
}

function ToastItem({
  toast,
  onDismiss,
}: {
  toast: ToastMessage;
  onDismiss: () => void;
}) {
  // Auto-dismiss timer. Re-arms whenever `toast` identity changes; we key
  // by id at the stack level so a re-render of an existing toast doesn't
  // reset its countdown.
  useEffect(() => {
    const id = window.setTimeout(onDismiss, AUTO_DISMISS_MS);
    return () => window.clearTimeout(id);
  }, [onDismiss]);

  const onRevertClick = () => {
    toast.revert?.onRevert();
    onDismiss();
  };

  return (
    <div className={`toast toast-${toast.kind}`} role="status" aria-live="polite">
      <span className="toast-text">{toast.text}</span>
      {toast.revert && (
        <button
          type="button"
          className="toast-revert"
          onClick={onRevertClick}
          title="Undo this action — restores the previous value."
        >
          {toast.revert.label ?? "Revert"}
        </button>
      )}
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

/// Back-compat single-toast wrapper. New code should use ToastStack +
/// pushToast at the App level; kept for any consumer still passing a
/// nullable single toast.
export function Toast({
  toast,
  onDismiss,
}: {
  toast: ToastMessage | null;
  onDismiss: () => void;
}) {
  if (!toast) return null;
  return (
    <ToastStack
      toasts={[{ ...toast, id: toast.id ?? 0 }]}
      onDismiss={onDismiss}
    />
  );
}
