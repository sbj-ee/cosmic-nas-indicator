# cosmic-nas-indicator

A COSMIC panel applet that displays **NAS** as plain text in the toolbar,
colored by status:

- **green** — the NAS share is mounted
- **orange** — the NAS is reachable but the share is not mounted
- **red** — the NAS is not reachable at all

Reachability is determined by attempting a TCP connection to the NAS
(default: `SGIAB-NAS.local:445`, the SMB port) on a fixed interval. When no
mount is configured (no `share`/`mount_point`), there is no "mounted" state,
so the color is simply green when reachable and red when not.

## Interaction

- **Hover** over the label to see a tooltip with NAS details (connection
  status, host/port, resolved IP address, latency, and poll interval).
- **Right-click** the label for a context menu that lets you:
  - **Connect / Disconnect** — mount or unmount the configured SMB share
    (only shown when `share` and `mount_point` are set; see below).
  - Increase, decrease, or reset the label font size. Font size changes are
    saved to the config file and persist across restarts.
  - Exit the applet.

## Build and install

```sh
cargo build --release
just install   # or: make install
```

This installs the binary to `~/.local/bin` and the applet desktop entry to
`~/.local/share/applications`.

Then add it to the panel: **Settings → Desktop → Panel → Configure panel
applets → Add applet → NAS Indicator**.

## Configuration

Optional file at `~/.config/cosmic-nas-indicator/config` with `KEY=VALUE`
lines:

```
host=SGIAB-NAS.local
port=445
interval_secs=10
font_size=14

# Optional: enable the Connect/Disconnect (mount/unmount) menu item.
share=data
mount_point=/mnt/nas
mount_options=credentials=/home/me/.smbcredentials,uid=1000,gid=1000
use_pkexec=true
```

All keys are optional; unspecified keys use the defaults shown above.
When `font_size` is omitted, the label uses the panel-derived default
sizing. It is written automatically when you adjust the font size from the
right-click menu. Restart the applet (or the panel) after manually changing
the config.

### Mounting keys

- `share` — the SMB share to mount. Either a bare share name (expanded to
  `//<host>/<share>`) or a full source like `//host/share`.
- `mount_point` — the local directory the share is mounted onto. This
  directory must already exist.
- `mount_options` — extra options passed to `mount -o` (for example CIFS
  credentials, `uid`, `gid`, `vers`). Avoid putting a plaintext password
  here; prefer a `credentials=` file with restrictive permissions.
- `use_pkexec` — when `true` (the default), `mount`/`umount` run via
  `pkexec` so you get a graphical authentication prompt. Set to `false` if
  you have configured passwordless mounting (for example an `/etc/fstab`
  entry with the `user` option).

The Connect/Disconnect menu item only appears when both `share` and
`mount_point` are set. Mounting CIFS shares normally requires elevated
privileges, which is why `pkexec` is used by default.
