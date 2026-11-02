#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod autostart;
#[cfg(windows)]
mod tray;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    crate::app::main()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("auto-midi-recorder only runs on Windows");
    std::process::ExitCode::FAILURE
}
