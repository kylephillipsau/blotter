#!/bin/bash
# Build and install blotter.
#
#   ./deploy.sh pi     sync the source to the Pi, build it there, install blotterd and blotter
#                      in ~/.local/bin and (re)start the blotterd user service
#   ./deploy.sh mac    install blotter-hook in ~/.local/bin on this Mac
#
# Configs are never overwritten; see config/*.example.toml.
set -e
cd "$(dirname "$0")"

SSH_HOST="${SSH_HOST:-macclassic}"
REMOTE_DIR="${REMOTE_DIR:-blotter}"

case "$1" in
    pi)
        rsync -az --delete --exclude target --exclude .git --exclude .DS_Store ./ "$SSH_HOST:$REMOTE_DIR/"
        ssh "$SSH_HOST" "set -e
            cd $REMOTE_DIR
            \$HOME/.cargo/bin/cargo build --release -p blotterd -p blotter 2>&1 | tail -3
            target=\$(\$HOME/.cargo/bin/cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"target_directory\"])')
            mkdir -p ~/.local/bin ~/.config/systemd/user
            install -m 755 \$target/release/blotterd \$target/release/blotter ~/.local/bin/
            install -m 644 systemd/blotterd.service ~/.config/systemd/user/
            systemctl --user daemon-reload
            if [ -f ~/.config/blotter/config.toml ]; then
                systemctl --user enable blotterd.service
                systemctl --user restart blotterd.service
                echo 'blotterd restarted'
            else
                echo 'No ~/.config/blotter/config.toml yet: copy config/config.example.toml there'
            fi"
        ;;
    mac)
        cargo install --quiet --path crates/blotter-hook --root "$HOME/.local" --force
        echo "Installed $HOME/.local/bin/blotter-hook"
        [ -f "$HOME/.config/blotter/hook.toml" ] || echo "No ~/.config/blotter/hook.toml yet: copy config/hook.example.toml there"
        ;;
    *)
        echo "usage: $0 pi | mac" >&2
        exit 1
        ;;
esac
