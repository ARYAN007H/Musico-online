//! App identity — the single source of truth for every on-disk path, D-Bus
//! name and window title in the online build.
//!
//! The clean-room rebuild deliberately uses its **own** data namespace
//! (`~/.local/share/musico-online`, `~/.cache/musico-online`,
//! `~/.config/musico-online`) so it can run side-by-side with the public
//! Musico install without fighting over the sled DB, cover cache or settings.

use std::path::PathBuf;

/// Directory name under XDG data/config/cache roots.
pub const DATA_SUBDIR: &str = "musico-online";

/// D-Bus / SMTC name (media keys, playerctl).
pub const DBUS_NAME: &str = "musico-online";

/// MPRIS bus suffix: `org.mpris.MediaPlayer2.<name>`.
pub const MPRIS_NAME: &str = "org.mpris.MediaPlayer2.musico-online";

/// Window title / desktop entry display name.
pub const DISPLAY_NAME: &str = "Musico Online";

/// XDG data dir for this app (`~/.local/share/musico-online`).
pub fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(DATA_SUBDIR))
}

/// XDG cache dir for this app (`~/.cache/musico-online`).
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join(DATA_SUBDIR))
}

/// XDG config dir for this app (`~/.config/musico-online`).
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(DATA_SUBDIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdirs_are_namespaced() {
        let data = data_dir().expect("data dir exists on linux");
        assert!(data.ends_with("musico-online"), "got {data:?}");
        assert!(!data.ends_with("/musico"), "must not collide with public Musico");
        let cache = cache_dir().unwrap();
        assert!(cache.ends_with("musico-online"));
        let cfg = config_dir().unwrap();
        assert!(cfg.ends_with("musico-online"));
    }

    #[test]
    fn mpris_name_is_well_formed() {
        assert!(MPRIS_NAME.starts_with("org.mpris.MediaPlayer2."));
        assert!(DBUS_NAME.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
