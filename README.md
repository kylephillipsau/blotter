# blotter

The work screen for the Macintosh Classic II e-ink display: a live board of Claude Code
sessions, kept by a small daemon on the Classic's Pi and fed by hooks on the Mac. The idle
screen is [epaper-dashboard](https://github.com/kylephillipsau/epaper-dashboard); the design,
including what comes next (mode switching, Mac actions, a reference reader), is in the
Obsidian note *Blotter*.

```
MacBook                                       Classic (Pi 4)
Claude Code hooks ──► blotter-hook ──HTTP──►  blotterd ◄── blotter (in foot, sway workspace 2)
                      (over Tailscale)        board.json
```

| Crate | Runs on | What it does |
| --- | --- | --- |
| `blotter-hook` | Mac | Async hook for every event: keeps the session, state, tool and file (never file contents), adds the tmux pane, posts to blotterd. Always exits 0. |
| `blotterd` | Pi | Applies events to the board, saves it, serves it. `GET /state?after=N` waits for the next change, so clients don't poll. |
| `blotter` | Pi | The ratatui screen: sessions needing you first, then working, then idle. No colour or animation, for e-ink. |
| `blotter-proto` | both | The event and board types, the session state machine (the same one sofle-status uses) and the config. |

## Setup

```bash
./deploy.sh pi      # build on the Pi, install blotterd + blotter, start the user service
./deploy.sh mac     # install blotter-hook in ~/.local/bin
```

Configs (never overwritten by deploy):
- Pi: `~/.config/blotter/config.toml`, from `config/config.example.toml`
- Mac: `~/.config/blotter/hook.toml`, from `config/hook.example.toml`

Both need the same token (`openssl rand -hex 32`). Then add the hook to
`~/.claude/settings.json` for `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
`PreToolUse`, `PostToolUse`, `PermissionRequest`, `Notification`, `Stop` and `StopFailure`:

```json
{ "hooks": [{ "type": "command", "command": "/Users/you/.local/bin/blotter-hook", "async": true }] }
```

Open the screen on the Classic with `foot -a blotter blotter`.

## Checking it

```bash
curl http://100.81.63.71:7340/healthz                       # ok
ssh macclassic journalctl --user -u blotterd -f             # daemon log
ssh macclassic cat ~/.local/state/blotter/board.json        # the board
cargo test                                                  # state machine and hook filtering
```

Async hooks can arrive out of order. Every event carries the time the hook ran, and state only
moves forward in time, so a late `PostToolUse` can't make a finished session look busy again.
