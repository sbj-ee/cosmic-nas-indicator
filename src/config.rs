//! Applet configuration: defaults, parsing, and persistence of the
//! `~/.config/cosmic-nas-indicator/config` KEY=VALUE file.

use std::path::PathBuf;

/// Fallback defaults, overridable via the config file or the in-applet
/// settings panel. The host is intentionally empty so a fresh install starts
/// in an "unconfigured" state rather than probing a stranger's hostname.
pub const DEFAULT_HOST: &str = "";
pub const DEFAULT_PORT: u16 = 445;
pub const DEFAULT_INTERVAL_SECS: u64 = 10;
/// Shortest allowed polling interval.
pub const MIN_INTERVAL_SECS: u64 = 2;

/// Font-size handling for the panel label when the user overrides sizing via
/// the right-click menu. `DEFAULT_FONT_SIZE` is the starting point when no
/// explicit size has been chosen yet. The size can only be nudged
/// `FONT_STEPS` increments of `FONT_STEP` in either direction from the default.
pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const FONT_STEP: f32 = 2.0;
const FONT_STEPS: f32 = 2.0;
pub const MIN_FONT_SIZE: f32 = DEFAULT_FONT_SIZE - FONT_STEP * FONT_STEPS;
pub const MAX_FONT_SIZE: f32 = DEFAULT_FONT_SIZE + FONT_STEP * FONT_STEPS;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub interval_secs: u64,
    /// Explicit panel label font size. `None` means "use the panel-derived
    /// default sizing from libcosmic".
    pub font_size: Option<f32>,
    /// SMB share to mount. Either a full source like `//host/share` (or a
    /// UNC-style `\\host\share`) or a bare share name, in which case the
    /// source becomes `//<host>/<share>`.
    pub share: Option<String>,
    /// Local directory the share is mounted onto.
    pub mount_point: Option<String>,
    /// Extra options passed to `mount -o` (e.g. `credentials=/path,uid=1000`).
    pub mount_options: Option<String>,
    /// Run mount/umount through `pkexec` so a normal user gets a polkit auth
    /// prompt. Set to `false` if you have an fstab `user` entry instead.
    pub use_pkexec: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            interval_secs: DEFAULT_INTERVAL_SECS,
            font_size: None,
            share: None,
            mount_point: None,
            mount_options: None,
            use_pkexec: true,
        }
    }
}

impl Config {
    /// Loads the config file. A missing or unreadable file yields defaults.
    pub fn load() -> Self {
        config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|contents| Self::parse(&contents))
            .unwrap_or_default()
    }

    /// Parses KEY=VALUE lines. Recognized keys: host, port, interval_secs,
    /// font_size, share, mount_point, mount_options, use_pkexec. Blank lines
    /// and `#` comments are skipped; unknown keys and invalid values are
    /// ignored and fall back to the defaults.
    pub fn parse(contents: &str) -> Self {
        let mut config = Config::default();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "host" => config.host = value.to_string(),
                "port" => {
                    if let Some(port) = parse_port(value) {
                        config.port = port;
                    }
                }
                "interval_secs" => {
                    if let Some(secs) = parse_interval(value) {
                        config.interval_secs = secs;
                    }
                }
                "font_size" => {
                    if let Ok(size) = value.parse::<f32>() {
                        if size.is_finite() {
                            config.font_size = Some(size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
                        }
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
        config
    }

    /// Whether the applet has enough configuration to do anything useful (a
    /// non-empty host to probe).
    pub fn is_configured(&self) -> bool {
        !self.host.trim().is_empty()
    }

    /// Persists the configuration to the config file, updating known keys in
    /// place while preserving comments and any unrecognized lines.
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither XDG_CONFIG_HOME nor HOME is set",
            )
        })?;
        let existing = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.render(&existing))
    }

    /// Produces the new file contents from `existing`: known keys are
    /// rewritten in place (duplicates dropped, unset keys removed), other
    /// lines are kept verbatim, and keys not yet present are appended.
    pub fn render(&self, existing: &str) -> String {
        // Desired value for each known key. `None` means the key should be
        // absent from the file entirely.
        let desired: [(&str, Option<String>); 8] = [
            ("host", non_empty(self.host.trim())),
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
                    if !seen.contains(k) {
                        seen.push(k);
                        if let Some(v) = val {
                            out.push(format!("{k} = {v}"));
                        }
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
        out.join("\n") + "\n"
    }

    /// The `mount -t cifs` source for the configured share, if any. A bare
    /// share name is expanded to `//<host>/<share>`; a value already starting
    /// with `//` or `\\` is used as-is (with backslashes normalized).
    pub fn mount_source(&self) -> Option<String> {
        let share = self.share.as_deref()?;
        if share.starts_with("//") || share.starts_with(r"\\") {
            Some(share.replace('\\', "/"))
        } else {
            Some(format!(
                "//{}/{}",
                self.host.trim(),
                share.trim_start_matches('/')
            ))
        }
    }

    /// Whether both a share and a mount point are configured, so the
    /// connect/disconnect menu item is meaningful.
    pub fn is_mountable(&self) -> bool {
        self.mount_source().is_some() && self.mount_point.is_some()
    }
}

/// Parses a TCP port, rejecting 0 and anything out of range.
pub fn parse_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|&p| p != 0)
}

/// Parses a polling interval in seconds, clamped to `MIN_INTERVAL_SECS`.
pub fn parse_interval(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| s.max(MIN_INTERVAL_SECS))
}

