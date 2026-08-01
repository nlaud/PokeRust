---
name: discord-todo-loop
description: Schedule the Discord TODO skill every six hours for the maximum supported time. Accept true or false to control the first run. Only use this skill when requested by name from the user.
argument-hint: <true|false>
disable-model-invocation: false
allowed-tools:
  - CronCreate
  - Skill
---

# Discord TODO Loop

Arguments: `$ARGUMENTS`

1. Accept only `true` or `false`.
2. If the input is invalid, print `Usage: /discord-todo-loop <true|false>` and stop.
3. Create one recurring scheduled task with these values:

- Cron expression: `0 0,6,12,18 * * *`
- Prompt: `/discord-todo`
- Recurring: `true`

4. Use the local timezone.
5. Use the default seven-day expiry.
6. If the input is `true`, invoke `/discord-todo` once after you create the task.
7. If the input is `false`, do not invoke `/discord-todo` immediately.
8. Report the job ID and schedule.

The schedule runs near midnight, 6:00 AM, noon, and 6:00 PM. Claude adds jitter
to recurring tasks. The seven-day expiry is the maximum supported time.
