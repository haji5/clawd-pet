# Phase 0 — Hook findings

Status: **done**. The logger hooks were removed (settings restored from the backup), and the probe and logs were deleted.

Claude Code on Windows 11, 2026-11-07. Two CLI sessions and one new desktop Code tab session went through the PLAN §3 script, then the desktop app was quit and reopened. 125 valid events were logged (3 lines were corrupted by concurrent writes, see below). Scrubbed fixtures are in `tests/fixtures/hooks/`.

## Summary: what the plan got wrong

| Plan assumption | Reality | Consequence |
|---|---|---|
| `SubagentStart` marks subagent start | **Never fired**, not even for a real `general-purpose` subagent | Track subagents from events that carry `agent_id` (tool calls inside a subagent carry `agent_id` + `agent_type`) and from `PreToolUse(Agent)` |
| `SubagentStop` = a subagent finished | Also fires **after every `Stop`** for an internal helper agent with **empty `agent_type`** (its `last_assistant_message` looks like a predicted next prompt) | Ignore subagent events with empty `agent_type`. The hook now drops `a` when it's empty |
| `Notification(permission_prompt / idle_prompt)` drive attention | `idle_prompt` **never fired**, even after > 60 s idle. `permission_prompt` fires only ~6 s after an unanswered prompt (found in Phase 2) | `PermissionRequest` is the attention signal. `Stop` is the "your turn" signal. `Notification(permission_prompt)` only fills a gap |
| `Stop` fires on Esc-interrupt | **No event at all** on Esc, neither while thinking nor during a running Bash (no `Stop`, no `PostToolUseFailure`) | The staleness decay is the only way out of Thinking/Tool after an Esc. It needs a shorter timeout for Thinking |
| Denying a permission produces an event | **No event.** After a deny, the next event is the user's next `UserPromptSubmit`. `PermissionDenied` fires only for auto-mode classifier denials (no prompt was shown) | Permission attention stays up until the next event in that session. Add a way to dismiss it (click Clawd) |
| Hook < 15 ms | Our own work is ~1 ms (`PostToolUse`), but process-start → exit is 1–115 ms (avg 12.7) because **Claude Code writes stdin late** for some events (`PreToolUse` ≈ 25 ms vs `PostToolUse` ≈ 1 ms with identical work) | Doesn't matter since hooks are `async`. Measure the budget with stdin pre-filled (6.7 ms avg incl. process spawn) |

## Phase 2 live test (2026-11-07, desktop app)

- **`Notification(permission_prompt)` does fire**, about **6 s after** a permission prompt that is still unanswered, with no tool name. In Phase 0 every prompt was answered within 6 s, which is why it never showed up. It duplicates `PermissionRequest`, so the tracker only uses it when nothing is pending yet, and as the sign that a prompt is still open (no notification after 10 s means it was answered). Since the second audit (2026-11-09) the tracker draws that conclusion only for a client it has seen send the notification: the desktop app from the start, the CLI once one arrives within 10 s of its prompt. Whether the CLI sends it that soon is still not known.
- **`AskUserQuestion` goes through the permission dialog:** `PreToolUse(AskUserQuestion)` → 12 ms later `PermissionRequest(AskUserQuestion)` → `PostToolUse(AskUserQuestion)` when answered. The tracker maps a `PermissionRequest` for `AskUserQuestion`/`ExitPlanMode` to "waiting for you" (?) instead of permission (!).
- Real events reach the pet live from a session that was already running when the hooks were added.

## Event order (observed)

```
SessionStart(startup)
UserPromptSubmit
  PreToolUse(Glob) → PostToolUse(Glob)
  PreToolUse(Bash) → PostToolUseFailure(Bash, error="Exit code 2 …", is_interrupt=false)
  PreToolUse(Bash) → PermissionRequest(Bash) → [approve] PostToolUse(Bash)
                                             → [deny]    (nothing until the next UserPromptSubmit)
  PreToolUse(Agent) → … events with agent_id/agent_type from inside the subagent …
Stop
SubagentStop(agent_type="")            ← internal helper, after every Stop
UserPromptSubmit → [Esc]               (nothing)
PreCompact(trigger=manual) → SubagentStop(agent_type="") → PostCompact(trigger=manual) → SessionStart(source=compact)
SessionEnd(reason=prompt_input_exit)   ← CLI /exit;  reason=other ← CLI Ctrl+C, and desktop app quit
```

`PostCompact` and `SessionStart(compact)` arrive within the same millisecond, in either order.

## Fields seen per event (beyond the common ones)

Common: `session_id`, `transcript_path`, `cwd`, `scratchpad_dir`, `hook_event_name`. Usually also `prompt_id`, `permission_mode` (`auto` / `default`) and `effort: {level}`. Inside a subagent: `agent_id` (17 hex chars), `agent_type`.

