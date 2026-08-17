---
name: todo
description: Take the topmost item of TODO.md with all of its sub-bullets, or take the given text, then research it, write a plan, get the plan approved, implement it, and commit it on the current branch. Report the next three TODO items at the end. Use this skill when the user types "/todo", or asks to "work the next TODO item", "take the top TODO and plan it", or "plan it and ping me for approval". The skill runs safely in a loop and finishes an active task before it starts a new one.
argument-hint: [task description]
disable-model-invocation: true
allowed-tools:
  - Agent
  - PushNotification
  - SendUserFile
  - Read
---

# TODO

A subagent does the whole task. This file is the dispatcher. Start the subagent,
then report what it returns.

Arguments: `$ARGUMENTS`

## Why a subagent

The task reads source files, runs `cargo test`, and runs a Codex review. Those
steps produce many large tool results. A subagent keeps those results out of the
main thread. The main thread then stays cheap across many loop runs.

## Why the main thread reports

The user never sees the subagent report. Only this thread reaches the user.
Print what the user needs, and send the push notification from here.

The subagent sends no notification. Two senders would ping the user two times.

## 1. Collect the handoff

The subagent cannot read this conversation. Collect these facts first.

1. Find the newest user message in this conversation.
2. Record its full text. It holds the answer of the user to an earlier question
   of the subagent.
3. If the newest user message is only the skill invocation, record `none`.

This skill needs no channel and no external service. The conversation is the
channel.

## 2. Start the subagent

Call `Agent` one time with these settings:

- `subagent_type`: `general-purpose`
- `run_in_background`: `false`
- `description`: `TODO task`

Use this prompt. Replace each angle-bracket field with a recorded value.

```text
Read C:\Users\natha\OneDrive\Documents\GitHub\PokeRust\.claude\skills\todo\WORKFLOW.md
and follow it exactly. It is the full procedure. Also follow CLAUDE.md at the
repository root.

Handoff block:
- latest_message: <full text of the newest user message, or none>
- arguments: <$ARGUMENTS, or the word none>

You send no push notification. The main thread sends it.
End your report with the next-item block, the screenshot block, and one line
from the return contract in WORKFLOW.md, in that order.
```

Start one subagent for each run. Never start two at the same time.

Add a fact to the prompt when this thread knows something that `state.json` does
not hold. A running job and the current time are two examples.

## 3. Report the result

Map the last line of the subagent report to this action:

| Return line | Action |
|---|---|
| `PAUSED: awaiting-approval` | Print the plan summary. Surface `.claude/todo/plan.md` with `SendUserFile`. Ask for an approval. |
| `PAUSED: question` | Print the question and the options. |
| `DONE: <hash>` | Print the hash, the subject, and the facts that the user must know. Then print the next-item block. |
| `BLOCKED: <error>` | Print the error and the repository state. |

Print the facts that change what the user does next. A defect that the Codex
review found is one example. A limit that the run left in place is another.

Never invent a result. If the subagent returns no contract line, do not guess
the outcome. See the section *A run with no status*.

### The next-item block

A `DONE:` report holds a `NEXT:` block with the next three `TODO.md` items.
Print that block under the heading `Next 3 TODO items`. Keep the order of the
block.

Print the items exactly as the subagent wrote them. Never invent an item, and
never read `TODO.md` to build the list yourself. The subagent reads the file
after it removes the finished item, so only its list is current.

Print nothing for this section when the block reads `NEXT: none`.

If a `DONE:` report holds no `NEXT:` block, ask the subagent for one with
`SendMessage`.

## 4. Show the screenshots

Every subagent report holds a `SCREENSHOTS:` block near its end. Read that
block.

1. If the block lists one path or more, call `SendUserFile` with every path.
2. Set `status` to `normal` and `display` to `render`.
3. Write a one-line caption that names the task.
4. If the block reads `none`, show nothing.

Pass the paths as a JSON array of strings. Use forward slashes. A Windows
backslash breaks the argument.

Show the screenshots on every run that captured one. The user never reads the
subagent report, so an unshown screenshot never reaches the user.

If the report holds no screenshot block, ask the subagent for one with
`SendMessage`. Never guess a path.

The subagent writes each screenshot to `.claude/todo/screenshots/`. Delete no
screenshot after you show it. The next run removes the whole directory, and it
also removes the other temporary files of this run.

## 5. Send the push notification

Send one push notification for each run. This step is required.

Call `PushNotification` after you print the report. The content must exist
before the ping arrives.

Keep the message under 200 characters. Lead with the action that the user must
take.

| Return line | Message shape |
|---|---|
| `PAUSED: awaiting-approval` | `Plan ready for approval: <task>` |
| `PAUSED: question` | `Needs a decision: <the short question>` |
| `DONE: <hash>` | `Committed <hash>: <subject>` |
| `BLOCKED: <error>` | `Blocked: <error>` |

The tool skips the notification when the user sits at the terminal. That result
is correct. Send the notification every time, and let the tool decide.

## 6. Resume the loop

A `PAUSED` return ends the turn. The next user message starts the next run.

When that message arrives, repeat sections 1 through 5. Record the new message
text. Start a fresh subagent. The subagent reads `.claude/todo/state.json` and
continues at the stored stage.

Never ask the subagent to wait for an answer. A subagent has no turn to end. It
must return instead.

## A run with no status

A subagent can stop without a contract line. It can also stop while it waits for
a job that it started.

1. Read the last words of the report.
2. If the subagent stalled mid-stage, resume it by name with `SendMessage`. Tell
   it that it cannot block, and ask for a contract line.
3. If the report shows no work at all, print `the subagent returned no status`
   and stop.

Resume the agent when its context still holds the work. A fresh subagent starts
from `state.json` and loses everything after the last stage write.

## Before you start a run

A run costs several minutes and one notification. Skip the run when it can
change nothing.

Check the state that the run would wait on. A job with two hours left is one
example. Report the arithmetic, and ask the user whether to dispatch.

This check is main-thread work. It is not a step of the task.

## Guard rails

- Do the work in the subagent. Do not do a step yourself because it looks small.
- Deciding whether to dispatch is main-thread work. That check is allowed.
- Never commit `.claude/todo/`. It holds loop state, not project work.
- The subagent removes the temporary files. Delete no file from this thread.
- Report a leftover artifact that the user must know about. Do not remove it
  yourself.
