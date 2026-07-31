import { useEffect, useMemo, useState } from 'react'
import ConfirmDialog from '../components/common/ConfirmDialog'
import { fetchItemCatalog, type CatalogItem } from '../lib/items'
import { cachedImageUrl, itemSpriteUrl } from '../lib/sprites'
import { favoritesFirst, loadFormats, newId, saveFormats, type StoredFormat } from '../lib/storage'

export default function FormatsPage() {
  const [formats, setFormats] = useState<StoredFormat[]>(loadFormats)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [pendingDelete, setPendingDelete] = useState<StoredFormat | null>(null)

  const update = (next: StoredFormat[]) => {
    setFormats(next)
    saveFormats(next)
  }

  const save = (format: StoredFormat) => {
    if (formats.some((f) => f.id === format.id)) {
      update(formats.map((f) => (f.id === format.id ? format : f)))
    } else {
      update([...formats, format])
    }
    setEditingId(null)
    setCreating(false)
  }

  const toggleFavorite = (format: StoredFormat) =>
    update(formats.map((f) => (f.id === format.id ? { ...f, favorite: !f.favorite } : f)))

  const sorted = favoritesFirst(formats)

  return (
    <div className="mx-auto max-w-6xl p-6">
      <h1 className="mb-6 text-xl font-semibold">Formats</h1>
      <div className="grid grid-cols-3 gap-6">
        {sorted.map((format) =>
          editingId === format.id ? (
            <FormatEditor key={format.id} initial={format} onSave={save} onCancel={() => setEditingId(null)} />
          ) : (
            <FormatCard
              key={format.id}
              format={format}
              onEdit={() => setEditingId(format.id)}
              onFavorite={() => toggleFavorite(format)}
              onDelete={() => setPendingDelete(format)}
            />
          ),
        )}
        {creating ? (
          <FormatEditor
            initial={{
              id: newId(),
              name: '',
              activePokemon: 1,
              totalPokemon: 6,
              broughtPokemon: 3,
              bannedItems: [],
              forceMaxIvs: true,
              // Pokémon Champions has no Terastallization.
              teraEnabled: false,
              megaEnabled: true,
              favorite: false,
            }}
            onSave={save}
            onCancel={() => setCreating(false)}
          />
        ) : (
          <button
            onClick={() => setCreating(true)}
            className="lift flex min-h-40 items-center justify-center rounded-card border-2 border-dashed border-subtle text-4xl text-ink-muted hover:border-primary hover:text-primary"
            aria-label="Add format"
          >
            +
          </button>
        )}
      </div>

      {pendingDelete && (
        <ConfirmDialog
          title={`Delete "${pendingDelete.name}"?`}
          message="This removes the format from local storage. This cannot be undone."
          onConfirm={() => {
            update(formats.filter((f) => f.id !== pendingDelete.id))
            setPendingDelete(null)
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </div>
  )
}

function FormatCard({
  format,
  onEdit,
  onFavorite,
  onDelete,
}: {
  format: StoredFormat
  onEdit: () => void
  onFavorite: () => void
  onDelete: () => void
}) {
  return (
    <div onClick={onEdit} className="lift cursor-pointer rounded-card bg-card p-4 shadow-sm">
      <div className="mb-2 flex items-center justify-between">
        <h2 className="truncate text-sm font-semibold">{format.name}</h2>
        <div className="flex gap-1">
          <button
            onClick={(e) => {
              e.stopPropagation()
              onFavorite()
            }}
            className={`lift rounded-card p-1.5 ${format.favorite ? 'text-warning' : 'text-ink-muted'}`}
            aria-label="Favorite"
            title="Favorite"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill={format.favorite ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinejoin="round">
              <path d="m12 2 3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01z" />
            </svg>
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation()
              onDelete()
            }}
            className="lift rounded-card p-1.5 text-ink-muted hover:text-danger"
            aria-label="Delete"
            title="Delete"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
            </svg>
          </button>
        </div>
      </div>
      <p className="text-sm text-ink-muted">
        {format.activePokemon} active / bring {format.broughtPokemon} of {format.totalPokemon}
      </p>
      <p className="mt-1 text-xs text-ink-muted">
        {format.bannedItems.length === 0
          ? 'All items allowed'
          : `${format.bannedItems.length} item(s) banned`}
        {' · '}
        {format.forceMaxIvs ? 'Max IVs assumed' : 'IVs unknown'}
      </p>
      <p className="mt-1 text-xs text-ink-muted">
        {format.teraEnabled && format.megaEnabled
          ? 'Tera + Mega'
          : format.teraEnabled
            ? 'Tera only'
            : format.megaEnabled
              ? 'Mega only'
              : 'No transformation mechanic'}
      </p>
    </div>
  )
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

function FormatEditor({
  initial,
  onSave,
  onCancel,
}: {
  initial: StoredFormat
  onSave: (format: StoredFormat) => void
  onCancel: () => void
}) {
  const [draft, setDraft] = useState<StoredFormat>(initial)
  const [catalog, setCatalog] = useState<CatalogItem[]>([])
  const [search, setSearch] = useState('')

  useEffect(() => {
    void fetchItemCatalog().then(setCatalog)
  }, [])

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase()
    // Show the complete Champions item list.
    return q ? catalog.filter((i) => i.label.toLowerCase().includes(q)) : catalog
  }, [catalog, search])

  const banned = new Set(draft.bannedItems)
  const toggleItem = (name: string) => {
    const next = new Set(banned)
    if (next.has(name)) next.delete(name)
    else next.add(name)
    setDraft({ ...draft, bannedItems: [...next] })
  }

  // Keep `active <= brought <= total <= 6` after a value changes.
  const setNumbers = (field: 'activePokemon' | 'broughtPokemon' | 'totalPokemon', raw: number) => {
    let { activePokemon, broughtPokemon, totalPokemon } = draft
    if (field === 'totalPokemon') totalPokemon = clamp(raw, 1, 6)
    if (field === 'broughtPokemon') broughtPokemon = clamp(raw, 1, 6)
    if (field === 'activePokemon') activePokemon = clamp(raw, 1, 6)
    totalPokemon = clamp(totalPokemon, 1, 6)
    broughtPokemon = clamp(broughtPokemon, 1, totalPokemon)
    activePokemon = clamp(activePokemon, 1, broughtPokemon)
    setDraft({ ...draft, activePokemon, broughtPokemon, totalPokemon })
  }

  return (
    <div className="col-span-2 rounded-card bg-card p-4 shadow-md">
      <input
        value={draft.name}
        onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        placeholder="Format name"
        className="mb-3 w-full rounded-card border border-subtle bg-surface px-2 py-1.5 text-sm font-medium outline-none focus:border-primary"
      />
      <div className="mb-3 grid grid-cols-3 gap-3">
        {(
          [
            ['activePokemon', 'Active'],
            ['broughtPokemon', 'Brought'],
            ['totalPokemon', 'Total'],
          ] as const
        ).map(([field, label]) => (
          <label key={field} className="text-xs text-ink-muted">
            {label}
            <input
              type="number"
              min={1}
              max={6}
              value={draft[field]}
              onChange={(e) => setNumbers(field, Number(e.target.value))}
              className="mt-1 w-full rounded-card border border-subtle bg-surface px-2 py-1.5 text-sm text-ink outline-none focus:border-primary"
            />
          </label>
        ))}
      </div>

      <label className="mb-3 flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={draft.forceMaxIvs}
          onChange={(e) => setDraft({ ...draft, forceMaxIvs: e.target.checked })}
          className="h-4 w-4 rounded border-subtle"
        />
        <span>Assume max IVs (31)</span>
      </label>

      <div className="mb-3 flex flex-wrap gap-x-5 gap-y-2">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.teraEnabled}
            onChange={(e) => setDraft({ ...draft, teraEnabled: e.target.checked })}
            className="h-4 w-4 rounded border-subtle"
          />
          <span>Allow Terastallization</span>
        </label>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.megaEnabled}
            onChange={(e) => setDraft({ ...draft, megaEnabled: e.target.checked })}
            className="h-4 w-4 rounded border-subtle"
          />
          <span>Allow Mega Evolution</span>
        </label>
      </div>

      <div className="mb-2">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search items… (click to ban/allow)"
          className="w-full rounded-card border border-subtle bg-surface px-2 py-1.5 text-sm outline-none focus:border-primary"
        />
      </div>
      <div className="mb-3 grid max-h-64 grid-cols-6 gap-2 overflow-y-auto">
        {filtered.map((item) => {
          const isBanned = banned.has(item.name)
          return (
            <button
              key={item.name}
              onClick={() => toggleItem(item.name)}
              title={`${item.label}${isBanned ? ' (banned)' : ''}`}
              className={`lift flex flex-col items-center rounded-card border p-1.5 text-center ${
                isBanned ? 'border-danger opacity-35 grayscale' : 'border-subtle'
              }`}
            >
              <img
                src={cachedImageUrl(itemSpriteUrl(item.name))}
                alt={item.label}
                width={30}
                height={30}
                loading="lazy"
                onError={(e) => {
                  ;(e.target as HTMLImageElement).style.visibility = 'hidden'
                }}
              />
              <span className="mt-1 line-clamp-2 text-[10px] leading-tight text-ink-muted">
                {item.label}
              </span>
            </button>
          )
        })}
      </div>

      <div className="flex justify-end gap-2">
        <button onClick={onCancel} className="lift rounded-card border border-subtle px-3 py-1.5 text-sm">
          Cancel
        </button>
        <button
          onClick={() => onSave({ ...draft, name: draft.name.trim() || 'Untitled Format' })}
          className="lift rounded-card bg-primary px-3 py-1.5 text-sm font-medium text-white"
        >
          Save
        </button>
      </div>
    </div>
  )
}
