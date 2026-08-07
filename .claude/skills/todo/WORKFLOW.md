# TODO workflow

This file holds the full procedure. A subagent runs it. The main thread never
reads this file.

`SKILL.md` starts the subagent and passes the handoff block. Read the handoff
block before you read this section.

Do one task from start to commit. Record each stage in the progress log.

This workflow runs in a loop. One run does one task. Finish an active task
before you start a new task.

## Return contract

You cannot wait for an answer. Your run ends when you return. The main thread
prints your report, sends one push notification, and ends its turn. The next
user message starts a new run.

End every run with one of these lines as the last line of your report:

```text
PAUSED: awaiting-approval — <task>
PAUSED: question — <the question>
DONE: <commit hash> — <commit subject>
BLOCKED: <error text>
```

The main thread is the only surface that the user reads. Your report is the
message. Write the facts that the user needs, and write nothing more.

Keep the report under 20 lines. Put the substance in the report. Do not tell the
user to read a file for a fact that belongs in the report.

Put a screenshot block directly before the contract line in every report:

```text
SCREENSHOTS:
<absolute path>
<absolute path>
```

Write `SCREENSHOTS: none` when the run captured no screenshot. The main thread
shows each listed file to the user. The screenshot block does not replace the
contract line. Write both.

## 1. Resume an active task

Read `.claude/todo/state.json`.

If the file exists, a task is active. Do these steps:

1. Read `stage` and `task` from the file.
2. Read the `latest_message` field of the handoff block. It holds the answer of
   the user to your last question.
3. Go to the section for that stage. Continue there.
4. Choose no new task. Read no new item from `TODO.md`.
5. If the handoff block holds argument text, do not use it. Tell the user that
   the active task comes first. Ask the user to send the text again after this
   task.

If the file does not exist, no task is active. Go to section 2.

Before you go to section 2, run `git status --short`. If the working tree holds
changes that you did not make, stop and ask. See the section *When unsure*.

## 2. Choose the task

If the handoff block holds argument text, use that text as the task. Go to
step 7.

1. Read `TODO.md` at the repository root.
2. Take the first `## ` section. This is the topmost major element.
3. Take the first `- [ ]` item in that section.
4. Read the indented sub-bullets under the item. They hold the acceptance rules.
5. Read the prose paragraphs after the item list. They hold the rationale.
6. If the section holds no `- [ ]` item, use the next `## ` section.
7. Write `state.json` with the task, source, stage, and task-start usage.
8. Quote the chosen task in the approval summary.

## 3. Research

Follow `CLAUDE.md`. It overrides this section.

1. Read the source files that the task names.
2. Search for helpers that already do part of the work. Prefer reuse.
3. For a game mechanic, read Bulbapedia first. Use the newest-generation
   Effect, In battle, Notes, and Trivia sections.
4. Check the mechanic against other moves, abilities, items, and field effects.
5. Read the tests that cover the area. Note the helper functions they use.

## 4. Write the plan

Write the plan to `.claude/todo/plan.md`. Do not write the plan to the session
scratchpad. A loop can resume in a new session, and a scratchpad does not
survive that change.

Use these sections:

- **Context** — the problem, and the wanted outcome.
- **Approach** — the design, in prose.
- **Files** — each file to change, and the change in one line.
- **Reuse** — existing functions to call, with their paths.
- **Verification** — the exact commands, and the expected result.
- **Risks** — what could break, and the invariant that protects it.

Use the `ste-writing` skill for the plan text. Use strict mode.

Set the stage to `awaiting-approval` in `state.json`.

## 5. Get approval

1. Write a summary of the plan in 15 lines or less.
2. Put the summary in your report. Name the task, the design, and the file count.
3. Ask the user to answer with an approval, or with the changes they want.
4. Return `PAUSED: awaiting-approval`.

The main thread surfaces `plan.md` to the user. Do not paste the whole plan into
the report.

The return is the wait. The main thread ends its turn. The next user message
starts a new run with the answer of the user in the handoff block. Do not poll.
Do not sleep. Do not implement before an approval arrives.

If the `latest_message` field asks for changes, revise `plan.md`. Summarize the
change in your report. Return `PAUSED: awaiting-approval` again. Repeat until
the user approves.

When the `latest_message` field holds an approval, do these steps immediately:

1. Create `.claude/todo/progress.md` with the first log entry.
2. Set the stage to `implementing` in `state.json`.

Use this initial text:

```text
todo — <task>
Now: Starting the approved implementation
Usage: Daily/5h <remaining>% left | Weekly <remaining>% left
[5/12] Approval — plan approved <time>
```

## 6. Implement

