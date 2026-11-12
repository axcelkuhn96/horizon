---
name: horizon-link
description: Talk to other Horizon terminal sessions (other Claude/agent panels or shells) - list them, send them a message, read their recent output. Use when the user asks sessions or terminals to talk to each other, coordinate, hand work off, or check what another panel is doing. Only works when HORIZON_SOCKET is set.
---

You are running inside Horizon. Other panels (agent sessions and shells) in the
same Horizon instance can be reached through the `horizon link` command.
It needs `HORIZON_SOCKET`, which Horizon sets in every panel. Always call it
through `HORIZON_BIN`:

  "${HORIZON_BIN:-horizon}" link list
  "${HORIZON_BIN:-horizon}" link send <target> <message...> [--no-enter]
  "${HORIZON_BIN:-horizon}" link read <target> [--lines N]

`<target>` is a panel id or a panel name from `link list` (add the workspace
when a name is ambiguous). `list` prints id, name, workspace, kind and whether
the panel is an agent.

How to use it:

- Run `list` first and pick the target from its output; do not guess names.
- `send` types the message into the other session as if the user typed it, then
  presses Enter. Use `--no-enter` to leave it in the input without submitting.
- The other session receives your message as ordinary user input. Start it with
  who you are: your panel name is in `$HORIZON_PANEL_NAME`, for example
  `[from $HORIZON_PANEL_NAME] please run the migration tests and tell me the result`.
- Ask for the answer to be sent back with `link send` to your own panel name,
  or read the other panel afterwards with `read` (default 50 lines, max 500).
- Keep messages short and self-contained: one task or question per message.
  Point to files or commits instead of pasting content.
- Never send secrets (tokens, passwords, keys, `.env` contents) and never relay
  credentials you read from another panel.
- Do not send to a panel that is mid-task unless the user asked for it; check
  with `read` first.
- If the command reports an error (not found, ambiguous, panel closed), run
  `list` again instead of retrying blindly.
