import { useEffect, useMemo, useRef, useState } from 'react'
import { useTracker } from '../../store/trackerStore'
import { completionsAt, contentLinesOf, norm } from '../../lib/trackerGrammar'

/** Cap on the rising suggestion list — the glass panel above the bar grows to
 * fit however many render, so this bounds how tall it can get. */
const MAX_SUGGESTIONS = 6

/** One addressable line in the flat, terminal-style navigation history:
 * either an already-committed turn's content line (editing it rebuilds
 * through `editCommitted`) or a line in the current, still-uncommitted draft
 * turn (editing it is purely local until the turn is ended). */
type HistoryEntry =
  | { kind: 'committed'; turnIndex: number; lineIndex: number; text: string }
  | { kind: 'draft'; lineIndex: number; text: string }

function buildHistory(committedTurns: string[], draftLines: string[]): HistoryEntry[] {
  const out: HistoryEntry[] = []
  committedTurns.forEach((turnText, turnIndex) => {
    contentLinesOf(turnText).forEach((text, lineIndex) => {
      out.push({ kind: 'committed', turnIndex, lineIndex, text })
    })
  })
  draftLines.forEach((text, lineIndex) => out.push({ kind: 'draft', lineIndex, text }))
  return out
}

function flatCommittedCount(committedTurns: string[]): number {
  return committedTurns.reduce((n, t) => n + contentLinesOf(t).length, 0)
}

/** Split `before` (the text up to the caret) into completed tokens and the
 * partial word currently being typed — the unit `completionsAt` ranks
 * suggestions for. */
function tokensAndPartial(before: string): { tokens: string[]; partial: string } {
  const parts = before.split(/\s+/)
  const partial = parts[parts.length - 1]
  const tokens = parts.slice(0, -1).filter((t) => t !== '')
  return { tokens, partial }
}

/** Replace the word under the caret with `suggestion`, inserting exactly one
 * separating space before whatever followed the caret (none if `after` is
 * empty or already starts with whitespace). Returns the new full text and
 * the caret position immediately after the inserted word. */
function applyTopSuggestion(
  current: string,
  caret: number,
  suggestion: string,
): { text: string; caret: number } {
  const before = current.slice(0, caret)
  const after = current.slice(caret)
  const { partial } = tokensAndPartial(before)
  const prefix = before.slice(0, before.length - partial.length)
  // A trailing space is wanted whenever `after` doesn't already start with
  // one — including (this is the common case) when `after` is empty, so the
  // caret lands ready to type the next word immediately.
  const separator = /^\s/.test(after) ? '' : ' '
  const text = `${prefix}${suggestion}${separator}${after}`
  return { text, caret: prefix.length + suggestion.length + separator.length }
}

/**
 * The floating, glassy, single-line tracker input — replaces the plain
 * textarea `TrackerScreen.tsx` used to render directly. Minecraft-chat-style
 * word completion (ranked suggestion list rising above the bar, ghost text
 * for the top candidate, Tab to accept), terminal-style history navigation
 * (ArrowUp/Down over every line ever typed — both this session's committed
 * turns AND the in-progress draft — Alt+Left/Right to jump by word), and the
 * two-tier commit model: `Enter` saves the current line locally (or, when
 * positioned on an already-committed line, immediately rebuilds through it —
 * see `editCommitted`'s doc comment) and jumps to a fresh event; `Shift+Enter`
 * additionally ends the turn, appending `endofturn` and rebuilding the whole
 * script so inference recomputes for real; `Escape` discards the current
 * line's unsaved edits without jumping anywhere new-content-wise;
 * `Shift+Escape` discards the whole in-progress draft (never sent to the
 * server, so purely local) and reopens the last committed line for editing.
 */
