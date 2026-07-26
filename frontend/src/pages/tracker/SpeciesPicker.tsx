import { useEffect, useMemo, useRef, useState } from 'react'
import { levenshtein, norm } from '../../lib/trackerGrammar'

const MAX_SUGGESTIONS = 6

/** Rank `catalog` against what the user has typed.
 *
 * Prefix matches first, then substring matches, each alphabetically — a
 * thousand-entry species list is only navigable in alphabetical order, which is
 * why this doesn't reuse `trackerGrammar`'s `rank` (that deliberately scrambles
 * via `stableHash`, which suits its small fixed keyword pools and would make
 * "gar" return Garganacl, Garchomp, Gardevoir in an arbitrary order here).
 *
 * The edit-distance tail is the same autocorrect idea though: a typo like
 * `garchmop` still surfaces Garchomp as the top, Tab-fillable hit instead of an
 * empty list. Nothing is suggested for an empty query — dumping the head of the
 * alphabet at someone who hasn't typed anything is noise. */
function rankSpecies(catalog: string[], partial: string): string[] {
  const p = norm(partial)
  if (p === '') return []
  const prefix: string[] = []
  const substring: string[] = []
  for (const name of catalog) {
    const n = norm(name)
    if (n.startsWith(p)) prefix.push(name)
    else if (n.includes(p)) substring.push(name)
  }
  if (prefix.length > 0 || substring.length > 0) {
    prefix.sort()
    substring.sort()
    return [...prefix, ...substring]
  }
  return [...catalog].sort((a, b) => {
    const d = levenshtein(norm(a), p) - levenshtein(norm(b), p)
    return d !== 0 ? d : a.localeCompare(b)
  })
}

/** Resolve one free-typed or pasted word to its canonical catalog spelling,
 * matching the way the Rust side normalizes names (`norm` mirrors
 * `tracker_parse.rs::norm`), so `flutter-mane`, `Flutter Mane`, and
 * `fluttermane` all land on the same entry. Returns `null` if it isn't a real
 * species — the whole point of this control is that those never reach the
 * server, where an unparseable name is silently dropped by the teamsheet parser
 * and only resurfaces as a confusing "N parsed but the format brings M". */
function resolve(catalog: string[], word: string): string | null {
  const n = norm(word)
  if (n === '') return null
  return catalog.find((name) => norm(name) === n) ?? null
}

/**
 * The opponent roster field for tracker mode's Closed Team Sheet: a set of
 * species chips plus a filtering text input, in place of the free-text
 * `<textarea>` the sheet-based modes still use.
 *
 * Closed sheet is exactly the case where a chip picker fits — only species are
 * known, there is no item/ability/move data to paste, and every entry is
 * validated at pick time. The Open Team Sheet modes keep the textarea, since
 * there the user genuinely does have a full sheet to paste.
 */
export default function SpeciesPicker({
  value,
  catalog,
  minCount,
  loading,
  onChange,
}: {
  value: string[]
  catalog: string[]
  minCount: number
  loading: boolean
  onChange: (species: string[]) => void
}) {
  const [query, setQuery] = useState('')
  const [highlight, setHighlight] = useState(0)
  const rootRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const suggestions = useMemo(
    () => rankSpecies(catalog, query).slice(0, MAX_SUGGESTIONS),
    [catalog, query],
  )
  // Any narrowing of the list invalidates the old index — without this, typing
  // one more character while the highlight sat on row 5 would leave it pointing
  // past the end of a now-shorter list.
  useEffect(() => setHighlight(0), [query])

  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setQuery('')
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [])

  const add = (name: string) => {
    onChange([...value, name])
    setQuery('')
  }
  const removeAt = (i: number) => onChange(value.filter((_, j) => j !== i))

  /** Split pasted text on the separators a roster is realistically pasted with
   * and take every word that resolves; anything that doesn't is left behind in
   * the input so the user can see and fix it rather than losing it silently. */
  const addMany = (text: string) => {
    const words = text.split(/[,\n]/).map((w) => w.trim()).filter(Boolean)
    const added: string[] = []
    const rejected: string[] = []
    for (const word of words) {
      const resolved = resolve(catalog, word)
      if (resolved) added.push(resolved)
      else rejected.push(word)
    }
    if (added.length > 0) onChange([...value, ...added])
    setQuery(rejected.join(', '))
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (suggestions.length === 0) return
      e.preventDefault()
      setHighlight((h) => {
        const next = e.key === 'ArrowDown' ? h + 1 : h - 1
        return Math.min(suggestions.length - 1, Math.max(0, next))
      })
    } else if (e.key === 'Enter' || e.key === 'Tab' || e.key === ',') {
      // Comma commits too, so muscle memory from the old comma-separated
      // textarea still works.
      if (suggestions.length === 0) return
      e.preventDefault()
      add(suggestions[highlight] ?? suggestions[0])
    } else if (e.key === 'Backspace' && query === '' && value.length > 0) {
      e.preventDefault()
      removeAt(value.length - 1)
    } else if (e.key === 'Escape') {
      setQuery('')
    }
  }

  const short = value.length < minCount

  return (
    <div ref={rootRef} className="relative">
      <div
        onClick={() => inputRef.current?.focus()}
        className="flex min-h-[4.5rem] w-full flex-wrap content-start gap-1.5 rounded-card border border-subtle bg-card px-2 py-2 text-sm focus-within:border-primary"
      >
        {value.map((name, i) => (
          <span
            key={`${name}-${i}`}
            data-testid="species-chip"
            className="flex items-center gap-1 rounded-md bg-primary-soft px-2 py-0.5 text-xs text-primary"
          >
            {name}
            <button
              type="button"
              aria-label={`Remove ${name}`}
              onClick={(e) => {
                e.stopPropagation()
                removeAt(i)
              }}
              className="text-sm leading-none opacity-60 hover:opacity-100"
            >
              ×
            </button>
          </span>
        ))}
        <input
          ref={inputRef}
          data-testid="species-input"
          value={query}
          spellCheck={false}
          disabled={loading}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          onPaste={(e) => {
            const text = e.clipboardData.getData('text')
            if (!/[,\n]/.test(text)) return // single name — let it type normally
            e.preventDefault()
            addMany(text)
          }}
          placeholder={
            loading
              ? 'Loading species…'
              : value.length === 0
                ? 'Type a species, e.g. Flutter Mane'
                : ''
          }
          className="min-w-[10rem] flex-1 bg-transparent text-xs outline-none"
        />
      </div>

      <span className={`mt-1 block text-[11px] ${short ? 'text-warning' : 'text-ink-muted'}`}>
        {value.length} added — need at least {minCount}
      </span>

      {suggestions.length > 0 && (
        <div
          role="listbox"
          className="glass-soft absolute inset-x-0 top-full z-30 mt-1 rounded-card border border-subtle p-1 shadow-lg"
        >
          {suggestions.map((name, i) => (
            <button
              key={name}
              type="button"
              role="option"
              data-testid="species-suggestion"
              aria-selected={i === highlight}
              onMouseEnter={() => setHighlight(i)}
              onClick={() => add(name)}
              className={`block w-full rounded-md px-2.5 py-1.5 text-left text-sm transition-colors ${
                i === highlight ? 'bg-primary-soft text-primary' : ''
              }`}
            >
              {name}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
