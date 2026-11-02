const TICK: std::time::Duration = std::time::Duration::from_secs(1);
const PORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const CLIENT_NAME: &str = "auto-midi-recorder";

/// The loop wakes every TICK, so a gap this long means the PC slept. USB devices usually
/// re-enumerate during sleep: the port name is listed again by the first poll while our handle is
/// already dead, so the name check alone would keep it and silently record nothing.
const RESUME_GAP_MICROS: u64 = 30_000_000;

struct Connection {
    port_name: String,
    // Closes the port on drop.
    _input: midir::MidiInputConnection<()>,
}

pub fn main() -> std::process::ExitCode {
    let Ok(exe_path) = std::env::current_exe() else {
        return std::process::ExitCode::FAILURE;
    };
    if auto_midi_recorder::diag::init(&exe_path.with_file_name("auto-midi-recorder.log")).is_err() {
        return std::process::ExitCode::FAILURE;
    }

    // A panic would otherwise end the logger with no trace, as there is no console to print to.
    std::panic::set_hook(Box::new(|info| {
        auto_midi_recorder::diag::log(&format!("panic: {info}"));
    }));

    match run(&exe_path.with_file_name("config.toml")) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            auto_midi_recorder::diag::log(&format!("fatal: {message}"));
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(config_path: &std::path::Path) -> Result<(), String> {
    let config = auto_midi_recorder::config::Config::load(config_path)?;
    std::fs::create_dir_all(&config.output_dir)
        .map_err(|e| format!("cannot create {}: {e}", config.output_dir.display()))?;

    // Without this, a second instance would "recover" the log the first one is still writing.
    let lock_path = config.output_dir.join(".lock");
    let lock = std::fs::File::create(&lock_path)
        .map_err(|e| format!("cannot create {}: {e}", lock_path.display()))?;
    if lock.try_lock().is_err() {
        auto_midi_recorder::diag::log("another instance is already running; exiting");
        return Ok(());
    }

    auto_midi_recorder::diag::log("started");
    recover_leftovers(&config.output_dir)?;

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (status_sender, statuses) = std::sync::mpsc::channel();
    let output_dir = config.output_dir.clone();
    // The tray has to stay on the thread that created it, so recording is what moves off main.
    let recording = std::thread::spawn({
        let stop = stop.clone();
        move || record_until_stopped(&config, &stop, &status_sender)
    });

    crate::tray::run(&statuses, &output_dir);

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // Waiting lets an open session finish into a .mid instead of needing recovery at next start.
    let _ = recording.join();
    auto_midi_recorder::diag::log("stopped");
    Ok(())
}

fn recover_leftovers(output_dir: &std::path::Path) -> Result<(), String> {
    let outcomes = auto_midi_recorder::recorder::recover(output_dir)
        .map_err(|e| format!("cannot scan {}: {e}", output_dir.display()))?;
    for outcome in outcomes {
        match outcome.result {
            Ok(_) => {
                auto_midi_recorder::diag::log(&format!("recovered {}", outcome.log_path.display()))
            }
            Err(e) => auto_midi_recorder::diag::log(&format!(
                "cannot recover {}: {e}",
                outcome.log_path.display()
            )),
        }
    }
    Ok(())
}

fn record_until_stopped(
    config: &auto_midi_recorder::config::Config,
    stop: &std::sync::atomic::AtomicBool,
    statuses: &std::sync::mpsc::Sender<crate::tray::Status>,
) {
    let (sender, receiver) = std::sync::mpsc::channel::<auto_midi_recorder::raw_log::Event>();
    let mut recorder = auto_midi_recorder::recorder::Recorder::new(
        config.output_dir.clone(),
        config.idle_timeout_micros(),
    );
    let mut connection: Option<Connection> = None;
    let mut connect_failure_logged = false;
    let mut last_wake_micros = now_micros();
    let mut last_sync = std::time::Instant::now();
    let mut last_poll: Option<std::time::Instant> = None;

    let mut last_status = crate::tray::Status::Waiting;

    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let received = receiver.recv_timeout(TICK).ok();
        let now = now_micros();

        // Before recording, so that an event after long silence lands in a new file.
        report_close(recorder.close_if_idle(now));
        if let Some(event) = received {
            let device = connection.as_ref().map_or("", |c| c.port_name.as_str());
            record(&mut recorder, device, &event);
        }

        if now.saturating_sub(last_wake_micros) >= RESUME_GAP_MICROS {
            auto_midi_recorder::diag::log("wake from sleep detected; reconnecting");
            disconnect(&mut connection, &mut recorder, &receiver);
            last_poll = None;
        }
        last_wake_micros = now;

        if last_sync.elapsed() >= TICK {
            if let Err(e) = recorder.sync() {
                auto_midi_recorder::diag::log(&format!("cannot sync session log: {e}"));
            }
            last_sync = std::time::Instant::now();
        }

        if last_poll.is_none_or(|at| at.elapsed() >= PORT_POLL_INTERVAL) {
            last_poll = Some(std::time::Instant::now());
            if connection
                .as_ref()
                .is_some_and(|connection| !port_is_listed(&connection.port_name))
            {
                disconnect(&mut connection, &mut recorder, &receiver);
            }
            if connection.is_none() {
                match connect(config, &sender) {
                    Ok(Some(new_connection)) => {
                        auto_midi_recorder::diag::log(&format!(
                            "connected to {}",
                            new_connection.port_name
                        ));
                        connection = Some(new_connection);
                        connect_failure_logged = false;
                    }
                    Ok(None) => connect_failure_logged = false,
                    Err(message) => {
                        // Retried every poll; once per outage is enough for the log.
                        if !connect_failure_logged {
                            auto_midi_recorder::diag::log(&message);
                            connect_failure_logged = true;
                        }
                    }
                }
            }
        }

        let status = current_status(connection.as_ref(), &recorder);
        if status != last_status {
            let _ = statuses.send(status.clone());
            last_status = status;
        }
    }

    disconnect(&mut connection, &mut recorder, &receiver);
}