export default function TrackerInputBar() {
  const {
    view,
    completions,
    committedTurns,
    previewEvents,
    error,
    errorLine,
    busy,
    previewDraft,
    clearPreview,
    endTurn,
    editCommitted,
    clearError,
  } = useTracker()

  const [draftLines, setDraftLines] = useState<string[]>([])
  const [text, setText] = useState('')
  const [caretPos, setCaretPos] = useState(0)
  const [historyIndex, setHistoryIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  // `disabled={busy}` makes the browser auto-blur the input the instant a
  // rebuild starts (native behavior for disabling a focused element) — it
  // does NOT regain focus on its own once re-enabled, which would silently
  // strand every subsequent keystroke (ArrowUp/Enter/etc.) with nothing
  // listening. Explicitly reclaim focus the moment busy work finishes.
  useEffect(() => {
    if (!busy) inputRef.current?.focus()
  }, [busy])

  const history = useMemo(() => buildHistory(committedTurns, draftLines), [committedTurns, draftLines])
  const currentEntry = historyIndex < history.length ? history[historyIndex] : null

  const { tokens, partial } = tokensAndPartial(text.slice(0, caretPos))
  const atEndOfInput = caretPos === text.length
  const suggestions = useMemo(
    () => completionsAt(tokens, tokens.length, partial, completions),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [text, caretPos, completions],
  )
  const ghost =
    atEndOfInput && suggestions.length > 0 && partial !== '' && norm(suggestions[0]).startsWith(norm(partial))
      ? suggestions[0].slice(partial.length)
      : ''

  /** Load `newText` into the buffer with the caret at its end — every
   * navigation/reset action (ArrowUp/Down, jump-to-append, Shift+Escape's
   * reopen) wants exactly this. */
  function loadBuffer(newText: string) {
    setText(newText)
    setCaretPos(newText.length)
    requestAnimationFrame(() => inputRef.current?.setSelectionRange(newText.length, newText.length))
  }

  function jumpToAppend(nextDraftLines: string[]) {
    setHistoryIndex(flatCommittedCount(committedTurns) + nextDraftLines.length)
    loadBuffer('')
  }

  async function jumpToAppendAfterRebuild() {
    // The store's `set()` inside `endTurn`/`editCommitted` has already run by
    // the time the awaited promise resolves, so the freshest `committedTurns`
    // is available synchronously here — no need to wait for a re-render.
    const fresh = useTracker.getState().committedTurns
    setHistoryIndex(flatCommittedCount(fresh) + draftLines.length)
    loadBuffer('')
  }

  function moveCaretByWord(direction: -1 | 1) {
    let pos = caretPos
    if (direction === -1) {
      while (pos > 0 && /\s/.test(text[pos - 1])) pos--
      while (pos > 0 && !/\s/.test(text[pos - 1])) pos--
    } else {
      while (pos < text.length && /\s/.test(text[pos])) pos++
      while (pos < text.length && !/\s/.test(text[pos])) pos++
    }
    setCaretPos(pos)
    inputRef.current?.setSelectionRange(pos, pos)
  }

  function acceptTopSuggestion() {
    if (suggestions.length === 0) return
    const { text: next, caret } = applyTopSuggestion(text, caretPos, suggestions[0])
    setText(next)
    setCaretPos(caret)
    requestAnimationFrame(() => inputRef.current?.setSelectionRange(caret, caret))
  }

  async function handleEnter() {
    const trimmed = text.trim()
    if (trimmed === '') {
      jumpToAppend(draftLines)
      return
    }
    if (currentEntry?.kind === 'committed') {
      const ok = await editCommitted(currentEntry.turnIndex, currentEntry.lineIndex, trimmed)
      if (ok) await jumpToAppendAfterRebuild()
      return
    }
    if (currentEntry?.kind === 'draft') {
      const next = [...draftLines]
      next[currentEntry.lineIndex] = trimmed
      setDraftLines(next)
      void previewDraft(next)
      jumpToAppend(next)
      return
    }
    const next = [...draftLines, trimmed]
    setDraftLines(next)
    void previewDraft(next)
    jumpToAppend(next)
  }

  function handleEscape() {
    // Discard whatever's in the buffer (including an in-progress edit to an
    // existing line) and land on a fresh event — content never changes.
    jumpToAppend(draftLines)
  }

  async function handleShiftEnter() {
    if (currentEntry?.kind === 'committed') {
      const trimmed = text.trim()
      if (trimmed === '') return
      const ok = await editCommitted(currentEntry.turnIndex, currentEntry.lineIndex, trimmed)
      if (ok) await jumpToAppendAfterRebuild()
      return
    }
    let finalDraft = draftLines
    const trimmed = text.trim()
    if (currentEntry?.kind === 'draft' && trimmed !== '') {
      finalDraft = [...draftLines]
      finalDraft[currentEntry.lineIndex] = trimmed
    } else if (trimmed !== '') {
      finalDraft = [...draftLines, trimmed]
    }
    if (finalDraft.length === 0) return
    const ok = await endTurn(finalDraft)
    if (ok) {
      setDraftLines([])
      const fresh = useTracker.getState().committedTurns
      setHistoryIndex(flatCommittedCount(fresh))
      loadBuffer('')
    }
  }

  function handleShiftEscape() {
    clearPreview()
    setDraftLines([])
    const flatCount = flatCommittedCount(committedTurns)
    if (flatCount === 0) {
      setHistoryIndex(0)
      loadBuffer('')
      return
    }
    // The committed prefix of `history` is unaffected by clearing draftLines,
    // so it's still safe to read from the pre-update array here.
    setHistoryIndex(flatCount - 1)
    loadBuffer(history[flatCount - 1]?.text ?? '')
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (busy) return
    if (e.key === 'Tab') {
      e.preventDefault()
      acceptTopSuggestion()
      return
    }
    if (e.key === 'ArrowUp' && !e.shiftKey) {
      e.preventDefault()
      if (historyIndex > 0) {
        const next = historyIndex - 1
        setHistoryIndex(next)
        loadBuffer(history[next].text)
      }
      return
    }
    if (e.key === 'ArrowDown' && !e.shiftKey) {
      e.preventDefault()
      if (historyIndex < history.length) {
        const next = historyIndex + 1
        setHistoryIndex(next)
        loadBuffer(next >= history.length ? '' : history[next].text)
      }
      return
    }
    if (e.key === 'ArrowLeft' && e.altKey) {
      e.preventDefault()
      moveCaretByWord(-1)
      return
    }
    if (e.key === 'ArrowRight' && e.altKey) {
      e.preventDefault()
      moveCaretByWord(1)
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      if (e.shiftKey) handleShiftEscape()
      else handleEscape()
      return
    }
    if (e.key === 'Enter') {
      e.preventDefault()
      if (e.shiftKey) void handleShiftEnter()
      else void handleEnter()
    }
  }

  if (!view) return null

  return (
    <div className="glass-soft absolute inset-x-0 bottom-3 z-20 mx-auto w-full max-w-3xl rounded-card px-1 py-1 shadow-lg">
      <div>
        {suggestions.length > 0 && (
          // Its own glass-soft layer (same treatment as the outer bar, no
          // gap below) so the blur/tint visually continues upward over every
          // option, sitting directly against the input with nothing between
          // them. Positioned `bottom-full` against the OUTER bar (this
          // element has no `relative` of its own — see below).
          <div className="glass-soft absolute inset-x-3 bottom-full flex flex-col-reverse gap-0.5 rounded-t-card px-1 pb-1 pt-1.5">
            {suggestions.slice(0, MAX_SUGGESTIONS).map((s, i) => (
              <div
                key={`${s}-${i}`}
                data-testid={i === 0 ? 'tracker-suggestion-top' : 'tracker-suggestion'}
                className={`truncate rounded px-2 py-0.5 text-sm ${
                  i === 0 ? 'font-semibold text-primary' : 'text-ink-muted'
                }`}
              >
                {s}
              </div>
            ))}
          </div>
        )}

        <div className="relative rounded-card border border-subtle bg-card">
          {/* Ghost-text overlay: identical font/padding to the real input on
              top, or the ghost drifts out of alignment with the caret. The
              input itself must stay bg-transparent — an opaque input
              background paints over this layer entirely. */}
          <div
            aria-hidden
            className="pointer-events-none absolute inset-0 flex items-center overflow-hidden whitespace-pre px-2.5 font-mono text-lg"
          >
            <span className="invisible">{text}</span>
            <span className="text-ink-muted" data-testid="tracker-ghost">
              {ghost}
            </span>
          </div>
          <input
            ref={inputRef}
            data-testid="tracker-input"
            value={text}
            disabled={busy}
            onChange={(e) => {
              setText(e.target.value)
              setCaretPos(e.target.selectionStart ?? e.target.value.length)
            }}
            onKeyDown={handleKeyDown}
            onSelect={(e) => setCaretPos(e.currentTarget.selectionStart ?? 0)}
            onClick={(e) => setCaretPos(e.currentTarget.selectionStart ?? 0)}
            spellCheck={false}
            autoComplete="off"
            placeholder="Input Event"
            className="relative w-full bg-transparent px-2.5 py-1.5 font-mono text-lg text-ink outline-none disabled:opacity-60"
          />
        </div>
      </div>

      <div className="flex items-center justify-between px-3 pb-1 pt-1">
        <span className="text-[11px] text-ink-muted">
          Enter: save · Shift+Enter: end turn · Esc: cancel · Shift+Esc: reopen last
        </span>
        {previewEvents.length > 0 && (
          <span className="text-[11px] text-ink-muted">
            {previewEvents.length} event(s) pending this turn
          </span>
        )}
      </div>

      {error && (
        <div className="mx-1 mb-1 mt-1 flex items-center justify-between gap-2 rounded-card bg-danger px-3 py-1.5 text-xs text-white">
          <span>{errorLine !== null ? `Line ${errorLine}: ${error}` : error}</span>
          <button onClick={clearError} className="font-bold" aria-label="Dismiss error">
            ✕
          </button>
        </div>
      )}
    </div>
  )
}
