use std::net::SocketAddr;
use std::time::{Duration, Instant};

use cosmic::app::{Core, Task};
use cosmic::iced::window::Id;
use cosmic::iced::{time, Alignment, Color, Length, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::{Application, Element};

/// Fallback defaults, overridable via ~/.config/cosmic-nas-indicator/config or
/// the in-applet settings panel. The host is intentionally empty so a fresh
/// install starts in an "unconfigured" state rather than probing a stranger's
/// hostname.
const DEFAULT_HOST: &str = "";
const DEFAULT_PORT: u16 = 445;
const DEFAULT_INTERVAL_SECS: u64 = 10;
const CONNECT_TIMEOUT_SECS: u64 = 3;

/// Font-size handling for the panel label when the user overrides sizing via
/// the right-click menu. `DEFAULT_FONT_SIZE` is the starting point when no
/// explicit size has been chosen yet. The size can only be nudged
/// `FONT_STEPS` increments of `FONT_STEP` in either direction from the default.
const DEFAULT_FONT_SIZE: f32 = 14.0;
const FONT_STEP: f32 = 2.0;
const FONT_STEPS: f32 = 2.0;
const MIN_FONT_SIZE: f32 = DEFAULT_FONT_SIZE - FONT_STEP * FONT_STEPS;
const MAX_FONT_SIZE: f32 = DEFAULT_FONT_SIZE + FONT_STEP * FONT_STEPS;

const GREEN: Color = Color::from_rgb(0.18, 0.80, 0.25);
const ORANGE: Color = Color::from_rgb(1.0, 0.58, 0.05);
const RED: Color = Color::from_rgb(0.90, 0.16, 0.16);
/// Neutral color used before the applet has been configured.
const GREY: Color = Color::from_rgb(0.6, 0.6, 0.6);

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<NasIndicator>(())
}

struct Config {
    host: String,
    port: u16,
    interval_secs: u64,
    /// Explicit panel label font size. `None` means "use the panel-derived
    /// default sizing from libcosmic".
    font_size: Option<f32>,
    /// SMB share to mount. Either a full source like `//host/share` (or a
    /// UNC-style `\\host\share`) or a bare share name, in which case the
    /// source becomes `//<host>/<share>`.
    share: Option<String>,
    /// Local directory the share is mounted onto.
    mount_point: Option<String>,
    /// Extra options passed to `mount -o` (e.g. `credentials=/path,uid=1000`).
    mount_options: Option<String>,
    /// Run mount/umount through `pkexec` so a normal user gets a polkit auth
    /// prompt. Set to `false` if you have an fstab `user` entry instead.
    use_pkexec: bool,
}