| Event | Extra fields |
|---|---|
| `SessionStart` | `source` (`startup` / `resume` / `compact`), `model` (not always). Desktop app adds `session_title`. Resume adds `seconds_since_last_response`, `context_tokens`, `prompt_cache_likely_expired`, `estimated_cache_write_usd` |
| `UserPromptSubmit` | `prompt`. Desktop app adds `session_title` |
| `PreToolUse` | `tool_name`, `tool_input`, `tool_use_id`. MCP tools add `mcp_server: {name, source}` |
| `PostToolUse` | as above + `tool_response`, `duration_ms` |
| `PostToolUseFailure` | `tool_name`, `tool_input`, `tool_use_id`, `error` (free text), `is_interrupt`, `duration_ms` |
| `PermissionRequest` | `tool_name`, `tool_input`, `permission_suggestions` |
| `PermissionDenied` | `tool_name`, `tool_input`, `tool_use_id`, `reason` (**free text** from the classifier) |
| `Stop` | `stop_hook_active`, `last_assistant_message`, `background_tasks`, `session_crons` |
| `SubagentStop` | `agent_id`, `agent_type` (often empty), `agent_transcript_path`, `last_assistant_message`, `stop_hook_active`, `background_tasks`, `session_crons` |
| `PreCompact` | `trigger`, `custom_instructions` |
| `PostCompact` | `trigger`, `compact_summary` |
| `SessionEnd` | `reason` |

Not observed: `SubagentStart`, `Notification`, `StopFailure` (needs a real API error).

Tool names seen: `Glob`, `Read`, `Write`, `Edit`, `Bash`, `PowerShell`, `Agent`, `SubagentHandback`, `mcp__ccd_turn__update_plan`, `mcp__ccd_turn__wrap_up`, `mcp__ccd_session__hand_off_to_session`. The desktop app uses many `mcp__ccd_*` bookkeeping tools; they should map to "thinking", not to the generic gear animation, or Clawd will spin gears constantly.

## Desktop app specifics

- Quitting the app fires `SessionEnd(reason=other)` for **every** open session (5 here), all within 300 ms.
- After reopening, `SessionStart(resume)` fires for the session that is **opened** in the app, without anything being typed (re-test: it fired 30+ s before the first prompt). Other restored sessions in the sidebar get nothing until they are opened. So "launch with Claude" brings Clawd up as soon as the app opens on a session.
- **Settings edits are picked up live** by a running session: after `phase0-hooks.ps1 -Restore`, the running session stopped firing hooks immediately (its next prompt and tool calls weren't logged).

## Process and launch checks

| Check | Result |
|---|---|
| GUI subsystem (`windows_subsystem = "windows"`) reads piped stdin | ✅ (`stdin_ok: true` on all events) |
| Valid / garbage / empty / 6 MB stdin → stdout, stderr | ✅ empty, exit 0 (automated: `tests/hook_silent.rs`) |
| UDP datagram reaches the pet port | ✅ |
| Hook wall time with stdin ready (20 runs, from PowerShell) | min 6.3 / avg 6.7 / max 9.5 ms |
| `CreateProcessW` with `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP \| CREATE_BREAKAWAY_FROM_JOB`, no handle inheritance | ✅ the breakaway flag was accepted from inside both the CLI and the desktop app's hooks |
| Probe launched from a CLI `SessionStart` survives hook exit, `SessionEnd`, CLI exit, desktop app quit | ✅ 11+ minutes, parent process gone |
| Probe launched from the desktop app survives the desktop app quitting | ✅ heartbeats continued after every session's `SessionEnd` and the app restart |
| No "hook error" notice, no console flash | ✅ confirmed by the user in both clients |

**Launch method for Phase 4:** `CreateProcessW` with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB` and `bInheritHandles = FALSE` (`src/win/launch.rs`), with a retry without breakaway. The `explorer.exe` fallback isn't needed.

## Gotchas

- In PowerShell, `'...' | & gui.exe` does **not** connect stdin to a GUI-subsystem exe (and doesn't wait for it). Test with `System.Diagnostics.Process` / `std::process::Command` and piped streams, as Claude Code does.
- With `panic = "abort"`, `catch_unwind` can't catch anything, so the hook installs a panic hook that calls `std::process::exit(0)`. Panics stay silent and still exit 0.
- Concurrent hook processes appending to one log file interleaved when `writeln!` split a line into several writes. Fixed with one `write_all` per line. This only affects the `--log-raw` debug logger.
- Only enum-like detail values (`^[a-z_]{1,32}$`) are forwarded in `n`. `PermissionDenied.reason` and `PostToolUseFailure.error` are free text and never leave the hook.

## Docs reference (code.claude.com/docs/en/hooks, read 2026-11-07)

- **Exec form:** with `args`, `command` is spawned directly with no shell, and on Windows it must be a real `.exe`. `shell` is ignored then. Works as documented.
- `timeout` defaults to 600 s; we set 5.
- No matcher support on `UserPromptSubmit` and `Stop` (and `PostToolBatch`, `TeammateIdle`, `TaskCreated`, `TaskCompleted`, `Worktree*`, `MessageDisplay`, `CwdChanged`). The installer leaves `matcher` off for those.
- `StopFailure` matcher values: `rate_limit | overloaded | authentication_failed | billing_error | invalid_request | server_error | max_output_tokens | unknown | …`. The input field name is not confirmed.
- Exit 0 = success; 2 = block; anything else = a "hook error" notice.
