# Meetily Deployment

This directory contains deployment helpers for running Meetily outside the desktop development flow.

## Model Download CLI

Use `scripts/download-model.sh` from the repository root:

```bash
./scripts/download-model.sh
./scripts/download-model.sh large-v3-turbo
```

The default model is `large-v3-turbo`. The script expects the release binary at `target/release/meetily-client` and exits if it has not been built.

Build the binary first:

```bash
cargo build --release --bin meetily-client
```

If the binary lives elsewhere, set `MEETILY_CLIENT_BIN`:

```bash
MEETILY_CLIENT_BIN=/path/to/meetily-client ./scripts/download-model.sh large-v3-turbo
```

## Linux User Service

The user systemd unit is `meetily-server.service`. It runs the FastAPI server from:

```text
%h/git/meetily/backend
```

Install and start it:

```bash
mkdir -p ~/.config/systemd/user ~/.config/meetily
cp deploy/meetily-server.service ~/.config/systemd/user/
cp deploy/server.env.example ~/.config/meetily/server.env
$EDITOR ~/.config/meetily/server.env
systemctl --user daemon-reload
systemctl --user enable --now meetily-server.service
```

View logs and status:

```bash
systemctl --user status meetily-server.service
journalctl --user -u meetily-server.service -f
```

The service uses:

```text
EnvironmentFile=%h/.config/meetily/server.env
ExecStart=/usr/bin/env uvicorn app.main:app --host 0.0.0.0 --port 5167
Restart=on-failure
RestartSec=5
```

Make sure `uvicorn` and the backend Python dependencies are available in the user service environment.

## macOS LaunchAgent

The LaunchAgent plist is `com.meetily.client.plist`. It starts:

```text
~/git/meetily/target/release/meetily-client record
```

Install it:

```bash
./deploy/install-macos.sh
```

The installer writes the plist to:

```text
~/Library/LaunchAgents/com.meetily.client.plist
```

Logs go to:

```text
~/Library/Logs/meetily-client.log
```

Useful commands:

```bash
launchctl kickstart gui/$(id -u)/com.meetily.client
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.meetily.client.plist
launchctl print gui/$(id -u)/com.meetily.client
```
