//! How the chrome moves: pure timing, so it is tested without a window or a clock.
//!
//! Like the cursor's animation, nothing here runs on a schedule of its own. A draw asks
//! what things look like now and when that next changes, so a window with nothing
//! moving stops drawing.

use super::config::MotionPreference;

/// How far the chrome is allowed to move, once the setting and the platform have had
/// their say.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum MotionPace {
    #[default]
    Full,
    /// Things change at once rather than travelling there.
    Reduced,
}

impl MotionPace {
    /// The pace a setting asks for, where `system` is what the platform says.
    pub(super) fn resolve(preference: MotionPreference, system: Self) -> Self {
        match preference {
            MotionPreference::System => system,
            MotionPreference::Full => Self::Full,
            MotionPreference::Reduced => Self::Reduced,
        }
    }

    /// What the platform's accessibility setting says, where one can be read: GNOME's
    /// `enable-animations` and macOS's Reduce Motion. GPUI exposes neither, and each
    /// takes a process to ask, so this blocks and belongs off the UI thread. Anything
    /// that cannot answer is taken as no preference.
    pub(super) fn from_system() -> Self {
        let read = |program: &str, args: &[&str]| {
            std::process::Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        if cfg!(target_os = "macos") {
            read(
                "defaults",
                &["read", "com.apple.universalaccess", "reduceMotion"],
            )
            .map_or(Self::Full, |answer| Self::from_macos_setting(&answer))
        } else if cfg!(target_os = "linux") {
            read(
                "gsettings",
                &["get", "org.gnome.desktop.interface", "enable-animations"],
            )
            .map_or(Self::Full, |answer| Self::from_gnome_setting(&answer))
        } else {
            Self::Full
        }
    }

    fn from_gnome_setting(enable_animations: &str) -> Self {
        match enable_animations.trim() {
            "false" => Self::Reduced,
            _ => Self::Full,
        }
    }

    fn from_macos_setting(reduce_motion: &str) -> Self {
        match reduce_motion.trim() {
            "1" => Self::Reduced,
            _ => Self::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_system_setting_defers_to_the_platform() {
        for system in [MotionPace::Full, MotionPace::Reduced] {
            assert_eq!(
                MotionPace::resolve(MotionPreference::System, system),
                system
            );
            assert_eq!(
                MotionPace::resolve(MotionPreference::Full, system),
                MotionPace::Full
            );
            assert_eq!(
                MotionPace::resolve(MotionPreference::Reduced, system),
                MotionPace::Reduced
            );
        }
    }

    #[test]
    fn the_platform_only_reduces_motion_when_it_clearly_says_so() {
        assert_eq!(
            MotionPace::from_gnome_setting("false\n"),
            MotionPace::Reduced
        );
        assert_eq!(MotionPace::from_gnome_setting("true\n"), MotionPace::Full);
        assert_eq!(MotionPace::from_macos_setting("1\n"), MotionPace::Reduced);
        assert_eq!(MotionPace::from_macos_setting("0\n"), MotionPace::Full);
        assert_eq!(MotionPace::from_gnome_setting(""), MotionPace::Full);
        assert_eq!(MotionPace::from_macos_setting("???"), MotionPace::Full);
    }
}