1. Make the changes from the approved plan.
2. Keep the diff inside the approved scope. Ask before you go outside that scope.
3. Match the style of the surrounding code.
4. Use the `ste-writing` skill for every comment and document you write.
5. Update the progress log after each file that you finish.
6. Set `Now:` to the next file or action before you start it.

## 7. Verify the implementation

Set the stage to `verifying` in `state.json`.

Run these commands from `poke_rust/`:

```sh
cargo test
cargo clippy
```

Add a test for new behavior. Report a failure. Never report a pass that you did
not see. If a command fails, fix the cause, then run the command again.

Set the stage to `committing`.

## 8. Commit the review target

1. Stay on the current branch. Create no branch. Push nothing.
2. If the task came from `TODO.md`, remove the finished item from `TODO.md`.
   The file says to remove completed work.
3. Stage only the files that this task changed. Include `TODO.md` when step 2
   changed it.
4. Never stage `.claude/todo/`. It holds loop state, not project work.
5. Write a commit message in the style of the recent log. Read it with
   `git log --oneline -10`.
6. Commit.
7. Record the new commit hash in `state.json` as `review_commit`.
8. Delete `plan.md`.
9. Set the stage to `reviewing`.

The commit gives Codex an exact review target. Do not delete `state.json` yet.

## 9. Independent Codex review

Invoke `/codex:rescue` for the review. Do not call the raw Codex MCP tools
directly.

Do not give Codex the current Claude chat or a chat summary. Do not give Codex
the plan, the task text, Claude's reasoning, or expected findings. Do not tell
Codex which code Claude thinks is correct.

Give Codex only the repository and the Git history. Tell Codex to review the
commit named by `review_commit`. Codex must compare that commit with its first
parent.

Before the review, confirm that `HEAD` equals `review_commit`. Confirm that no
task file has an uncommitted change.

Use this command and neutral prompt:

```text
/codex:rescue --wait --fresh --model gpt-5.6-sol --effort high Review the
commit at HEAD independently. Use only repository files and Git history. Do
not read .claude/todo. Find anything wrong with the code introduced by HEAD,
including its interactions with existing code. Fix every problem that you find
within HEAD's scope. Add or update tests for each fix. Run the applicable Rust
and Playwright tests. Use Playwright to inspect each affected browser workflow
and capture screenshots. Do not commit.
```

A Codex job can die and still leave its record as `running`. Judge the job by
the log write time and by process liveness. Do not trust the job status field.

If the job dies, read its log file. The log can hold a complete review. Use the
findings that you can verify against the code. Run every verification yourself.

Inspect `git status --short` and `git diff` after Codex finishes. Make sure that
Codex changed only files related to the reviewed commit.

If Codex needs a follow-up, invoke `/codex:rescue` with `--wait`, `--resume`,
`--model gpt-5.6-sol`, and `--effort high`. Do not add chat context or the
deleted plan. Give only repository facts that Codex can verify.

If Codex finds a required fix outside the commit scope, ask the user first.
Update the progress log for each file that Codex changes.

Set the stage to `reverifying`.

## 10. Verify the Codex changes

Run these commands from `poke_rust/`:

```sh
cargo test
cargo clippy
cargo build --release --bin server
```

Run the applicable Playwright specs from `frontend/`:

```sh
npm exec playwright -- test <spec-files>
```

Use Playwright to inspect each affected page and interaction. Capture a
screenshot of each affected page. Inspect every screenshot before you report
success.

Record the absolute path of each screenshot. Write every path in the screenshot
block of your report. The user sees a screenshot only through that block.

If a command fails, fix the cause and run it again. Never report a pass that you
did not see.

Set the stage to `finalizing`.

## 11. Finalize the commit

1. Inspect all uncommitted Codex changes.
2. If Codex changed files, stage only those files.
3. If Codex changed files, run `git commit --amend --no-edit`.
4. Record the final commit hash.
5. Read the current usage limits.
6. Set `Now: Done` in the progress log. Add the task-end usage.
7. Delete `state.json`. The task is now closed.

Step 7 ends the task. A later loop run then starts a new task.

## 12. Report

Return a report with these facts:

- The commit hash and subject.
- The count of changed files.
- The Codex review result and any fixes that Codex made.
- The test result and the clippy result.
- The Playwright result and the screenshot paths.
- The task-start and task-end usage limits.
- Any work that you left undone.

The main thread prints this report and sends the push notification. Return
`DONE:` as the last line.

## Long jobs

A task can start a job that runs for hours. Never block on that job. Start it,
record the facts, and return.

Record these facts in `state.json`:

1. The process id.
2. The log path.
3. The measured start time. Never record an estimate.
4. The expected end time.
5. A note that tells the next run not to start a second job.

