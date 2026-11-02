static LOG_FILE: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// The exe has no console, so this file is the only place failures can be seen.
pub fn init(path: &std::path::Path) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let _ = LOG_FILE.set(file);
    Ok(())
}

pub fn log(message: &str) {
    use std::io::Write;

    if let Some(mut file) = LOG_FILE.get() {
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%z");
        let _ = writeln!(file, "{now} {message}");
    }
}
