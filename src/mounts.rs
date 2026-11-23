//! Mount-state detection and mount/umount command construction.

/// Filesystem types the kernel reports for SMB mounts.
const SMB_FSTYPES: [&str; 2] = ["cifs", "smb3"];

/// Whether the CIFS mount helper (`mount.cifs` from the `cifs-utils` package)
/// is installed. Without it, `mount -t cifs` fails with an opaque error.
pub fn mount_cifs_available() -> bool {
    ["/sbin/mount.cifs", "/usr/sbin/mount.cifs", "/bin/mount.cifs"]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
}

/// Returns true if an SMB filesystem is currently mounted at `mount_point`,
/// according to /proc/mounts.
pub fn is_mounted(mount_point: &str) -> bool {
    std::fs::read_to_string("/proc/mounts")
        .map(|contents| is_mounted_in(&contents, mount_point))
        .unwrap_or(false)
}

/// Checks /proc/mounts-formatted `contents` for an SMB (`cifs`/`smb3`) mount
/// whose target is `mount_point`. Other filesystems mounted at the same path
/// do not count. The share source is deliberately not compared: the same
/// share is often mounted by IP in fstab but configured here by hostname.
pub fn is_mounted_in(contents: &str, mount_point: &str) -> bool {
    let target = normalize(mount_point);
    contents.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let (Some(_source), Some(mount), Some(fstype)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        SMB_FSTYPES.contains(&fstype) && normalize(&unescape_mount_field(mount)) == target
    })
}

/// Strips trailing slashes so `/mnt/nas/` and `/mnt/nas` compare equal.
fn normalize(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// Decodes the `\ooo` octal escapes /proc/mounts uses for space, tab,
/// newline, and backslash (and any other byte) in mount paths.
pub fn unescape_mount_field(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let digits = &bytes[i + 1..i + 4];
            if digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                let value = digits
                    .iter()
                    .fold(0u32, |acc, d| acc * 8 + u32::from(d - b'0'));
                if let Ok(byte) = u8::try_from(value) {
                    out.push(byte);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A program and its arguments, built separately from `tokio::process` so it
/// can be unit-tested.
#[derive(Debug, PartialEq)]
pub struct CommandSpec {
    pub program: &'static str,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn to_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(self.program);
        cmd.args(&self.args);
        cmd
    }
}

/// Wraps `tool` (`mount`/`umount`) in `pkexec` when requested.
fn privileged(tool: &'static str, use_pkexec: bool, mut args: Vec<String>) -> CommandSpec {
    if use_pkexec {
        args.insert(0, tool.to_string());
        CommandSpec {
            program: "pkexec",
            args,
        }
    } else {
        CommandSpec {
            program: tool,
            args,
        }
    }
}

/// `mount -t cifs [-o OPTIONS] -- SOURCE MOUNT_POINT`. The `--` keeps a source
/// or mount point that begins with `-` from being parsed as an option.
pub fn mount_command(
    source: &str,
    mount_point: &str,
    options: Option<&str>,
    use_pkexec: bool,
) -> CommandSpec {
    let mut args = vec!["-t".to_string(), "cifs".to_string()];
    if let Some(opts) = options {
        args.push("-o".to_string());
        args.push(opts.to_string());
    }
    args.push("--".to_string());
    args.push(source.to_string());
    args.push(mount_point.to_string());
    privileged("mount", use_pkexec, args)
}

/// `umount -- MOUNT_POINT`.
pub fn unmount_command(mount_point: &str, use_pkexec: bool) -> CommandSpec {
    privileged(
        "umount",
        use_pkexec,
        vec!["--".to_string(), mount_point.to_string()],
    )
}

/// Runs a prepared mount/umount command, returning `None` on success or a
/// short human-readable error message on failure.
pub async fn run_mount_command(spec: CommandSpec, verb: &str) -> Option<String> {
    match spec.to_command().output().await {
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

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "\
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
//nas.local/data /mnt/nas cifs rw,relatime,vers=3.1.1 0 0
//192.168.1.10/media /mnt/my\\040media smb3 rw,relatime 0 0
/dev/sdb1 /mnt/usb ext4 rw,relatime 0 0
";

    #[test]
    fn detects_cifs_mount() {
        assert!(is_mounted_in(MOUNTS, "/mnt/nas"));
        assert!(is_mounted_in(MOUNTS, "/mnt/nas/"));
    }

    #[test]
    fn detects_smb3_mount_with_escaped_space() {
        assert!(is_mounted_in(MOUNTS, "/mnt/my media"));
    }

    #[test]
    fn ignores_non_smb_filesystems_at_mount_point() {
        assert!(!is_mounted_in(MOUNTS, "/mnt/usb"));
        assert!(!is_mounted_in(MOUNTS, "/"));
    }

    #[test]
    fn missing_mount_point_is_not_mounted() {
        assert!(!is_mounted_in(MOUNTS, "/mnt/other"));
        assert!(!is_mounted_in("", "/mnt/nas"));
        assert!(!is_mounted_in("garbage\n", "/mnt/nas"));
    }

    #[test]
    fn unescapes_octal_sequences() {
        assert_eq!(unescape_mount_field(r"/a\040b"), "/a b");
        assert_eq!(unescape_mount_field(r"/a\011b"), "/a\tb");
        assert_eq!(unescape_mount_field(r"/a\012b"), "/a\nb");
        assert_eq!(unescape_mount_field(r"/a\134b"), r"/a\b");
        // Not a full escape: left untouched.
        assert_eq!(unescape_mount_field(r"/a\04"), r"/a\04");
        assert_eq!(unescape_mount_field(r"/a\9xy"), r"/a\9xy");
        assert_eq!(unescape_mount_field(r"/trailing\"), r"/trailing\");
    }

    #[test]
    fn builds_mount_command_with_pkexec() {
        let spec = mount_command("//nas/data", "/mnt/nas", Some("uid=1000"), true);
        assert_eq!(spec.program, "pkexec");
        assert_eq!(
            spec.args,
            ["mount", "-t", "cifs", "-o", "uid=1000", "--", "//nas/data", "/mnt/nas"]
        );
    }

    #[test]
    fn builds_mount_command_without_pkexec_or_options() {
        let spec = mount_command("//nas/data", "-weird", None, false);
        assert_eq!(spec.program, "mount");
        assert_eq!(spec.args, ["-t", "cifs", "--", "//nas/data", "-weird"]);
    }

    #[test]
    fn builds_unmount_command() {
        assert_eq!(
            unmount_command("/mnt/nas", true),
            CommandSpec {
                program: "pkexec",
                args: vec!["umount".into(), "--".into(), "/mnt/nas".into()],
            }
        );
        assert_eq!(unmount_command("/mnt/nas", false).args, ["--", "/mnt/nas"]);
    }
}
