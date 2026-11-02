const LOG_EXTENSION: &str = "midilog";

struct Session {
    log_path: std::path::PathBuf,
    file: std::fs::File,
    last_event_micros: u64,
    unsynced: bool,
}

pub struct Recorder {
    output_dir: std::path::PathBuf,
    idle_timeout_micros: u64,
    session: Option<Session>,
}

impl Recorder {
    pub fn new(output_dir: std::path::PathBuf, idle_timeout_micros: u64) -> Self {
        Self {
            output_dir,
            idle_timeout_micros,
            session: None,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.session.is_some()
    }

    /// The session opens on the first event rather than on connect, so powering the piano on
    /// without playing leaves no file behind.
    pub fn record(&mut self, device: &str, event: &crate::raw_log::Event) -> std::io::Result<()> {
        use std::io::Write;

        if self.session.is_none() {
            self.session = Some(self.open_session(device, event.unix_micros)?);
        }
        let session = self.session.as_mut().expect("session was just opened");
        // Unbuffered on purpose: once write_all returns, the line survives a process crash.
        session
            .file
            .write_all(crate::raw_log::event_line(event).as_bytes())?;
        session.last_event_micros = event.unix_micros;
        session.unsynced = true;
        Ok(())
    }

    fn open_session(&self, device: &str, unix_micros: u64) -> std::io::Result<Session> {
        use std::io::Write;

        let log_path = self
            .output_dir
            .join(format!("{}.{LOG_EXTENSION}", session_stem(unix_micros)));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        file.write_all(crate::raw_log::device_line(device).as_bytes())?;
        Ok(Session {
            log_path,
            file,
            last_event_micros: unix_micros,
            unsynced: true,
        })
    }

    /// Pushes written lines to disk so they also survive power loss. Too slow to do per event.
    pub fn sync(&mut self) -> std::io::Result<()> {
        if let Some(session) = self.session.as_mut().filter(|session| session.unsynced) {
            session.file.sync_data()?;
            session.unsynced = false;
        }
        Ok(())
    }

    pub fn close_if_idle(
        &mut self,
        now_micros: u64,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        let idle = self.session.as_ref().is_some_and(|session| {
            now_micros.saturating_sub(session.last_event_micros) >= self.idle_timeout_micros
        });
        if idle { self.close() } else { Ok(None) }
    }

    /// On failure the log stays on disk, and the next startup's `recover` retries it.
    pub fn close(&mut self) -> std::io::Result<Option<std::path::PathBuf>> {
        match self.session.take() {
            Some(session) => {
                let Session { log_path, file, .. } = session;
                drop(file);
                convert(&log_path)
            }
            None => Ok(None),
        }
    }
}

/// Finishes logs left behind by a crash or an abrupt shutdown. Must only run while no other
/// instance is recording into `output_dir`.
pub fn recover(output_dir: &std::path::Path) -> std::io::Result<Vec<RecoveryOutcome>> {
    let mut outcomes = Vec::new();
    for entry in std::fs::read_dir(output_dir)? {
        let log_path = entry?.path();
        if log_path.extension().is_some_and(|ext| ext == LOG_EXTENSION) {
            let result = convert(&log_path);
            outcomes.push(RecoveryOutcome { log_path, result });
        }
    }
    Ok(outcomes)
}

pub struct RecoveryOutcome {
    pub log_path: std::path::PathBuf,
    pub result: std::io::Result<Option<std::path::PathBuf>>,
}

fn convert(log_path: &std::path::Path) -> std::io::Result<Option<std::path::PathBuf>> {
    let log = crate::raw_log::parse(&std::fs::read(log_path)?);
    if log.bad_lines > 0 {
        crate::diag::log(&format!(
            "skipped {} corrupt lines in {}",
            log.bad_lines,
            log_path.display()
        ));
    }

    let mid_path = if log.events.is_empty() {
        None
    } else {
        let mid_path = log_path.with_extension("mid");
        crate::midi_file::write(&log, &mid_path)?;
        Some(mid_path)
    };
    std::fs::remove_file(log_path)?;
    Ok(mid_path)
}

/// Local time for browsing, with the offset so that names stay unique and unambiguous across DST.
fn session_stem(unix_micros: u64) -> String {
    i64::try_from(unix_micros)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_micros)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%dT%H%M%S%z")
        .to_string()
}