impl Config {
    /// Reads KEY=VALUE lines from ~/.config/cosmic-nas-indicator/config.
    /// Recognized keys: host, port, interval_secs, font_size. Missing file or
    /// keys fall back to the compiled-in defaults.
    fn load() -> Self {
        let mut config = Config {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            interval_secs: DEFAULT_INTERVAL_SECS,
            font_size: None,
            share: None,
            mount_point: None,
            mount_options: None,
            use_pkexec: true,
        };
        let path = dirs_config_path();
        if let Ok(contents) = std::fs::read_to_string(path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let (key, value) = (key.trim(), value.trim());
                    match key {
                        "host" => config.host = value.to_string(),
                        "port" => {
                            if let Ok(port) = value.parse() {
                                config.port = port;
                            }
                        }
                        "interval_secs" => {
                            if let Ok(secs) = value.parse::<u64>() {
                                config.interval_secs = secs.max(2);
                            }
                        }
                        "font_size" => {
                            if let Ok(size) = value.parse::<f32>() {
                                config.font_size =
                                    Some(size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
                            }
                        }
                        "share" => config.share = non_empty(value),
                        "mount_point" => config.mount_point = non_empty(value),
                        "mount_options" => config.mount_options = non_empty(value),
                        "use_pkexec" => {
                            if let Ok(flag) = value.parse::<bool>() {
                                config.use_pkexec = flag;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        config
    }

    /// Whether the applet has enough configuration to do anything useful (a
    /// non-empty host to probe).
    fn is_configured(&self) -> bool {
        !self.host.trim().is_empty()
    }

    /// Persists the full configuration back to the config file, updating known
    /// keys in place while preserving comments and any unrecognized lines.
    fn save(&self) {
        let path = dirs_config_path();
        let existing = std::fs::read_to_string(&path).unwrap_or_default();

        // Desired value for each known key. `None` means the key should be
        // absent from the file entirely.
        let desired: [(&str, Option<String>); 8] = [
            ("host", non_empty(self.host.trim()).map(|s| s.to_string())),
            ("port", Some(self.port.to_string())),
            ("interval_secs", Some(self.interval_secs.to_string())),
            ("font_size", self.font_size.map(|s| s.to_string())),
            ("share", self.share.clone()),
            ("mount_point", self.mount_point.clone()),
            ("mount_options", self.mount_options.clone()),
            ("use_pkexec", Some(self.use_pkexec.to_string())),
        ];

        let mut seen: Vec<&str> = Vec::new();
        let mut out: Vec<String> = Vec::new();
        for line in existing.lines() {
            if let Some((key, _)) = line.split_once('=') {
                let key = key.trim();
                if let Some((k, val)) = desired.iter().find(|(dk, _)| *dk == key) {
                    seen.push(*k);
                    if let Some(v) = val {
                        out.push(format!("{k} = {v}"));
                    }
                    continue;
                }
            }
            out.push(line.to_string());
        }
        for (k, val) in &desired {
            if !seen.contains(k) {
                if let Some(v) = val {
                    out.push(format!("{k} = {v}"));
                }
            }
        }

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, out.join("\n") + "\n");
    }

    /// The `mount -t cifs` source for the configured share, if any. A bare
    /// share name is expanded to `//<host>/<share>`; a value already starting
    /// with `//` or `\\` is used as-is (with backslashes normalized).
    fn mount_source(&self) -> Option<String> {
        let share = self.share.as_deref()?;
        if share.starts_with("//") || share.starts_with(r"\\") {
            Some(share.replace('\\', "/"))
        } else {
            Some(format!("//{}/{}", self.host, share.trim_start_matches('/')))
        }
    }

    /// Whether both a share and a mount point are configured, so the
    /// connect/disconnect menu item is meaningful.
    fn is_mountable(&self) -> bool {
        self.mount_source().is_some() && self.mount_point.is_some()
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Whether the CIFS mount helper (`mount.cifs` from the `cifs-utils` package)
/// is installed. Without it, `mount -t cifs` fails with an opaque error.
fn mount_cifs_available() -> bool {
    ["/sbin/mount.cifs", "/usr/sbin/mount.cifs", "/bin/mount.cifs"]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
}

/// Runs a prepared mount/umount command, returning `None` on success or a
/// short human-readable error message on failure.
async fn run_mount_command(mut cmd: tokio::process::Command, verb: &str) -> Option<String> {
    match cmd.output().await {
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg = stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("command failed");
            Some(format!("{verb} failed: {msg}"))
        }
        Err(e) => Some(format!("could not run {verb}: {e}")),
    }
}

/// Returns true if `mount_point` currently appears as a mount target in
/// /proc/mounts.
fn is_mounted(mount_point: &str) -> bool {
    // /proc/mounts encodes spaces as \040; normalize before comparing.
    let target = mount_point.trim_end_matches('/');
    std::fs::read_to_string("/proc/mounts")
        .map(|contents| {
            contents.lines().any(|line| {
                line.split_whitespace()
                    .nth(1)
                    .map(|m| m.replace("\\040", " ").trim_end_matches('/') == target)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn dirs_config_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    base.join("cosmic-nas-indicator").join("config")
}

/// The most recent probe result, used to color the text and fill the tooltip.
#[derive(Debug, Clone, Default)]
struct Status {
    connected: bool,
    /// Resolved peer address on a successful connect (gives the NAS IP).
    addr: Option<SocketAddr>,
    /// Round-trip time of the successful TCP connect.
    latency: Option<Duration>,
    /// Whether the configured mount point is currently mounted.
    mounted: bool,
}

async fn check_nas(host: String, port: u16, mount_point: Option<String>) -> Status {
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await;
    let mounted = mount_point.as_deref().map(is_mounted).unwrap_or(false);
    match result {
        Ok(Ok(stream)) => Status {
            connected: true,
            addr: stream.peer_addr().ok(),
            latency: Some(start.elapsed()),
            mounted,
        },
        _ => Status {
            mounted,
            ..Status::default()
        },
    }
}

/// Editable string buffers backing the in-applet settings form.
#[derive(Debug, Clone, Default)]
struct SettingsForm {
    host: String,
    port: String,
    interval_secs: String,
    share: String,
    mount_point: String,
    mount_options: String,
    use_pkexec: bool,
}

impl SettingsForm {
    fn from_config(config: &Config) -> Self {
        SettingsForm {
            host: config.host.clone(),
            port: config.port.to_string(),
            interval_secs: config.interval_secs.to_string(),
            share: config.share.clone().unwrap_or_default(),
            mount_point: config.mount_point.clone().unwrap_or_default(),
            mount_options: config.mount_options.clone().unwrap_or_default(),
            use_pkexec: config.use_pkexec,
        }
    }
}

struct NasIndicator {
    core: Core,
    config: Config,
    status: Status,
    /// The currently open right-click menu popup, if any.
    popup: Option<Id>,
    /// When true, the popup shows the settings form instead of the menu.
    editing: bool,
    /// Buffers for the settings form while editing.
    form: SettingsForm,
    /// Last mount/unmount error to surface to the user, if any.
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Status(Status),
    /// Surface actions emitted by the hover tooltip and right-click menu
    /// (create/destroy popup surfaces).
    Surface(cosmic::surface::Action),
    /// The popup window was closed (by clicking away or losing focus).
    PopupClosed(Id),
    /// Adjust the label font size by the given delta (from the menu).
    FontDelta(f32),
    /// Reset the label font size back to the panel default.
    FontReset,
    /// Mount the configured SMB share.
    Connect,
    /// Unmount the configured SMB share.
    Disconnect,
    /// Result of a mount/unmount attempt: `None` on success, `Some(err)` on
    /// failure.
    MountDone(Option<String>),
    /// Open the settings form in the popup.
    OpenSettings,
    /// Discard edits and return to the menu.
    CancelSettings,
    /// Persist the settings form and return to the menu.
    SaveSettings,
    /// Edits to individual settings-form fields.
    EditHost(String),
    EditPort(String),
    EditInterval(String),
    EditShare(String),
    EditMountPoint(String),
    EditMountOptions(String),
    EditPkexec(bool),
    /// Quit the applet.
    Exit,
}

impl NasIndicator {
    fn check_task(&self) -> Task<Message> {
        if !self.config.is_configured() {
            return Task::none();
        }
        let host = self.config.host.clone();
        let port = self.config.port;
        let mount_point = self.config.mount_point.clone();
        cosmic::task::future(
            async move { Message::Status(check_nas(host, port, mount_point).await) },
        )
    }

    /// Runs `mount` (via pkexec when configured) for the configured share, then
    /// re-checks status. Reports failures via `Message::MountDone`.
    fn mount_task(&self) -> Task<Message> {
        let (Some(source), Some(mount_point)) =
            (self.config.mount_source(), self.config.mount_point.clone())
        else {
            return Task::none();
        };
        if !mount_cifs_available() {
            return cosmic::task::future(async {
                Message::MountDone(Some(
                    "cifs-utils not installed (mount.cifs missing)".to_string(),
                ))
            });
        }
        let options = self.config.mount_options.clone();
        let use_pkexec = self.config.use_pkexec;
        cosmic::task::future(async move {
            let mut cmd = if use_pkexec {
                let mut c = tokio::process::Command::new("pkexec");
                c.arg("mount");
                c
            } else {
                tokio::process::Command::new("mount")
            };
            cmd.arg("-t").arg("cifs").arg(&source).arg(&mount_point);
            if let Some(opts) = options {
                cmd.arg("-o").arg(opts);
            }
            Message::MountDone(run_mount_command(cmd, "mount").await)
        })
    }

    /// Runs `umount` (via pkexec when configured) for the configured mount
    /// point, then re-checks status. Reports failures via `Message::MountDone`.
    fn unmount_task(&self) -> Task<Message> {
        let Some(mount_point) = self.config.mount_point.clone() else {
            return Task::none();
        };
        let use_pkexec = self.config.use_pkexec;
        cosmic::task::future(async move {
            let mut cmd = if use_pkexec {
                let mut c = tokio::process::Command::new("pkexec");
                c.arg("umount");
                c
            } else {
                tokio::process::Command::new("umount")
            };
            cmd.arg(&mount_point);
            Message::MountDone(run_mount_command(cmd, "umount").await)
        })
    }

    /// Parses the settings form into the config and persists it.
    fn apply_settings(&mut self) {
        self.config.host = self.form.host.trim().to_string();
        if let Ok(port) = self.form.port.trim().parse::<u16>() {
            if port != 0 {
                self.config.port = port;
            }
        }
        if let Ok(secs) = self.form.interval_secs.trim().parse::<u64>() {
            self.config.interval_secs = secs.max(2);
        }
        self.config.share = non_empty(self.form.share.trim());
        self.config.mount_point = non_empty(self.form.mount_point.trim());
        self.config.mount_options = non_empty(self.form.mount_options.trim());
        self.config.use_pkexec = self.form.use_pkexec;
        self.config.save();
    }

    /// Task that destroys the context-menu popup if it is currently open.
    fn close_popup_task(&mut self) -> Task<Message> {
        if let Some(id) = self.popup.take() {
            cosmic::iced::Task::done(cosmic::Action::Cosmic(cosmic::app::Action::Surface(
                destroy_popup(id),
            )))
        } else {
            Task::none()
        }
    }

    /// One-line status summary that matches the label color: unconfigured
    /// (grey), mounted (green), reachable-but-not-mounted (orange), or
    /// unreachable (red).
    fn status_summary(&self) -> &'static str {
        if !self.config.is_configured() {
            "NAS — Not configured"
        } else if !self.status.connected {
            "NAS — Unreachable"
        } else if !self.config.is_mountable() {
            "NAS — Connected"
        } else if self.status.mounted {
            "NAS — Mounted"
        } else {
            "NAS — Reachable, not mounted"
        }
    }

    /// Multi-line text shown in the hover tooltip.
    fn tooltip_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push(self.status_summary().to_string());

        if !self.config.is_configured() {
            lines.push("Right-click → Settings to configure".to_string());
            return lines.join("\n");
        }

        let host = &self.config.host;
        let port = self.config.port;
        lines.push(format!("Host: {host}:{port}"));

        if let Some(addr) = self.status.addr {
            lines.push(format!("Address: {}", addr.ip()));
        }
        if let Some(latency) = self.status.latency {
            lines.push(format!("Latency: {:.1} ms", latency.as_secs_f64() * 1000.0));
        } else if !self.status.connected {
            lines.push(format!("No response on port {port}"));
        }

        if let Some(mount_point) = &self.config.mount_point {
            if self.config.is_mountable() {
                lines.push(if self.status.mounted {
                    format!("Mounted at {mount_point}")
                } else {
                    "Not mounted".to_string()
                });
            }
        }

        lines.push(format!("Checked every {}s", self.config.interval_secs));

        if let Some(err) = &self.last_error {
            lines.push(format!("⚠ {err}"));
        }

        lines.join("\n")
    }

    /// The effective font size currently applied to the panel label.
    fn current_font_size(&self) -> f32 {
        self.config.font_size.unwrap_or(DEFAULT_FONT_SIZE)
    }

    /// The popup body, dispatching between the settings form and the menu.
    fn popup_body(&self) -> Element<'_, Message> {
        if self.editing {
            self.settings_view()
        } else {
            self.menu_view()
        }
    }

    /// Contents of the right-click context menu popup.
    fn menu_view(&self) -> Element<'_, Message> {
        use cosmic::applet::{menu_button, padded_control};
        use cosmic::widget::{divider, text, Column};

        let header = self.status_summary();

        let font_label = format!("Font size: {:.0}", self.current_font_size());

        let mut menu = Column::new()
            .padding([8, 0])
            .push(padded_control(text::heading(header)));

        if self.config.is_configured() {
            menu = menu.push(padded_control(text::caption(format!(
                "{}:{}",
                self.config.host, self.config.port
            ))));
        }

        if let Some(err) = &self.last_error {
            menu = menu.push(padded_control(
                text::caption(format!("⚠ {err}")).class(cosmic::style::Text::Color(ORANGE)),
            ));
        }

        // Connect / disconnect (mount / unmount) — only when a share and mount
        // point are configured.
        if self.config.is_mountable() {
            menu = menu.push(padded_control(divider::horizontal::default()));
            menu = if self.status.mounted {
                menu.push(menu_button(text::body("Disconnect (unmount)")).on_press(Message::Disconnect))
            } else {
                menu.push(menu_button(text::body("Connect (mount)")).on_press(Message::Connect))
            };
        }

        let current = self.current_font_size();
        let can_increase = current < MAX_FONT_SIZE;
        let can_decrease = current > MIN_FONT_SIZE;

        menu.push(padded_control(divider::horizontal::default()))
            .push(menu_button(text::body("Settings…")).on_press(Message::OpenSettings))
            .push(padded_control(divider::horizontal::default()))
            .push(padded_control(text::caption(font_label)))
            .push(
                menu_button(text::body("Increase font size"))
                    .on_press_maybe(can_increase.then_some(Message::FontDelta(FONT_STEP))),
            )
            .push(
                menu_button(text::body("Decrease font size"))
                    .on_press_maybe(can_decrease.then_some(Message::FontDelta(-FONT_STEP))),
            )
            .push(menu_button(text::body("Reset font size")).on_press(Message::FontReset))
            .push(padded_control(divider::horizontal::default()))
            .push(menu_button(text::body("Exit")).on_press(Message::Exit))
            .into()
    }

    /// The settings form shown in the popup.
    fn settings_view(&self) -> Element<'_, Message> {
        use cosmic::applet::{menu_button, padded_control};
        use cosmic::widget::{divider, text, text_input, toggler, Column};

        let field = |label: &str, placeholder: &str, value: &str, on_input: fn(String) -> Message| {
            Column::new()
                .spacing(2)
                .push(text::caption(label.to_string()))
                .push(text_input(placeholder.to_string(), value.to_string()).on_input(on_input))
        };

        Column::new()
            .padding([8, 0])
            .spacing(4)
            .push(padded_control(text::heading("NAS Settings")))
            .push(padded_control(field(
                "Host",
                "e.g. nas.local or 192.168.1.10",
                &self.form.host,
                Message::EditHost,
            )))
            .push(padded_control(field(
                "Port",
                "445",
                &self.form.port,
                Message::EditPort,
            )))
            .push(padded_control(field(
                "Check interval (seconds)",
                "10",
                &self.form.interval_secs,
                Message::EditInterval,
            )))
            .push(padded_control(divider::horizontal::default()))
            .push(padded_control(text::caption(
                "Mounting (optional)".to_string(),
            )))
            .push(padded_control(field(
                "Share",
                "share name or //host/share",
                &self.form.share,
                Message::EditShare,
            )))
            .push(padded_control(field(
                "Mount point",
                "/mnt/nas",
                &self.form.mount_point,
                Message::EditMountPoint,
            )))
            .push(padded_control(field(
                "Mount options",
                "credentials=/path,uid=1000",
                &self.form.mount_options,
                Message::EditMountOptions,
            )))
            .push(padded_control(
                toggler(self.form.use_pkexec)
                    .label("Use pkexec (auth prompt)".to_string())
                    .on_toggle(Message::EditPkexec),
            ))
            .push(padded_control(divider::horizontal::default()))
            .push(menu_button(text::body("Save")).on_press(Message::SaveSettings))
            .push(menu_button(text::body("Cancel")).on_press(Message::CancelSettings))
            .into()
    }
}

impl Application for NasIndicator {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "io.github.sbj_ee.CosmicNasIndicator";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        let config = Config::load();
        let app = NasIndicator {
            core,
            config,
            status: Status::default(),
            popup: None,
            editing: false,
            form: SettingsForm::default(),
            last_error: None,
        };
        let initial_check = app.check_task();
        (app, initial_check)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => self.check_task(),
            Message::Status(status) => {
                self.status = status;
                Task::none()
            }
            Message::Surface(action) => cosmic::iced::Task::done(cosmic::Action::Cosmic(
                cosmic::app::Action::Surface(action),
            )),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.editing = false;
                }
                Task::none()
            }
            Message::FontDelta(delta) => {
                let new_size =
                    (self.current_font_size() + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                self.config.font_size = Some(new_size);
                self.config.save();
                Task::none()
            }
            Message::FontReset => {
                self.config.font_size = None;
                self.config.save();
                Task::none()
            }
            Message::Connect => {
                self.last_error = None;
                let close = self.close_popup_task();
                Task::batch([close, self.mount_task()])
            }
            Message::Disconnect => {
                self.last_error = None;
                let close = self.close_popup_task();
                Task::batch([close, self.unmount_task()])
            }
            Message::MountDone(result) => {
                self.last_error = result;
                self.check_task()
            }
            Message::OpenSettings => {
                self.form = SettingsForm::from_config(&self.config);
                self.editing = true;
                Task::none()
            }
            Message::CancelSettings => {
                self.editing = false;
                Task::none()
            }
            Message::SaveSettings => {
                self.apply_settings();
                self.editing = false;
                let close = self.close_popup_task();
                Task::batch([close, self.check_task()])
            }
            Message::EditHost(v) => {
                self.form.host = v;
                Task::none()
            }
            Message::EditPort(v) => {
                self.form.port = v;
                Task::none()
            }
            Message::EditInterval(v) => {
                self.form.interval_secs = v;
                Task::none()
            }
            Message::EditShare(v) => {
                self.form.share = v;
                Task::none()
            }
            Message::EditMountPoint(v) => {
                self.form.mount_point = v;
                Task::none()
            }
            Message::EditMountOptions(v) => {
                self.form.mount_options = v;
                Task::none()
            }
            Message::EditPkexec(v) => {
                self.form.use_pkexec = v;
                Task::none()
            }
            Message::Exit => {
                std::process::exit(0);
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        time::every(Duration::from_secs(self.config.interval_secs)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Message> {
        // Grey before configuration, red when unreachable, green when the
        // share is mounted, orange when reachable but not mounted. With no
        // mount configured there is no "mounted" state, so reachability alone
        // toggles green/red.
        let color = if !self.config.is_configured() {
            GREY
        } else if !self.status.connected {
            RED
        } else if !self.config.is_mountable() || self.status.mounted {
            GREEN
        } else {
            ORANGE
        };
        let tooltip = self.tooltip_text();
        let parent_id = self.core.main_window_id();
        let applet = &self.core.applet;

        // Center the text within the panel's suggested applet extent, the
        // same way libcosmic's applet text_button helper does.
        let suggested = applet.suggested_size(true);
        let (padding_major, padding_minor) = applet.suggested_padding(true);
        let (horizontal_padding, vertical_padding) = if applet.is_horizontal() {
            (padding_major, padding_minor)
        } else {
            (padding_minor, padding_major)
        };

        // Use the panel-derived default sizing unless the user has picked an
        // explicit font size via the right-click menu.
        let mut label = applet
            .text("NAS")
            .class(cosmic::style::Text::Color(color))
            .height(Length::Fill)
            .align_y(Alignment::Center);
        if let Some(size) = self.config.font_size {
            label = label.size(size);
        }

        let content = cosmic::widget::container(label)
            .center_y(Length::Fixed(f32::from(suggested.1 + 2 * vertical_padding)))
            .padding([0, horizontal_padding]);

        // Wayland-native hover tooltip: opens a small popup surface next to the
        // panel with the NAS details. Suppressed while the right-click menu
        // popup is open so the two don't fight over the surface.
        let tooltipped =
            applet.applet_tooltip(content, tooltip, self.popup.is_some(), Message::Surface, parent_id);

        // Right-click toggles a context menu popup anchored to the applet.
        let right_click = if let Some(id) = self.popup {
            Message::Surface(destroy_popup(id))
        } else {
            Message::Surface(app_popup::<NasIndicator>(
                |_| Default::default(),
                move |state: &mut NasIndicator| {
                    let new_id = Id::unique();
                    state.popup = Some(new_id);
                    state.core.applet.get_popup_settings(
                        state.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    )
                },
                Some(Box::new(|state: &NasIndicator| {
                    Element::from(state.core.applet.popup_container(state.popup_body()))
                        .map(cosmic::Action::App)
                })),
            ))
        };

        let area = cosmic::widget::mouse_area(tooltipped).on_right_press(right_click);

        // Let the panel surface grow to fit the (possibly enlarged) label so
        // wider text isn't clipped on the trailing edge.
        applet.autosize_window(area).into()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