fn current_status(
    connection: Option<&Connection>,
    recorder: &auto_midi_recorder::recorder::Recorder,
) -> crate::tray::Status {
    match connection {
        None => crate::tray::Status::Waiting,
        Some(connection) if recorder.is_recording() => {
            crate::tray::Status::Recording(connection.port_name.clone())
        }
        Some(connection) => crate::tray::Status::Connected(connection.port_name.clone()),
    }
}

fn record(
    recorder: &mut auto_midi_recorder::recorder::Recorder,
    device: &str,
    event: &auto_midi_recorder::raw_log::Event,
) {
    if let Err(e) = recorder.record(device, event) {
        auto_midi_recorder::diag::log(&format!("cannot write event: {e}"));
    }
}

fn disconnect(
    connection: &mut Option<Connection>,
    recorder: &mut auto_midi_recorder::recorder::Recorder,
    receiver: &std::sync::mpsc::Receiver<auto_midi_recorder::raw_log::Event>,
) {
    let Some(closed) = connection.take() else {
        return;
    };
    let port_name = closed.port_name.clone();
    drop(closed);
    auto_midi_recorder::diag::log(&format!("disconnected from {port_name}"));

    for event in receiver.try_iter() {
        record(recorder, &port_name, &event);
    }
    report_close(recorder.close());
}

fn report_close(result: std::io::Result<Option<std::path::PathBuf>>) {
    match result {
        Ok(Some(path)) => auto_midi_recorder::diag::log(&format!("saved {}", path.display())),
        Ok(None) => {}
        Err(e) => auto_midi_recorder::diag::log(&format!(
            "cannot finish session, will retry on next start: {e}"
        )),
    }
}

fn port_is_listed(port_name: &str) -> bool {
    let Ok(input) = midir::MidiInput::new(CLIENT_NAME) else {
        // Can't tell, and dropping a working connection would split the session for nothing.
        return true;
    };
    input
        .ports()
        .iter()
        .any(|port| input.port_name(port).is_ok_and(|name| name == port_name))
}

fn connect(
    config: &auto_midi_recorder::config::Config,
    sender: &std::sync::mpsc::Sender<auto_midi_recorder::raw_log::Event>,
) -> Result<Option<Connection>, String> {
    let mut input = midir::MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
    // Roland pianos can send active sensing several times a second even when idle. It carries no
    // performance data and would keep the idle timeout from ever firing.
    input.ignore(midir::Ignore::TimeAndActiveSense);

    let matched = input.ports().into_iter().find_map(|port| {
        let name = input.port_name(&port).ok()?;
        config.matches_port(&name).then_some((port, name))
    });
    let Some((port, port_name)) = matched else {
        return Ok(None);
    };

    let sender = sender.clone();
    let connection = input
        .connect(
            &port,
            CLIENT_NAME,
            // Runs on the driver's callback thread, where blocking on disk I/O would delay input.
            move |_, bytes, _| {
                if !bytes.is_empty() {
                    let _ = sender.send(auto_midi_recorder::raw_log::Event {
                        unix_micros: now_micros(),
                        bytes: bytes.to_vec(),
                    });
                }
            },
            (),
        )
        .map_err(|e| format!("cannot open {port_name}: {e}"))?;
    Ok(Some(Connection {
        port_name,
        _input: connection,
    }))
}

/// Wall clock rather than midir's timestamp, which only counts milliseconds from port open.
fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_micros() as u64)
}