#[cfg(test)]
mod tests {
    const IDLE_TIMEOUT: u64 = 1_000_000;

    fn event(unix_micros: u64) -> crate::raw_log::Event {
        crate::raw_log::Event {
            unix_micros,
            bytes: vec![0x90, 60, 64],
        }
    }

    fn recorder(dir: &tempfile::TempDir) -> crate::recorder::Recorder {
        crate::recorder::Recorder::new(dir.path().to_path_buf(), IDLE_TIMEOUT)
    }

    fn files_with_extension(dir: &tempfile::TempDir, extension: &str) -> usize {
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == extension)
            })
            .count()
    }

    fn note_count(path: &std::path::Path) -> usize {
        let bytes = std::fs::read(path).unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();
        smf.tracks[0]
            .iter()
            .filter(|event| matches!(event.kind, midly::TrackEventKind::Midi { .. }))
            .count()
    }

    #[test]
    fn no_events_means_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);

        assert!(recorder.close().unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn close_converts_log_to_mid_and_removes_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);
        recorder.record("FP-90X", &event(1_000)).unwrap();
        recorder.record("FP-90X", &event(2_000)).unwrap();
        assert_eq!(files_with_extension(&dir, "midilog"), 1);

        let mid_path = recorder.close().unwrap().unwrap();

        assert_eq!(note_count(&mid_path), 2);
        assert_eq!(files_with_extension(&dir, "midilog"), 0);
    }

    #[test]
    fn idle_timeout_closes_and_next_event_starts_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);
        recorder.record("FP-90X", &event(0)).unwrap();

        assert!(recorder.close_if_idle(IDLE_TIMEOUT - 1).unwrap().is_none());
        let first = recorder.close_if_idle(IDLE_TIMEOUT).unwrap().unwrap();

        recorder.record("FP-90X", &event(5_000_000)).unwrap();
        let second = recorder.close().unwrap().unwrap();

        assert_ne!(first, second);
        assert_eq!(files_with_extension(&dir, "mid"), 2);
    }

    #[test]
    fn is_recording_only_while_a_session_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);
        assert!(!recorder.is_recording());

        recorder.record("FP-90X", &event(1_000)).unwrap();
        assert!(recorder.is_recording());

        recorder.close().unwrap();
        assert!(!recorder.is_recording());
    }

    #[test]
    fn events_keep_a_session_alive() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);
        recorder.record("FP-90X", &event(0)).unwrap();
        recorder.record("FP-90X", &event(IDLE_TIMEOUT - 1)).unwrap();

        assert!(recorder.close_if_idle(IDLE_TIMEOUT).unwrap().is_none());
    }

    #[test]
    fn log_written_before_a_crash_is_recovered_on_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut crashed = recorder(&dir);
        crashed.record("FP-90X", &event(1_000)).unwrap();
        crashed.record("FP-90X", &event(2_000)).unwrap();
        // Dropping without close leaves the log behind, the same as a killed process.
        drop(crashed);

        let outcomes = crate::recorder::recover(dir.path()).unwrap();

        assert_eq!(outcomes.len(), 1);
        let mid_path = outcomes[0].result.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(note_count(mid_path), 2);
        assert_eq!(files_with_extension(&dir, "midilog"), 0);
    }

    #[test]
    fn recovering_a_log_without_events_removes_it_without_a_mid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.midilog"), "# device: FP-90X\n").unwrap();

        crate::recorder::recover(dir.path()).unwrap();

        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn recovery_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auto-midi-recorder.log"), "keep\n").unwrap();

        let outcomes = crate::recorder::recover(dir.path()).unwrap();

        assert!(outcomes.is_empty());
        assert!(dir.path().join("auto-midi-recorder.log").exists());
    }

    #[test]
    fn sync_succeeds_with_and_without_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = recorder(&dir);
        recorder.sync().unwrap();
        recorder.record("FP-90X", &event(1_000)).unwrap();
        recorder.sync().unwrap();
    }
}
