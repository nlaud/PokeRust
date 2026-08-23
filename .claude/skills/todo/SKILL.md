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
  - Bash
---

# TODO

A subagent does the whole task. This file is the dispatcher. Start the subagent,
then report what it returns.

Arguments: `$ARGUMENTS`

## Why a subagent

The task reads source files, runs `cargo test`, and runs a code review. Those
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
End your report with the manual-test block, the next-item block, the screenshot
block, and one line from the return contract in WORKFLOW.md, in that order.
Report the task-start usage and the task-end usage. The main thread subtracts
them to size the next run.
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
| `DONE: <hash>` | Print the hash, the subject, and the facts that the user must know. Then print the manual-test block, the next-item block, and the auto-continue line. |
| `BLOCKED: <error>` | Print the error and the repository state. |

Print the facts that change what the user does next. A defect that the review
found is one example. A limit that the run left in place is another.

Never invent a result. If the subagent returns no contract line, do not guess
the outcome. See the section *A run with no status*.

### The manual-test block

Every subagent report holds a `MANUAL TESTS:` block. Print that block under the
heading `Test this by hand`.

Keep the order and the text of the block. Never invent a check, and never write
one from the diff. The subagent wrote the code, so only the subagent knows which
change needs a hand check.

Print nothing for this section when the block reads `MANUAL TESTS: none`.

If a report holds no manual-test block, ask the subagent for one with
`SendMessage`.

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

### The auto-continue line

A `DONE:` report ends with one `Auto-continue:` line. Section 6 holds the rule
that builds it. Run that check before you print the report.

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

A chained turn sends two notifications. The first names the commit. The second
names the action that the chained run needs. This is correct. One run sends one
notification.

## 6. Continue to the next item

A `DONE:` return can start the next item in the same turn. No user message is
needed.

Run steps 6.1 through 6.3 before you print the report of section 3. The report
holds the decision. Start the chained run in step 6.4, after step 5 sends the
notification.

Skip this whole section for a `PAUSED:` return and for a `BLOCKED:` return.
Those returns wait on the user.

### 6.1 Read the current usage

1. Read `.claude/todo/usage-limits.json` from this thread.
2. Run `date +%s` to get the current epoch second.
3. Calculate `age` as `now - captured_at`.
4. Calculate each remaining percentage as `100 - used_percentage`.

Never reuse the numbers in the subagent report for this check. The subagent read
those numbers early in the run, and the used percentage grows during a run.

The configured status line writes this cache. It writes one time for each
refresh of the user session. An unattended run gets few refreshes, so the cache
can hold an old number. Read `age` before you trust a value.

### 6.2 Test the cache

Stop the chain when one of these is true:

- The file is missing.
- A field is missing.
- `age` is more than 900 seconds.

Never estimate a usage limit. An unknown number stops the chain.

A `now` that is later than `five_hour_resets_at` is a different case. The
five-hour window restarted, so the cached five-hour number is too high, never
too low. Treat the five-hour test as passed. Use the weekly test alone, and say
this in the report.

### 6.3 Test the budget

The subagent report holds the task-start usage and the task-end usage. Subtract
them to get the cost of the run:

```text
cost_five_hour = start_five_hour_remaining - end_five_hour_remaining
cost_weekly = start_weekly_remaining - end_weekly_remaining
```

Continue only when all four of these are true:

- `five_hour_remaining` is 20 or more.
- `weekly_remaining` is 10 or more.
- `five_hour_remaining` is `2 * cost_five_hour` or more.
- `weekly_remaining` is `2 * cost_weekly` or more.

The factor of two is deliberate. The measured cost is the only size that this
thread holds, and a next task can cost more than the last one.

Use the two floors alone when a cost is zero or less. A window that reset during
the run gives that result.

### 6.4 Start the chained run

Repeat sections 1 through 5 in this same turn. Change two things in the prompt of
section 2:

1. Set `latest_message` to `none`. No user message started this run.
2. Add these two lines to the end of the prompt.

```text
The auto-continue rule started this run. No user message started it.
The user approved no plan. Stop at the approval gate and return PAUSED.
```

The chained run researches the next item and writes `plan.md`. It then returns
`PAUSED: awaiting-approval`. The user reads that plan on the next message.

Start at most three chained runs in one turn. Stop the chain at the first
`PAUSED:` return and at the first `BLOCKED:` return.

Run this whole section again for each chained run that returns `DONE:`. Read the
cache again each time. One chained run changes the numbers.

### 6.5 Report the decision

Print one `Auto-continue:` line at the end of the completion report of the run
that just finished. Print it for every `DONE:` return, including a stopped
chain.

Use one of these shapes:

```text
Auto-continue: yes — 5h 62% left, weekly 31% left, this run cost 9% and 4%
Auto-continue: no — weekly 8% left is under the 10% floor
Auto-continue: no — the usage cache is 3 hours old
```

Name the number that decided the answer. A bare `yes` or `no` hides the
arithmetic that the user needs.

## 7. Resume the loop

A `PAUSED` return ends the turn. The next user message starts the next run. A
`DONE:` return that section 6 stopped also ends the turn.

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

Section 6 is the same check for a chained run. It asks the user nothing, because
the user already started the loop. It reports the arithmetic on the
`Auto-continue:` line instead.

This check is main-thread work. It is not a step of the task.

## Guard rails

- Do the work in the subagent. Do not do a step yourself because it looks small.
- Deciding whether to dispatch is main-thread work. That check is allowed.
- The auto-continue gate of section 6 is that same check. Read the live cache.
- A chained run never commits without an approval. It stops at the plan.
- Never commit `.claude/todo/`. It holds loop state, not project work.
- The subagent removes the temporary files. Delete no file from this thread.
- Report a leftover artifact that the user must know about. Do not remove it
  yourself.
