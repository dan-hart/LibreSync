# contrib

Service definitions and packaging helpers for embedding or running LibreSync.

## Linux: systemd user unit (`contrib/systemd/`)

Runs `libresync-alwayson-daemon` for your user, restarted on failure.

```bash
cargo install --path crates/libresync-alwayson-daemon --root ~/.local
mkdir -p ~/.config/systemd/user
cp contrib/systemd/libresync-alwayson-daemon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now libresync-alwayson-daemon.service
```

Check and follow logs:

```bash
systemctl --user status libresync-alwayson-daemon.service
journalctl --user -u libresync-alwayson-daemon.service -f
```

Keep it running after logout (otherwise user services stop with the session):

```bash
loginctl enable-linger "$USER"
```

Edit `ExecStart` in the unit if the binary lives elsewhere (`systemctl --user
edit libresync-alwayson-daemon.service` creates an override). The unit uses
`Restart=on-failure` with a 5 s delay and no start-rate limit, and confines
writes to `~/.config/libresync` and `~/.local/share/libresync`; add paths to
`ReadWritePaths` if your adapters live elsewhere.

Firewall rules for mDNS and the listener port are in
`docs/PACKAGING-LINUX.md`.

## macOS: launchd user agent (`contrib/launchd/`)

```bash
cargo build --release -p libresync-alwayson-daemon
sudo install -m 755 target/release/libresync-alwayson-daemon /usr/local/bin/
cp contrib/launchd/com.codedbydan.libresync.alwayson.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.codedbydan.libresync.alwayson.plist
launchctl kickstart -k gui/$(id -u)/com.codedbydan.libresync.alwayson
```

Stop and remove:

```bash
launchctl bootout gui/$(id -u)/com.codedbydan.libresync.alwayson
rm ~/Library/LaunchAgents/com.codedbydan.libresync.alwayson.plist
```

`KeepAlive/SuccessfulExit=false` restarts the daemon only after a failure and
`NetworkState=true` keeps it stopped while the machine is offline. Logs go to
`/tmp/libresync-alwayson.{out,err}.log`. The first launch triggers the Local
Network permission prompt; approve it or discovery stays empty. See
`docs/PACKAGING-MACOS.md` for entitlements, Bonjour and Keychain notes.

## Flatpak manifest fragment (`contrib/flatpak/`)

`com.example.LibreSyncApp.yml` shows the `finish-args` a GTK 4 app needs
(`--share=network` is mandatory) and the optional Tailscale and Secret-portal
permissions. Details in `docs/PACKAGING-LINUX.md`.
