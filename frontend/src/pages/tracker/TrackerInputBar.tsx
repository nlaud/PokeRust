import { useEffect, useMemo, useRef, useState } from 'react'
import { useTracker } from '../../store/trackerStore'
import { completionsAt, isSelfCompleteToken, norm } from '../../lib/trackerGrammar'
import TrackerSolverPanel from './TrackerSolverPanel'

/** Limits the height of the suggestion list. */
const MAX_SUGGESTIONS = 6

/** Stores one uncommitted draft line for keyboard navigation. */
interface HistoryEntry {
  lineIndex: number
  text: string
}

function buildHistory(draftLines: string[]): HistoryEntry[] {
  return draftLines.map((text, lineIndex) => ({ lineIndex, text }))
}

/** Splits text before the caret into complete tokens and one partial token. */
function tokensAndPartial(before: string): { tokens: string[]; partial: string } {
  const parts = before.split(/\s+/)
  const partial = parts[parts.length - 1]
  const tokens = parts.slice(0, -1).filter((t) => t !== '')
  return { tokens, partial }
}

/** Replaces the token at the caret with a suggestion.
 * Adds one separator when necessary.
 * Returns the new text and caret position. */
function applyTopSuggestion(
  current: string,
  caret: number,
  suggestion: string,
): { text: string; caret: number } {
  const before = current.slice(0, caret)
  const after = current.slice(caret)
  const { partial } = tokensAndPartial(before)
  const prefix = before.slice(0, before.length - partial.length)
  // Add a space unless the remaining text starts with one.
  // This places the caret at the next word.
  const separator = /^\s/.test(after) ? '' : ' '
  const text = `${prefix}${suggestion}${separator}${after}`
  return { text, caret: prefix.length + suggestion.length + separator.length }
}

/** Edits tracker events one line at a time.
 * Tab accepts the first completion.
 * Arrow keys navigate draft lines and tokens.
 * Enter saves or removes one draft line.
 * Shift+Enter commits the turn and rebuilds inference.
 * Escape discards line changes.
 * Shift+Escape discards the draft or reopens the latest committed turn. */
