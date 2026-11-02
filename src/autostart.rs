//! Per-user Run key: unlike a scheduled task, toggling it needs no administrator rights.

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "auto-midi-recorder";

/// A value pointing at another copy of the exe counts as off, so that ticking the menu item
/// repoints it at the copy the user is actually running.
pub fn is_enabled() -> bool {
    let Ok(key) = run_key() else {
        return false;
    };
    let Ok(command) = command() else {
        return false;
    };
    key.get_value::<String, _>(VALUE_NAME)
        .is_ok_and(|value| value == command)
}

pub fn set_enabled(enabled: bool) -> std::io::Result<()> {
    let key = run_key()?;
    if enabled {
        key.set_value(VALUE_NAME, &command()?)
    } else {
        match key.delete_value(VALUE_NAME) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

fn run_key() -> std::io::Result<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey_with_flags(
        RUN_KEY,
        winreg::enums::KEY_READ | winreg::enums::KEY_SET_VALUE,
    )
}

fn command() -> std::io::Result<String> {
    Ok(format!("\"{}\"", std::env::current_exe()?.display()))
}
