interface ConfirmDialogProps {
  title: string
  message: string
  confirmLabel?: string
  onConfirm: () => void
  onCancel: () => void
}

export default function ConfirmDialog({
  title,
  message,
  confirmLabel = 'Delete',
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onCancel} aria-hidden />
      <div className="glass relative w-96 rounded-card bg-card p-6 shadow-xl">
        <h3 className="mb-2 text-base font-semibold">{title}</h3>
        <p className="mb-5 text-sm text-ink-muted">{message}</p>
        <div className="flex justify-end gap-2">
          <button
            onClick={onCancel}
            className="lift rounded-card border border-subtle px-4 py-1.5 text-sm font-medium"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            className="lift rounded-card bg-danger px-4 py-1.5 text-sm font-medium text-white"
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  )
}
