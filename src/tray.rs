const ICON_SIZE: u32 = 32;
const POLL_INTERVAL_MILLIS: u32 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Connected(String),
    Recording(String),
}

impl Status {
    fn text(&self) -> String {
        match self {
            Status::Waiting => "Waiting for device".to_string(),
            Status::Connected(device) => format!("Connected: {device}"),
            Status::Recording(device) => format!("Recording: {device}"),
        }
    }

    fn color(&self) -> [u8; 3] {
        match self {
            Status::Waiting => [136, 136, 136],
            Status::Connected(_) => [46, 160, 67],
            Status::Recording(_) => [217, 45, 32],
        }
    }
}

/// Runs the tray on the calling thread until the user quits or `statuses` disconnects, which means
/// the recording thread died. Leaving then makes the icon vanish rather than claim to be recording.
pub fn run(statuses: &std::sync::mpsc::Receiver<Status>, output_dir: &std::path::Path) {
    let status_item = tray_icon::menu::MenuItem::new(Status::Waiting.text(), false, None);
    let open_item = tray_icon::menu::MenuItem::new("Open recordings folder", true, None);
    let autostart_item = tray_icon::menu::CheckMenuItem::new(
        "Start at logon",
        true,
        crate::autostart::is_enabled(),
        None,
    );
    let quit_item = tray_icon::menu::MenuItem::new("Quit", true, None);

    let menu = tray_icon::menu::Menu::new();
    let appended = menu.append_items(&[
        &status_item,
        &tray_icon::menu::PredefinedMenuItem::separator(),
        &open_item,
        &autostart_item,
        &tray_icon::menu::PredefinedMenuItem::separator(),
        &quit_item,
    ]);
    if let Err(e) = appended {
        auto_midi_recorder::diag::log(&format!("cannot build tray menu: {e}"));
    }

    // Recording matters more than the icon, so a tray failure is only logged.
    let mut tray = match tray_icon::TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip(&Status::Waiting))
        .with_icon(icon(&Status::Waiting))
        .build()
    {
        Ok(tray) => Some(tray),
        Err(e) => {
            auto_midi_recorder::diag::log(&format!("cannot create tray icon: {e}"));
            None
        }
    };

    // SAFETY: a null window with no callback only makes the system post WM_TIMER to this thread.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
            std::ptr::null_mut(),
            0,
            POLL_INTERVAL_MILLIS,
            None,
        );
    }

    // The tray's hidden window only works if this thread pumps messages; the timer wakes the pump
    // so that status changes get picked up.
    while pump_message() {
        for event in tray_icon::menu::MenuEvent::receiver().try_iter() {
            if event.id == quit_item.id() {
                return;
            } else if event.id == open_item.id() {
                open_folder(output_dir);
            } else if event.id == autostart_item.id() {
                toggle_autostart(&autostart_item);
            }
        }

        loop {
            match statuses.try_recv() {
                Ok(status) => {
                    status_item.set_text(status.text());
                    if let Some(tray) = tray.as_mut() {
                        update_tray(tray, &status);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }
    }
}

fn pump_message() -> bool {
    // SAFETY: MSG is plain data for which all-zero is valid, and it outlives each call using it.
    unsafe {
        let mut message: windows_sys::Win32::UI::WindowsAndMessaging::MSG = std::mem::zeroed();
        if windows_sys::Win32::UI::WindowsAndMessaging::GetMessageW(
            &mut message,
            std::ptr::null_mut(),
            0,
            0,
        ) <= 0
        {
            return false;
        }
        windows_sys::Win32::UI::WindowsAndMessaging::TranslateMessage(&message);
        windows_sys::Win32::UI::WindowsAndMessaging::DispatchMessageW(&message);
    }
    true
}

fn update_tray(tray: &mut tray_icon::TrayIcon, status: &Status) {
    if let Err(e) = tray.set_icon(Some(icon(status))) {
        auto_midi_recorder::diag::log(&format!("cannot update tray icon: {e}"));
    }
    if let Err(e) = tray.set_tooltip(Some(tooltip(status))) {
        auto_midi_recorder::diag::log(&format!("cannot update tray tooltip: {e}"));
    }
}

fn tooltip(status: &Status) -> String {
    format!("auto-midi-recorder\n{}", status.text())
}

fn icon(status: &Status) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(circle_rgba(status.color()), ICON_SIZE, ICON_SIZE)
        .expect("buffer size matches the icon dimensions")
}

fn circle_rgba([red, green, blue]: [u8; 3]) -> Vec<u8> {
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;
    let radius = ICON_SIZE as f32 / 2.0 - 2.0;
    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let distance = (x as f32 - center).hypot(y as f32 - center);
            // A one-pixel alpha ramp keeps the edge from looking jagged at small tray sizes.
            let alpha = (radius - distance + 0.5).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[red, green, blue, (alpha * 255.0) as u8]);
        }
    }
    rgba
}

fn open_folder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("explorer").arg(path).spawn() {
        auto_midi_recorder::diag::log(&format!("cannot open {}: {e}", path.display()));
    }
}

fn toggle_autostart(item: &tray_icon::menu::CheckMenuItem) {
    let enable = !crate::autostart::is_enabled();
    if let Err(e) = crate::autostart::set_enabled(enable) {
        auto_midi_recorder::diag::log(&format!("cannot change start at logon: {e}"));
    }
    // The menu flips its own check on click, which may not match what the registry now holds.
    item.set_checked(crate::autostart::is_enabled());
}
