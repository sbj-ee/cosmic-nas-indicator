# cosmic-nas-indicator

A COSMIC panel applet that displays **NAS** as plain text in the toolbar,
colored by status:

- **grey** — not configured yet (set a host via Settings)
- **green** — the NAS share is mounted
- **orange** — the NAS is reachable but the share is not mounted
- **red** — the NAS is not reachable at all

Reachability is determined by attempting a TCP connection to the NAS host on
the SMB port (445 by default) on a fixed interval. When no mount is
configured (no `share`/`mount_point`), there is no "mounted" state, so the
color is simply green when reachable and red when not.

On first run the applet is **unconfigured** (grey). Right-click →
**Settings…** to set the host and, optionally, the share to mount.

## Interaction

- **Hover** over the label to see a tooltip with NAS details (connection
  status, host/port, resolved IP address, latency, poll interval, and any
  recent mount error).
- **Right-click** the label for a context menu that lets you:
  - **Connect / Disconnect** — mount or unmount the configured SMB share
    (only shown when `share` and `mount_point` are set).
  - **Settings…** — edit host, port, check interval, and mount options
    directly in the popup; Save persists them to the config file.
  - Increase, decrease, or reset the label font size (persisted).
  - Exit the applet.

## Requirements

- A COSMIC desktop / panel to host the applet.
- **`cifs-utils`** — required only for the Connect/Disconnect (mount) feature;
  provides the `mount.cifs` helper. On Debian/Ubuntu:
  `sudo apt install cifs-utils`. If it is missing, the applet shows a clear
  error when you try to connect.

## Build and install

```sh
cargo build --release
just install   # or: make install
```

This installs the binary to `~/.local/bin` and the applet desktop entry to
`~/.local/share/applications`.

Then add it to the panel: **Settings → Desktop → Panel → Configure panel
applets → Add applet → NAS Indicator**.

### Building a .deb package

With [`cargo-deb`](https://github.com/kornelski/cargo-deb) installed
(`cargo install cargo-deb`):

```sh
cargo deb
```

The resulting package (in `target/debian/`) installs the binary to
`/usr/bin`, the desktop entry to `/usr/share/applications`, and declares a
dependency on `cifs-utils`.

## Configuration

The easiest way to configure the applet is the **Settings…** panel in the
right-click menu. Settings are stored in
`~/.config/cosmic-nas-indicator/config` as `KEY=VALUE` lines, which you can
also edit by hand:

```
host=nas.local
port=445
interval_secs=10
font_size=14

# Optional: enable the Connect/Disconnect (mount/unmount) menu item.
share=data
mount_point=/mnt/nas
mount_options=credentials=/home/me/.smbcredentials,uid=1000,gid=1000
use_pkexec=true
```

All keys are optional; unspecified keys use built-in defaults (the host is
empty by default, i.e. unconfigured). When `font_size` is omitted, the label
uses the panel-derived default sizing. Restart the applet (or the panel)
after editing the config by hand; changes made via Settings… apply
immediately.

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
