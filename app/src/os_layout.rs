//! Keyboard layout the operating system types with, to draw the symbols that
//! Shift gives (`;` → `:` on a US layout, `,` → `;` on an Italian one).
//! Only Italian is told apart from the rest (US).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Asking the system costs a process on Linux and macOS: not at every frame.
const REFRESH: Duration = Duration::from_secs(5);

static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// Does the system type with an Italian keyboard layout?
pub fn italian() -> bool {
    let Ok(mut cache) = CACHE.lock() else { return false };
    match *cache {
        Some((t, v)) if t.elapsed() < REFRESH => v,
        _ => {
            let v = detect();
            *cache = Some((Instant::now(), v));
            v
        }
    }
}

/// First layout of an XKB list ("it", "us,it", "it(winkeys)") is the Italian one.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn xkb_is_italian(layouts: &str) -> bool {
    let first = layouts.trim().trim_matches('"').split(',').next().unwrap_or("");
    first.trim().split('(').next().is_some_and(|l| l.eq_ignore_ascii_case("it"))
}

#[cfg(windows)]
fn detect() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // the layout of the window being typed in, not the HUD's own
    let hkl = unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
        GetKeyboardLayout(thread)
    };
    // low word: language id, primary language 0x10 = Italian
    (hkl as usize & 0x3FF) == 0x10
}

#[cfg(target_os = "macos")]
fn detect() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.HIToolbox", "AppleCurrentKeyboardLayoutInputSourceID"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Italian"))
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn detect() -> bool {
    use std::process::Command;
    let xkb = Command::new("setxkbmap").arg("-query").output().ok().filter(|o| o.status.success()).and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.lines().find_map(|l| l.strip_prefix("layout:").map(xkb_is_italian))
    });
    if let Some(v) = xkb {
        return v;
    }
    if let Ok(v) = std::env::var("XKB_DEFAULT_LAYOUT") {
        return xkb_is_italian(&v);
    }
    if let Ok(text) = std::fs::read_to_string("/etc/default/keyboard") {
        if let Some(v) = text.lines().find_map(|l| l.strip_prefix("XKBLAYOUT=")) {
            return xkb_is_italian(v);
        }
    }
    // last resort: the language of the session
    crate::i18n::system() == crate::i18n::Lang::It
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkb_layout_lists() {
        assert!(xkb_is_italian("     it"));
        assert!(xkb_is_italian("\"it,us\""));
        assert!(xkb_is_italian("it(winkeys)"));
        assert!(!xkb_is_italian("us,it"));
        assert!(!xkb_is_italian("gb"));
    }
}
