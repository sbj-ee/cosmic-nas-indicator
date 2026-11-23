mod config;
mod mounts;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use cosmic::app::{Core, Task};
use cosmic::iced::window::Id;
use cosmic::iced::{time, Alignment, Color, Length, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::{Application, Element};

use config::{
    non_empty, parse_interval, parse_port, Config, DEFAULT_FONT_SIZE, FONT_STEP, MAX_FONT_SIZE,
    MIN_FONT_SIZE,
};

/// Upper bound on one reachability probe (DNS lookup plus TCP connect).
const CONNECT_TIMEOUT_SECS: u64 = 3;

const GREEN: Color = Color::from_rgb(0.18, 0.80, 0.25);
const ORANGE: Color = Color::from_rgb(1.0, 0.58, 0.05);
const RED: Color = Color::from_rgb(0.90, 0.16, 0.16);
/// Neutral color used before the applet has been configured.
const GREY: Color = Color::from_rgb(0.6, 0.6, 0.6);

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<NasIndicator>(())
}

/// The most recent probe result, used to color the text and fill the tooltip.
#[derive(Debug, Clone, Default)]
struct Status {
    connected: bool,
    /// Resolved peer address on a successful connect (gives the NAS IP).
    addr: Option<SocketAddr>,
    /// Time taken by the successful TCP connect (excluding DNS resolution).
    latency: Option<Duration>,
    /// Whether an SMB filesystem is currently mounted at the mount point.
    mounted: bool,
}

