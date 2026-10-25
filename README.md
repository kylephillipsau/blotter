# blotter

A board of Claude Code sessions for an e-ink screen.

Hooks on the Mac report every session to a daemon on a Raspberry Pi. A terminal UI on the Pi
lists them: what needs you, what's working, what's idle. After 15 minutes without a key the
screen goes back to a dashboard; the next key brings the board back.

Built for one desk: a Macintosh Classic II with a 9.7" e-ink panel, driven by
[paper-tty](https://github.com/kylephillipsau/paper-tty-rs). The dashboard is
[epaper-dashboard](https://github.com/kylephillipsau/epaper-dashboard).

## Parts

- `blotter-hook`: async Claude Code hook, on the Mac. Sends state, tool and file, never file contents.
- `blotterd`: the board, on the Pi. Tailscale only, token auth, long-poll.
- `blotter`: the screen, on the Pi. ratatui, no colour, no animation.
- `sway/`: swayidle glue that switches between the dashboard and the board.

## Use

```sh
./deploy.sh pi     # build on the Pi, restart blotterd and the screen
./deploy.sh mac    # install blotter-hook
```

Configs start from `config/*.example.toml`. In `~/.claude/settings.json`, add the hook
`async` to SessionStart, SessionEnd, UserPromptSubmit, PreToolUse, PostToolUse,
PermissionRequest, Notification, Stop and StopFailure:

```json
{ "hooks": [{ "type": "command", "command": "~/.local/bin/blotter-hook", "async": true }] }
```

## Licence

MIT