pub fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// `$XDG_CONFIG_HOME/cosmic-nas-indicator/config`, falling back to
/// `$HOME/.config/...`. A relative `XDG_CONFIG_HOME` is ignored, per the XDG
/// spec. Returns `None` if no usable base directory is known.
fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cosmic-nas-indicator").join("config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_gives_defaults() {
        let c = Config::parse("");
        assert_eq!(c, Config::default());
        assert!(!c.is_configured());
        assert!(!c.is_mountable());
    }

    #[test]
    fn parses_full_example() {
        let c = Config::parse(
            "host=nas.local\n\
             port=139\n\
             interval_secs=30\n\
             font_size=16\n\
             \n\
             # comment line\n\
             share=data\n\
             mount_point=/mnt/nas\n\
             mount_options=credentials=/home/me/.smbcredentials,uid=1000\n\
             use_pkexec=false\n",
        );
        assert_eq!(c.host, "nas.local");
        assert_eq!(c.port, 139);
        assert_eq!(c.interval_secs, 30);
        assert_eq!(c.font_size, Some(16.0));
        assert_eq!(c.share.as_deref(), Some("data"));
        assert_eq!(c.mount_point.as_deref(), Some("/mnt/nas"));
        // Only the first '=' separates key from value.
        assert_eq!(
            c.mount_options.as_deref(),
            Some("credentials=/home/me/.smbcredentials,uid=1000")
        );
        assert!(!c.use_pkexec);
        assert!(c.is_configured());
        assert!(c.is_mountable());
    }

    #[test]
    fn tolerates_whitespace_around_keys_and_values() {
        let c = Config::parse("  host =  nas.local  \n  port = 445 ");
        assert_eq!(c.host, "nas.local");
        assert_eq!(c.port, 445);
    }

    #[test]
    fn invalid_values_fall_back_to_defaults() {
        let c = Config::parse(
            "port=0\ninterval_secs=abc\nfont_size=big\nuse_pkexec=maybe\nnot a kv line",
        );
        assert_eq!(c.port, DEFAULT_PORT);
        assert_eq!(c.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(c.font_size, None);
        assert!(c.use_pkexec);

        assert_eq!(Config::parse("port=70000").port, DEFAULT_PORT);
        assert_eq!(Config::parse("font_size=NaN").font_size, None);
    }

    #[test]
    fn clamps_interval_and_font_size() {
        assert_eq!(Config::parse("interval_secs=0").interval_secs, MIN_INTERVAL_SECS);
        assert_eq!(Config::parse("font_size=1").font_size, Some(MIN_FONT_SIZE));
        assert_eq!(Config::parse("font_size=99").font_size, Some(MAX_FONT_SIZE));
    }

    #[test]
    fn empty_optional_values_are_none() {
        let c = Config::parse("share=\nmount_point=\nmount_options=");
        assert_eq!(c.share, None);
        assert_eq!(c.mount_point, None);
        assert_eq!(c.mount_options, None);
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_port(" 445 "), Some(445));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("-1"), None);
        assert_eq!(parse_interval("1"), Some(MIN_INTERVAL_SECS));
        assert_eq!(parse_interval("15"), Some(15));
        assert_eq!(parse_interval(""), None);
    }

    #[test]
    fn render_appends_to_empty_file() {
        let c = Config {
            host: "nas".into(),
            ..Config::default()
        };
        assert_eq!(
            c.render(""),
            "host = nas\nport = 445\ninterval_secs = 10\nuse_pkexec = true\n"
        );
    }

    #[test]
    fn render_updates_in_place_and_preserves_other_lines() {
        let existing = "# my NAS\nhost=old\nunknown_key=keep me\n\nport=445\n";
        let c = Config {
            host: "new".into(),
            port: 4450,
            ..Config::default()
        };
        assert_eq!(
            c.render(existing),
            "# my NAS\nhost = new\nunknown_key=keep me\n\nport = 4450\n\
             interval_secs = 10\nuse_pkexec = true\n"
        );
    }

    #[test]
    fn render_removes_unset_keys_and_duplicates() {
        let existing = "host=a\nshare=data\nhost=b\nfont_size=16\n";
        let c = Config {
            host: "nas".into(),
            ..Config::default()
        };
        let out = c.render(existing);
        assert_eq!(out.matches("host").count(), 1);
        assert!(!out.contains("share"));
        assert!(!out.contains("font_size"));
    }

    #[test]
    fn render_keeps_commented_out_keys() {
        let c = Config {
            host: "nas".into(),
            ..Config::default()
        };
        assert!(c.render("#host=old\n").starts_with("#host=old\nhost = nas\n"));
    }

    #[test]
    fn render_then_parse_round_trips() {
        let c = Config {
            host: "nas.local".into(),
            port: 4450,
            interval_secs: 7,
            font_size: Some(12.0),
            share: Some("//nas.local/data".into()),
            mount_point: Some("/mnt/my nas".into()),
            mount_options: Some("credentials=/x,uid=1000".into()),
            use_pkexec: false,
        };
        assert_eq!(Config::parse(&c.render("")), c);
    }

    #[test]
    fn mount_source_variants() {
        let mut c = Config {
            host: " nas.local ".into(),
            ..Config::default()
        };
        assert_eq!(c.mount_source(), None);

        c.share = Some("data".into());
        assert_eq!(c.mount_source().as_deref(), Some("//nas.local/data"));

        c.share = Some("/data".into());
        assert_eq!(c.mount_source().as_deref(), Some("//nas.local/data"));

        c.share = Some("//other/share".into());
        assert_eq!(c.mount_source().as_deref(), Some("//other/share"));

        c.share = Some(r"\\other\share".into());
        assert_eq!(c.mount_source().as_deref(), Some("//other/share"));
    }

    #[test]
    fn mountable_requires_share_and_mount_point() {
        let mut c = Config {
            host: "nas".into(),
            share: Some("data".into()),
            ..Config::default()
        };
        assert!(!c.is_mountable());
        c.mount_point = Some("/mnt/nas".into());
        assert!(c.is_mountable());
    }
}