Return `PAUSED: question` with the progress and the expected end time.

A later run must check the job before it does other work. If the job still runs,
report the progress and return. Do not kill the job. Do not start a second job.

Measure cost before you scale a job. A cost distribution with a heavy tail
breaks a fixed item count. Scale such a job by wall clock instead.

## Usage limits

Read `.claude/todo/usage-limits.json`. The configured Claude Code status line
writes this cache from the live session data.

Read these fields:

- `five_hour_used_percentage`
- `seven_day_used_percentage`
- `captured_at`

Calculate each remaining percentage as `100 - used_percentage`. Round the
result to the nearest whole percent.

Label the 5-hour value as `Daily/5h`. Label the 7-day value as `Weekly`. Claude
Code does not expose a separate 24-hour value.

If the cache or a field is missing, print `unavailable` for that value. Never
estimate a usage limit. Never edit the cache.

The cache holds the usage of the main thread. A subagent shares that usage. Read
the cache in each run. Do not report the usage of the subagent alone.

Print both values at these times:

1. Before a new task starts.
2. In every progress log update.
3. After the task finishes.

Use this shape:

```text
Usage: Daily/5h 72% left | Weekly 59% left
```

Store the task-start values in `state.json`. Do not replace them when a later
run resumes an active task.

## Progress log

Path: `.claude/todo/progress.md`

Section 5 creates this file after the user approves the plan. The file is a log.
The user can read it during a long run.

1. Keep a `Now:` line directly below the task title.
2. Set `Now:` to the exact file, test, review, or action in progress.
3. Replace the `Now:` line before each new action starts.
4. Set `Now:` to `Waiting for user: <reason>` when a question blocks progress.
5. Set `Now:` to `Done` only after section 11 finishes.
6. Refresh the `Usage:` line in every update.
7. Add one log line at each later stage boundary.
8. Keep every earlier log line. The file grows into a log.

The progress log must always show the current work until the task ends. Do not
leave a completed action in the `Now:` line.

Use this shape:

```text
todo — Split nature_spread_coherence into two controls
Now: Running tracker-input.spec.ts with Playwright
Usage: Daily/5h 68% left | Weekly 57% left
[5/12] Approval — plan approved 12:15
[6/12] Implement — 2 files of 4 done
[7/12] Verify — tests and clippy passed
[8/12] Commit — created the review target
[9/12] Codex review — fixed one edge case
[10/12] Verify fixes — Playwright passed
[11/12] Finalize — amended the task commit
```

A progress log update sends no notification. Only the main thread notifies, and
it notifies one time for each run. This is why a progress update never needs a
message to the user.

## When unsure

Ask the user. Do not guess. Return `PAUSED: question` with the question text in
the report.

Ask when any of these conditions is true:

- The task text allows two different designs, and the cost differs.
- `TODO.md` names a file or a function that does not exist.
- Research contradicts the task text.
- `state.json` names a stage that does not match the working tree.
- The working tree holds changes that this workflow did not make.
- A fix needs a change outside the approved plan.
- A test fails for a reason that the plan did not predict.
- `TODO.md` holds no unchecked item.

State the options in the question. Give a recommendation. Wait for the answer in
the `latest_message` field of the next run.

## Stale answers

The `latest_message` field can hold an answer to a question that a past run
already closed. Check the answer against `state.json`.

If no task is active, the answer is stale. It belonged to the finished task. Say
this in the report. Then choose the next task.

Never treat a stale answer as an approval for a new plan.

## Failure rule

If any stage fails, return `BLOCKED:` with the error text. Keep `state.json` in
place, so the next run resumes at the failed stage. Leave the repository in a
state that the user can inspect. Never commit a broken build.

## State file

Path: `.claude/todo/state.json`

```json
{
  "task": "Split nature_spread_coherence into two controls",
  "source": "todo",
  "stage": "awaiting-approval",
  "review_commit": null,
  "usage_before": {
    "daily_five_hour_remaining": 72,
    "weekly_remaining": 59
  },
  "branch": "main",
  "started": "2026-07-29T12:00:00Z"
}
```

Valid stages: `researching`, `awaiting-approval`, `implementing`, `verifying`,
`committing`, `reviewing`, `reverifying`, `finalizing`.

Write the file after every stage change. The next run reads it.

This file is the only memory between runs. A new subagent starts with no
conversation history. Record every fact that the next run needs.

## Tool notes

- `/codex:rescue --fresh` starts a new Codex task without an old Codex thread.
- `/codex:rescue --resume` continues only the current task's Codex thread.
- The main thread owns the push notification. Send none from this workflow.