/// Resolves `host`, then tries each resolved address until one accepts a TCP
/// connection on `port`. The whole probe is bounded by `CONNECT_TIMEOUT_SECS`.
async fn check_nas(host: String, port: u16, mount_point: Option<String>) -> Status {
    let mounted = mount_point.as_deref().map(mounts::is_mounted).unwrap_or(false);
    let probe = async {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .ok()?
            .collect();
        for addr in addrs {
            let start = Instant::now();
            if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
                let latency = start.elapsed();
                return Some((stream.peer_addr().unwrap_or(addr), latency));
            }
        }
        None
    };
    match tokio::time::timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS), probe).await {
        Ok(Some((addr, latency))) => Status {
            connected: true,
            addr: Some(addr),
            latency: Some(latency),
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
    /// Sequence number handed to the most recently started probe. Probes can
    /// overlap (the interval may be shorter than the timeout) and finish out
    /// of order, so each result carries its sequence number.
    next_check_seq: u64,
    /// Sequence number of the probe whose result is currently in `status`.
    applied_check_seq: u64,
    /// Results from probes older than this were started against settings
    /// that have since changed, and are discarded.
    min_valid_check_seq: u64,
    /// The currently open right-click menu popup, if any.
    popup: Option<Id>,
    /// When true, the popup shows the settings form instead of the menu.
    editing: bool,
    /// Buffers for the settings form while editing.
    form: SettingsForm,
    /// Last mount/unmount error to surface to the user, if any.
    mount_error: Option<String>,
    /// Last failure to write the config file, if any.
    save_error: Option<String>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    /// A probe result, tagged with the probe's sequence number.
    Status(u64, Status),
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
    fn check_task(&mut self) -> Task<Message> {
        if !self.config.is_configured() {
            return Task::none();
        }
        self.next_check_seq += 1;
        let seq = self.next_check_seq;
        let host = self.config.host.trim().to_string();
        let port = self.config.port;
        let mount_point = self.config.mount_point.clone();
        cosmic::task::future(async move {
            Message::Status(seq, check_nas(host, port, mount_point).await)
        })
    }

    /// Runs `mount` (via pkexec when configured) for the configured share.
    /// Reports the outcome via `Message::MountDone`, which re-checks status.
    fn mount_task(&self) -> Task<Message> {
        let (Some(source), Some(mount_point)) =
            (self.config.mount_source(), self.config.mount_point.clone())
        else {
            return Task::none();
        };
        let precondition_error = if !mounts::mount_cifs_available() {
            Some("cifs-utils not installed (mount.cifs missing)".to_string())
        } else if !std::path::Path::new(&mount_point).is_dir() {
            Some(format!("mount point {mount_point} does not exist"))
        } else {
            None
        };
        if let Some(err) = precondition_error {
            return cosmic::task::future(async move { Message::MountDone(Some(err)) });
        }
        let spec = mounts::mount_command(
            &source,
            &mount_point,
            self.config.mount_options.as_deref(),
            self.config.use_pkexec,
        );
        cosmic::task::future(async move {
            Message::MountDone(mounts::run_mount_command(spec, "mount").await)
        })
    }

    /// Runs `umount` (via pkexec when configured) for the configured mount
    /// point. Reports the outcome via `Message::MountDone`.
    fn unmount_task(&self) -> Task<Message> {
        let Some(mount_point) = self.config.mount_point.as_deref() else {
            return Task::none();
        };
        let spec = mounts::unmount_command(mount_point, self.config.use_pkexec);
        cosmic::task::future(async move {
            Message::MountDone(mounts::run_mount_command(spec, "umount").await)
        })
    }

    /// Writes the config file, recording any failure for display.
    fn save_config(&mut self) {
        self.save_error = self
            .config
            .save()
            .err()
            .map(|e| format!("could not save settings: {e}"));
    }

    /// Parses the settings form into the config and persists it. Invalid
    /// port or interval entries leave the previous value in place.
    fn apply_settings(&mut self) {
        self.config.host = self.form.host.trim().to_string();
        if let Some(port) = parse_port(&self.form.port) {
            self.config.port = port;
        }
        if let Some(secs) = parse_interval(&self.form.interval_secs) {
            self.config.interval_secs = secs;
        }
        self.config.share = non_empty(self.form.share.trim());
        self.config.mount_point = non_empty(self.form.mount_point.trim());
        self.config.mount_options = non_empty(self.form.mount_options.trim());
        self.config.use_pkexec = self.form.use_pkexec;
        self.save_config();

        // Any probe still in flight was aimed at the old settings; ignore its
        // result and forget the old status until the next probe reports.
        self.min_valid_check_seq = self.next_check_seq + 1;
        self.status = Status::default();
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

    /// Errors to surface in the tooltip and menu.
    fn errors(&self) -> impl Iterator<Item = &String> {
        self.mount_error.iter().chain(self.save_error.iter())
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
            lines.extend(self.errors().map(|err| format!("⚠ {err}")));
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
        lines.extend(self.errors().map(|err| format!("⚠ {err}")));

        lines.join("\n")
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

        let mut menu = Column::new()
            .padding([8, 0])
            .push(padded_control(text::heading(header)));

        if self.config.is_configured() {
            menu = menu.push(padded_control(text::caption(format!(
                "{}:{}",
                self.config.host, self.config.port
            ))));
        }

        for err in self.errors() {
            menu = menu.push(padded_control(
                text::caption(format!("⚠ {err}")).class(cosmic::style::Text::Color(ORANGE)),
            ));
        }

        // Connect / disconnect (mount / unmount) — only when a share and mount
        // point are configured. Connecting is disabled while the NAS is
        // unreachable, since the mount would only time out.
        if self.config.is_mountable() {
            menu = menu.push(padded_control(divider::horizontal::default()));
            menu = if self.status.mounted {
                menu.push(
                    menu_button(text::body("Disconnect (unmount)"))
                        .on_press(Message::Disconnect),
                )
            } else {
                menu.push(
                    menu_button(text::body("Connect (mount)"))
                        .on_press_maybe(self.status.connected.then_some(Message::Connect)),
                )
            };
        }

        // With no explicit size, libcosmic sizes the label from the panel, so
        // there is no meaningful number to show.
        let font_label = match self.config.font_size {
            Some(size) => format!("Font size: {size:.0}"),
            None => "Font size: panel default".to_string(),
        };
        let current = self.config.font_size.unwrap_or(DEFAULT_FONT_SIZE);
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
            .push(
                menu_button(text::body("Reset font size"))
                    .on_press_maybe(self.config.font_size.is_some().then_some(Message::FontReset)),
            )
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
        let mut app = NasIndicator {
            core,
            config: Config::load(),
            status: Status::default(),
            next_check_seq: 0,
            applied_check_seq: 0,
            min_valid_check_seq: 0,
            popup: None,
            editing: false,
            form: SettingsForm::default(),
            mount_error: None,
            save_error: None,
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
            Message::Status(seq, status) => {
                if seq >= self.min_valid_check_seq && seq > self.applied_check_seq {
                    self.applied_check_seq = seq;
                    self.status = status;
                }
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
                let current = self.config.font_size.unwrap_or(DEFAULT_FONT_SIZE);
                self.config.font_size = Some((current + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
                self.save_config();
                Task::none()
            }
            Message::FontReset => {
                self.config.font_size = None;
                self.save_config();
                Task::none()
            }
            Message::Connect => {
                self.mount_error = None;
                let close = self.close_popup_task();
                Task::batch([close, self.mount_task()])
            }
            Message::Disconnect => {
                self.mount_error = None;
                let close = self.close_popup_task();
                Task::batch([close, self.unmount_task()])
            }
            Message::MountDone(result) => {
                self.mount_error = result;
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