export default function TrackerInputBar() {
  const {
    view,
    completions,
    previewEvents,
    lastLineWarning,
    error,
    errorLine,
    busy,
    previewDraft,
    clearPreview,
    endTurn,
    popLastCommittedTurn,
    clearError,
  } = useTracker()

  const [draftLines, setDraftLines] = useState<string[]>([])
  const [text, setText] = useState('')
  const [caretPos, setCaretPos] = useState(0)
  const [historyIndex, setHistoryIndex] = useState(0)
  // Records whether Shift+Escape reopened the last committed turn.
  // The next press discards that draft.
  const [reopenedCommittedTurn, setReopenedCommittedTurn] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  // A disabled input loses focus during a rebuild.
  // Restore focus when the rebuild finishes.
  useEffect(() => {
    if (!busy) inputRef.current?.focus()
  }, [busy])

  const history = useMemo(() => buildHistory(draftLines), [draftLines])
  const currentEntry = historyIndex < history.length ? history[historyIndex] : null

  const { tokens, partial } = tokensAndPartial(text.slice(0, caretPos))
  const atEndOfInput = caretPos === text.length
  const suggestions = useMemo(
    () => completionsAt(tokens, tokens.length, partial, completions),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [text, caretPos, completions],
  )
  // Hide suggestions when the current word matches a valid token.
  // Match without case or punctuation differences.
  // Also treat numeric HP values as complete tokens.
  const partialIsComplete =
    partial !== '' && (isSelfCompleteToken(partial) || suggestions.some((s) => norm(s) === norm(partial)))
  const ghost =
    !partialIsComplete &&
    atEndOfInput &&
    suggestions.length > 0 &&
    partial !== '' &&
    norm(suggestions[0]).startsWith(norm(partial))
      ? suggestions[0].slice(partial.length)
      : ''

  /** Loads text and moves the caret to its end. */
  function loadBuffer(newText: string) {
    setText(newText)
    setCaretPos(newText.length)
    requestAnimationFrame(() => inputRef.current?.setSelectionRange(newText.length, newText.length))
  }

  function jumpToAppend(nextDraftLines: string[]) {
    setHistoryIndex(nextDraftLines.length)
    loadBuffer('')
  }

  /** Deletes one draft line.
   * Then loads the previous line or a new empty line. */
  function deleteDraftLine(lineIndex: number) {
    const next = draftLines.filter((_, i) => i !== lineIndex)
    setDraftLines(next)
    void previewDraft(next)
    if (next.length === 0) {
      jumpToAppend(next)
      return
    }
    const prevIndex = Math.max(0, lineIndex - 1)
    setHistoryIndex(prevIndex)
    loadBuffer(next[prevIndex])
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
    if (suggestions.length === 0 || partialIsComplete) return
    const { text: next, caret } = applyTopSuggestion(text, caretPos, suggestions[0])
    setText(next)
    setCaretPos(caret)
    requestAnimationFrame(() => inputRef.current?.setSelectionRange(caret, caret))
  }

  async function handleEnter() {
    const trimmed = text.trim()
    if (trimmed === '') {
      if (currentEntry) {
        deleteDraftLine(currentEntry.lineIndex)
        return
      }
      jumpToAppend(draftLines)
      return
    }
    if (currentEntry) {
      const insertIndex = currentEntry.lineIndex + 1
      const next = [...draftLines]
      next[currentEntry.lineIndex] = trimmed
      // Insert an empty line after the saved line.
      // This lets the user add a missed event inside the turn.
      // Saving the empty line removes it.
      next.splice(insertIndex, 0, '')
      setDraftLines(next)
      void previewDraft(next)
      setHistoryIndex(insertIndex)
      loadBuffer('')
      return
    }
    const next = [...draftLines, trimmed]
    setDraftLines(next)
    void previewDraft(next)
    jumpToAppend(next)
  }

  function handleEscape() {
    // Discard the input buffer and select a new event line.
    jumpToAppend(draftLines)
  }

  async function handleShiftEnter() {
    const trimmed = text.trim()
    let finalDraft = draftLines
    if (currentEntry && trimmed === '') {
      // Remove an existing empty line before the turn ends.
      finalDraft = draftLines.filter((_, i) => i !== currentEntry.lineIndex)
    } else if (currentEntry && trimmed !== '') {
      finalDraft = [...draftLines]
      finalDraft[currentEntry.lineIndex] = trimmed
    } else if (trimmed !== '') {
      finalDraft = [...draftLines, trimmed]
    }
    // Remove other empty inserted lines before the server stores the turn.
    finalDraft = finalDraft.filter((l) => l.trim() !== '')
    if (finalDraft.length === 0) return
    const ok = await endTurn(finalDraft)
    if (ok) {
      setDraftLines([])
      setReopenedCommittedTurn(false)
      jumpToAppend([])
    }
  }

  async function handleShiftEscape() {
    if (draftLines.length > 0 || text.trim() !== '' || reopenedCommittedTurn) {
      // The first Shift+Escape discards the current draft.
      // Do not reopen a committed turn while unsaved lines exist.
      clearPreview()
      setDraftLines([])
      setReopenedCommittedTurn(false)
      jumpToAppend([])
      return
    }
    const popped = await popLastCommittedTurn()
    if (!popped) {
      // If no committed turn exists, discard the draft and start a new one.
      clearPreview()
      setDraftLines([])
      jumpToAppend([])
      return
    }
    // `popLastCommittedTurn` rebuilds the server session without the last turn.
    // Restore that turn's lines to the draft.
    setDraftLines(popped)
    setReopenedCommittedTurn(true)
    jumpToAppend(popped)
    void previewDraft(popped)
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (busy) return
    if (e.key === 'Tab') {
      e.preventDefault()
      acceptTopSuggestion()
      return
    }
    if (e.key === 'Backspace' && text === '' && currentEntry) {
      // Backspace removes an existing empty line.
      // Let the browser handle Backspace on the new append line.
      e.preventDefault()
      deleteDraftLine(currentEntry.lineIndex)
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
      if (e.shiftKey) void handleShiftEscape()
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
        {suggestions.length > 0 && !partialIsComplete && (
          // Continue the input bar background behind the suggestion list.
          // Position the list directly above the outer bar.
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

        <div className="relative flex items-center rounded-card border border-subtle bg-card">
          {/* Line-number gutter: the current draft line, 1-based — matches the
              server's own `Line N` error numbering (F3/"line number
              indicator"). */}
          <span
            data-testid="tracker-line-number"
            className="select-none pl-2.5 pr-1 font-mono text-sm text-ink-muted"
          >
            {historyIndex + 1}
          </span>
          <div className="relative min-w-0 flex-1">
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
      </div>

      <div className="flex items-center justify-between px-3 pb-1 pt-1">
        <span className="text-[11px] text-ink-muted">
          Enter: save · Shift+Enter: end turn · Esc: cancel · Shift+Esc: clear draft / reopen last turn
        </span>
        {previewEvents.length > 0 && (
          <span className="text-[11px] text-ink-muted">
            {previewEvents.length} event(s) pending this turn
          </span>
        )}
      </div>

      {/* The solver answer for the last committed turn. The bar is anchored at
          its bottom edge, so this panel grows the bar upward. */}
      <TrackerSolverPanel />

      {/* Non-blocking advisory (yellow): the line just committed/edited parsed
          fine but had no observable effect — distinct from `error` below
          (red), which means the turn was actually rejected. Auto-clears the
          next time `previewDraft` runs and the tree actually changes, so
          there's no dismiss button — see `lastLineWarning`'s doc comment. */}
      {lastLineWarning && !error && (
        <div className="mx-1 mb-1 mt-1 rounded-card bg-warning px-3 py-1.5 text-xs text-white">
          {lastLineWarning}
        </div>
      )}

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
