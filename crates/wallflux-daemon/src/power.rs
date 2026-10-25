//! Power source detection, read straight from sysfs.
//!
//! No logind, no upower, no D-Bus: three files in `/sys/class/power_supply`
//! answer the only question wallflux asks, and a daemon that already avoids
//! linking libmpv at build time should not drag in a D-Bus client for this.

use std::path::Path;
use std::time::{Duration, Instant};

/// How often sysfs is re-read. Power state changes are rare and the daemon's
/// reaction (an fps cap) is not time critical.
const POLL_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    /// Mains, or no battery at all — a desktop reports this.
    Ac,
    Battery,
}

pub struct PowerMonitor {
    root: std::path::PathBuf,
    source: PowerSource,
    last_check: Option<Instant>,
}

impl Default for PowerMonitor {
    fn default() -> Self {
        Self::new(Path::new("/sys/class/power_supply"))
    }
}

impl PowerMonitor {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            source: PowerSource::Ac,
            last_check: None,
        }
    }

    /// Current source, re-reading sysfs at most every [`POLL_INTERVAL`].
    /// Returns `(source, changed)`.
    pub fn poll(&mut self) -> (PowerSource, bool) {
        let due = self
            .last_check
            .map(|at| at.elapsed() >= POLL_INTERVAL)
            .unwrap_or(true);
        if !due {
            return (self.source, false);
        }
        self.last_check = Some(Instant::now());
        let fresh = read_source(&self.root);
        let changed = fresh != self.source;
        if changed {
            tracing::info!(?fresh, "power source changed");
            self.source = fresh;
        }
        (self.source, changed)
    }

    pub fn source(&self) -> PowerSource {
        self.source
    }
}

/// A machine is on battery when it has a mains supply that reports offline.
///
/// Keying off the AC adapter rather than the battery's own `status` is
/// deliberate: `status` reads `Discharging` for a few seconds after plugging
/// in, and `Unknown` on plenty of hardware, which would make the fps cap
/// flap. `online` is a single authoritative bit.
fn read_source(root: &Path) -> PowerSource {
    let Ok(entries) = std::fs::read_dir(root) else {
        // No sysfs power class at all: a container or an exotic kernel.
        return PowerSource::Ac;
    };

    let mut saw_mains = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = read_trimmed(&path.join("type")).unwrap_or_default();
        if kind != "Mains" && kind != "USB" && kind != "USB_PD" {
            continue;
        }
        saw_mains = true;
        if read_trimmed(&path.join("online")).as_deref() == Some("1") {
            return PowerSource::Ac;
        }
    }

    if saw_mains {
        PowerSource::Battery
    } else {
        // A desktop with no adapter node, or a VM: never throttle it.
        PowerSource::Ac
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sysfs(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("wallflux-power-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn supply(root: &Path, name: &str, kind: &str, online: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        if let Some(online) = online {
            std::fs::write(dir.join("online"), format!("{online}\n")).unwrap();
        }
    }

    #[test]
    fn no_sysfs_means_ac() {
        assert_eq!(read_source(Path::new("/nonexistent")), PowerSource::Ac);
    }

    #[test]
    fn a_desktop_without_an_adapter_is_ac() {
        let root = fake_sysfs("desktop");
        assert_eq!(read_source(&root), PowerSource::Ac);
    }

    #[test]
    fn offline_adapter_means_battery() {
        let root = fake_sysfs("unplugged");
        supply(&root, "AC", "Mains", Some("0"));
        supply(&root, "BAT0", "Battery", None);
        assert_eq!(read_source(&root), PowerSource::Battery);
    }

    #[test]
    fn online_adapter_means_ac() {
        let root = fake_sysfs("plugged");
        supply(&root, "AC", "Mains", Some("1"));
        supply(&root, "BAT0", "Battery", None);
        assert_eq!(read_source(&root), PowerSource::Ac);
    }

    #[test]
    fn any_online_supply_wins_over_an_offline_one() {
        // A laptop charging over USB-C with the barrel adapter unplugged.
        let root = fake_sysfs("usb-c");
        supply(&root, "AC", "Mains", Some("0"));
        supply(&root, "ucsi-source-psy-USBC000:001", "USB", Some("1"));
        assert_eq!(read_source(&root), PowerSource::Ac);
    }

    #[test]
    fn the_monitor_caches_between_polls() {
        let root = fake_sysfs("cache");
        supply(&root, "AC", "Mains", Some("1"));
        let mut monitor = PowerMonitor::new(&root);
        let (source, changed) = monitor.poll();
        assert_eq!(source, PowerSource::Ac);
        assert!(!changed, "first read matches the Ac default");

        // Unplug: the cached value stands until the interval elapses.
        supply(&root, "AC", "Mains", Some("0"));
        let (source, changed) = monitor.poll();
        assert_eq!(source, PowerSource::Ac);
        assert!(!changed);

        monitor.last_check = Some(Instant::now() - POLL_INTERVAL - Duration::from_secs(1));
        let (source, changed) = monitor.poll();
        assert_eq!(source, PowerSource::Battery);
        assert!(changed);
    }
}
