---
name: todo-loop
description: Schedule the TODO skill every six hours for the maximum supported time. Accept true or false to control the first run. Only use this skill when requested by name from the user.
argument-hint: <true|false>
disable-model-invocation: false
allowed-tools:
  - CronCreate
  - Agent
  - PushNotification
  - SendUserFile
  - Read
  - Bash
---

# TODO Loop

Arguments: `$ARGUMENTS`

1. Accept only `true` or `false`.
2. If the input is invalid, print `Usage: /todo-loop <true|false>` and stop.
3. Create one recurring scheduled task with these values:

- Cron expression: `0 0,6,12,18 * * *`
- Prompt: `/todo`
- Recurring: `true`

4. Use the local timezone.
5. Use the default seven-day expiry.
6. If the input is `true`, run one TODO task now. Follow the section *Run one
   task now*.
7. If the input is `false`, run no task now.
8. Report the job ID and schedule.
9. Report the next three TODO items. Follow the section *Report the next
   items*.

The schedule runs near midnight, 6:00 AM, noon, and 6:00 PM. Claude adds jitter
to recurring tasks. The seven-day expiry is the maximum supported time.

The scheduled job enqueues `/todo` as a typed prompt. The user does not need to
be present for the job to fire.

## Auto-continue

A finished TODO run can start the next item in the same turn. Section 6 of
`.claude/skills/todo/SKILL.md` holds the rule and the numbers. Follow that
section. Do not write a second copy of the rule here.

The rule reads `.claude/todo/usage-limits.json` again at the decision. The
number in the run report is old by then, because the used percentage grows
during a run. A stale cache stops the chain.

A chained run stops at the plan-approval gate. It commits nothing. The user
returns to one finished commit and one plan that waits for an approval.

The completion report of each run names the decision on an `Auto-continue:`
line. It also holds a `Test this by hand` list of the checks that no automated
test covers.

## Run one task now

The `todo` skill sets `disable-model-invocation: true`, so the `Skill` tool
refuses to start it. This is deliberate. The skill commits code, and only the
user starts it.

Do not try `Skill` with `todo`. Read
`.claude/skills/todo/SKILL.md` and follow it directly instead. The user asked
for this run, so the guard does not apply.

Follow every section of that file, including the push notification.

## Report the next items

End the run with the next three TODO items. Print them under the heading
`Next 3 TODO items`.

If the TODO run returned a `NEXT:` block, print that block. It is current,
because the run wrote it after it changed `TODO.md`.

If the run returned no `NEXT:` block, build the list yourself:

1. Read `TODO.md` at the repository root.
2. Take the next three top-level `- [ ]` items in file order.
3. Print the section title, the item text, and the sub-bullet count of each
   item.

The `false` input runs no task, so this path always applies to it.

List fewer than three items when the file holds fewer. Print `TODO.md holds no
unchecked item` when the file holds none.

One item and all of its sub-bullets are one task. Count the sub-bullets. Do not
list a sub-bullet as an item.
