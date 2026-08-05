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

The schedule runs near midnight, 6:00 AM, noon, and 6:00 PM. Claude adds jitter
to recurring tasks. The seven-day expiry is the maximum supported time.

The scheduled job enqueues `/todo` as a typed prompt. The user does not need to
be present for the job to fire.

## Run one task now

The `todo` skill sets `disable-model-invocation: true`, so the `Skill` tool
refuses to start it. This is deliberate. The skill commits code, and only the
user starts it.

Do not try `Skill` with `todo`. Read
`.claude/skills/todo/SKILL.md` and follow it directly instead. The user asked
for this run, so the guard does not apply.

Follow every section of that file, including the push notification.
