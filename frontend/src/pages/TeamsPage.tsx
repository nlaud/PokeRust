import { useState } from 'react'
import ConfirmDialog from '../components/common/ConfirmDialog'
import Sprite from '../components/common/Sprite'
import { parseSheetSpecies } from '../lib/teamsheet'
import { loadTeams, newId, saveTeams, type StoredTeam } from '../lib/storage'

interface EditorState {
  id: string | null // null = creating a new team
  name: string
  sheet: string
}

export default function TeamsPage() {
  const [teams, setTeams] = useState<StoredTeam[]>(loadTeams)
  const [editor, setEditor] = useState<EditorState | null>(null)
  const [pendingDelete, setPendingDelete] = useState<StoredTeam | null>(null)

  const update = (next: StoredTeam[]) => {
    setTeams(next)
    saveTeams(next)
  }

  const save = () => {
    if (!editor) return
    const name = editor.name.trim() || 'Untitled Team'
    if (editor.id === null) {
      update([
        ...teams,
        {
          id: newId(),
          name,
          sheet: editor.sheet,
          favorite: false,
          updatedAt: new Date().toISOString(),
        },
      ])
    } else {
      update(
        teams.map((t) =>
          t.id === editor.id
            ? { ...t, name, sheet: editor.sheet, updatedAt: new Date().toISOString() }
            : t,
        ),
      )
    }
    setEditor(null)
  }

  const toggleFavorite = (team: StoredTeam) =>
    update(teams.map((t) => (t.id === team.id ? { ...t, favorite: !t.favorite } : t)))

  const sorted = [...teams].sort((a, b) => Number(b.favorite) - Number(a.favorite))

  return (
    <div className="mx-auto max-w-6xl p-6">
      <h1 className="mb-6 text-xl font-semibold">Teams</h1>
      <div className="grid grid-cols-3 gap-6">
        {sorted.map((team) =>
          editor?.id === team.id ? (
            <TeamEditorCard
              key={team.id}
              editor={editor}
              setEditor={setEditor}
              onSave={save}
            />
          ) : (
            <TeamCard
              key={team.id}
              team={team}
              onEdit={() => setEditor({ id: team.id, name: team.name, sheet: team.sheet })}
              onFavorite={() => toggleFavorite(team)}
              onDelete={() => setPendingDelete(team)}
            />
          ),
        )}
        {editor && editor.id === null ? (
          <TeamEditorCard editor={editor} setEditor={setEditor} onSave={save} />
        ) : (
          <button
            onClick={() => setEditor({ id: null, name: '', sheet: '' })}
            className="lift flex min-h-48 items-center justify-center rounded-card border-2 border-dashed border-subtle text-4xl text-ink-muted hover:border-primary hover:text-primary"
            aria-label="Add team"
          >
            +
          </button>
        )}
      </div>

      {pendingDelete && (
        <ConfirmDialog
          title={`Delete "${pendingDelete.name}"?`}
          message="This removes the team from local storage. This cannot be undone."
          onConfirm={() => {
            update(teams.filter((t) => t.id !== pendingDelete.id))
            setPendingDelete(null)
          }}
          onCancel={() => setPendingDelete(null)}
        />
      )}
    </div>
  )
}

function TeamCard({
  team,
  onEdit,
  onFavorite,
  onDelete,
}: {
  team: StoredTeam
  onEdit: () => void
  onFavorite: () => void
  onDelete: () => void
}) {
  const species = parseSheetSpecies(team.sheet).slice(0, 6)

  return (
    <div className="lift rounded-card bg-card p-4 shadow-sm">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="truncate text-sm font-semibold">{team.name}</h2>
        <div className="flex gap-1">
          <button
            onClick={onFavorite}
            className={`lift rounded-card p-1.5 ${team.favorite ? 'text-warning' : 'text-ink-muted'}`}
            aria-label="Favorite"
            title="Favorite"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill={team.favorite ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinejoin="round">
              <path d="m12 2 3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01z" />
            </svg>
          </button>
          <button onClick={onEdit} className="lift rounded-card p-1.5 text-ink-muted hover:text-ink" aria-label="Edit" title="Edit">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z" />
            </svg>
          </button>
          <button onClick={onDelete} className="lift rounded-card p-1.5 text-ink-muted hover:text-danger" aria-label="Delete" title="Delete">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
            </svg>
          </button>
        </div>
      </div>
      <div className="grid grid-cols-3 gap-2">
        {Array.from({ length: 6 }, (_, i) => (
          <div key={i} className="flex h-16 items-center justify-center rounded-card bg-surface">
            {species[i] ? <Sprite species={species[i]} size={56} /> : <span className="text-xs text-ink-muted">—</span>}
          </div>
        ))}
      </div>
    </div>
  )
}

function TeamEditorCard({
  editor,
  setEditor,
  onSave,
}: {
  editor: EditorState
  setEditor: (e: EditorState | null) => void
  onSave: () => void
}) {
  return (
    <div className="rounded-card bg-card p-4 shadow-md">
      <input
        value={editor.name}
        onChange={(e) => setEditor({ ...editor, name: e.target.value })}
        placeholder="Team name"
        className="mb-2 w-full rounded-card border border-subtle bg-surface px-2 py-1.5 text-sm font-medium outline-none focus:border-primary"
      />
      <textarea
        value={editor.sheet}
        onChange={(e) => setEditor({ ...editor, sheet: e.target.value })}
        placeholder={'Paste a Showdown teamsheet…\n\nGarchomp @ Choice Scarf\nAbility: Rough Skin\nLevel: 50\n...'}
        className="mb-2 h-48 w-full resize-y rounded-card border border-subtle bg-surface p-2 font-mono text-xs outline-none focus:border-primary"
        spellCheck={false}
      />
      <div className="flex justify-end gap-2">
        <button
          onClick={() => setEditor(null)}
          className="lift rounded-card border border-subtle px-3 py-1.5 text-sm"
        >
          Cancel
        </button>
        <button
          onClick={onSave}
          className="lift rounded-card bg-primary px-3 py-1.5 text-sm font-medium text-white"
        >
          Save
        </button>
      </div>
    </div>
  )
}
