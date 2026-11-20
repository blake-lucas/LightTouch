//! Linux: letting the person at the computer open Switch and PlayStation controllers' motion sensors.
//!
//! SDL reads those sensors through the controller's /dev/hidraw node, or (SDL 2.28 and newer) the separate motion
//! device the kernel's hid-nintendo or hid-playstation driver makes. Most systems let only root open either unless a
//! udev rule says otherwise, as Steam's controller rules (steam-devices) do. The fix is one rule file of our own in
//! /etc/udev/rules.d, the place for local additions: it changes no other file, survives updates (/etc is kept on
//! Fedora Silverblue, Bluefin, Bazzite and other atomic systems, whose /usr is read-only), and deleting it undoes it.
//! `uaccess` gives access to whoever is signed in at the computer only, as Steam's rules do, not to every account.

/// Where the rule goes. Before 73-seat-late.rules, which applies `uaccess`.
pub const RULES_PATH: &str = "/etc/udev/rules.d/70-lighttouch-controllers.rules";

pub const RULES: &str = r#"# Added by LightTouch so the person signed in here can read Switch and PlayStation
# controllers' motion sensors, as Steam's rules allow. Delete this file to undo.
# Raw HID over USB and Bluetooth: Nintendo 057e, Sony 054c.
KERNEL=="hidraw*", ATTRS{idVendor}=="057e", TAG+="uaccess"
KERNEL=="hidraw*", KERNELS=="*057E:*", TAG+="uaccess"
KERNEL=="hidraw*", ATTRS{idVendor}=="054c", TAG+="uaccess"
KERNEL=="hidraw*", KERNELS=="*054C:*", TAG+="uaccess"
# The motion devices of the kernel's hid-nintendo and hid-playstation drivers.
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="Nintendo Switch*IMU", TAG+="uaccess"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="*Pro Controller (IMU)", TAG+="uaccess"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="Joy-Con*(IMU)", TAG+="uaccess"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="Sony*Motion Sensors", TAG+="uaccess"
SUBSYSTEM=="input", KERNEL=="event*", ATTRS{name}=="*Wireless Controller Motion Sensors", TAG+="uaccess"
"#;

/// Apply the rules to devices already connected (`change` re-runs uaccess for the signed-in person).
const RELOAD: &str = "udevadm control --reload-rules && udevadm trigger --action=change --subsystem-match=hidraw \
                      --subsystem-match=input";

/// The script that installs the rule, run as root.
fn script() -> String {
    format!(
        "set -e\nmkdir -p /etc/udev/rules.d\ncat > {RULES_PATH}.tmp <<'LIGHTTOUCH_RULES'\n{RULES}LIGHTTOUCH_RULES\n\
         chmod 0644 {RULES_PATH}.tmp\nmv {RULES_PATH}.tmp {RULES_PATH}\n{RELOAD}\n"
    )
}

/// The same, for someone to run in a terminal.
pub fn install_commands() -> String {
    format!(
        "sudo tee {RULES_PATH} > /dev/null <<'LIGHTTOUCH_RULES'\n{RULES}LIGHTTOUCH_RULES\n\
         sudo udevadm control --reload-rules\n\
         sudo udevadm trigger --action=change --subsystem-match=hidraw --subsystem-match=input\n"
    )
}

pub fn remove_commands() -> String {
    format!("sudo rm {RULES_PATH}\nsudo udevadm control --reload-rules\n")
}

pub fn installed() -> bool {
    std::path::Path::new(RULES_PATH).exists()
}

/// Install the rule through pkexec, which asks for the password in the desktop's own prompt. Blocks until done.
pub fn install() -> Result<(), String> {
    let pkexec = ["/usr/bin/pkexec", "/bin/pkexec", "/usr/local/bin/pkexec"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or("This system has no pkexec to ask for your password: run the commands shown under ? yourself")?;
    let output = std::process::Command::new(pkexec)
        .args(["/bin/sh", "-c", &script()])
        // The AppImage's library path is for LightTouch, not the system's tools.
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run pkexec: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 when the prompt was dismissed or not allowed, 127 when authentication failed.
        Some(126) => Err("Not done: the password prompt was cancelled".into()),
        Some(127) => Err("Not done: your password wasn't accepted, or no password prompt is available. You can run \
                          the commands shown under ? yourself"
            .into()),
        _ => {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            Err(if err.is_empty() { format!("It didn't finish ({})", output.status) } else { err.to_string() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_writes_exactly_the_rules() {
        let s = script();
        let body = s.split("<<'LIGHTTOUCH_RULES'\n").nth(1).unwrap().split("LIGHTTOUCH_RULES\n").next().unwrap();
        assert_eq!(body, RULES);
        assert!(RULES.ends_with('\n'));
        assert!(install_commands().contains(RULES));
    }

    #[test]
    fn the_script_installs_the_file_whole() {
        let dir = std::env::temp_dir().join(format!("lt-udev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("70-lighttouch-controllers.rules");
        // The same script, writing into the temporary folder, with udevadm left out.
        let script = script()
            .replace(RULES_PATH, target.to_str().unwrap())
            .replace("/etc/udev/rules.d", dir.to_str().unwrap())
            .replace(RELOAD, "true");
        let status = std::process::Command::new("/bin/sh").args(["-c", &script]).status().unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), RULES);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o644);
        // The commands shown to type, as the person would (less sudo), then the undo.
        std::fs::remove_file(&target).unwrap();
        let typed = |text: String| {
            text.replace("sudo ", "")
                .replace(RULES_PATH, target.to_str().unwrap())
                .lines()
                .map(|l| if l.starts_with("udevadm") { "true" } else { l })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            std::process::Command::new("/bin/sh").args(["-c", &typed(install_commands())]).status().unwrap().success()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), RULES);
        assert!(
            std::process::Command::new("/bin/sh").args(["-c", &typed(remove_commands())]).status().unwrap().success()
        );
        assert!(!target.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
