//! Crash-safe session log. One line per event: `<unix micros> <hex bytes>`.

const DEVICE_PREFIX: &str = "# device: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub unix_micros: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RawLog {
    pub device: String,
    pub events: Vec<Event>,
    pub bad_lines: usize,
}

pub fn device_line(device: &str) -> String {
    format!("{DEVICE_PREFIX}{device}\n")
}

pub fn event_line(event: &Event) -> String {
    let mut line = event.unix_micros.to_string();
    for byte in &event.bytes {
        line.push_str(&format!(" {byte:02X}"));
    }
    line.push('\n');
    line
}

pub fn parse(content: &[u8]) -> RawLog {
    // A crash or power loss can leave garbage at the tail, which must not cost us the whole log.
    let text = String::from_utf8_lossy(content);
    let mut log = RawLog::default();

    for line in text.split_inclusive('\n') {
        // Without its newline a line may have been cut mid-byte ("90 3C 4"), so it can't be trusted.
        let Some(line) = line.strip_suffix('\n') else {
            continue;
        };
        if let Some(device) = line.strip_prefix(DEVICE_PREFIX) {
            log.device = device.to_string();
        } else if let Some(event) = parse_event(line) {
            log.events.push(event);
        } else {
            log.bad_lines += 1;
        }
    }
    log
}

fn parse_event(line: &str) -> Option<Event> {
    let mut fields = line.split(' ');
    let unix_micros = fields.next()?.parse().ok()?;
    let bytes = fields
        .map(|field| u8::from_str_radix(field, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    if bytes.is_empty() {
        return None;
    }
    Some(Event { unix_micros, bytes })
}

#[cfg(test)]
mod tests {
    fn event(unix_micros: u64, bytes: &[u8]) -> crate::raw_log::Event {
        crate::raw_log::Event {
            unix_micros,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn lines_round_trip() {
        let note = event(1_793_620_092_123_456, &[0x90, 0x3C, 0x40]);
        let mut content = crate::raw_log::device_line("FP-90X");
        content.push_str(&crate::raw_log::event_line(&note));

        let log = crate::raw_log::parse(content.as_bytes());

        assert_eq!(log.device, "FP-90X");
        assert_eq!(log.events, vec![note]);
        assert_eq!(log.bad_lines, 0);
    }

    #[test]
    fn event_line_is_time_then_hex() {
        assert_eq!(
            crate::raw_log::event_line(&event(12, &[0x90, 0x0A])),
            "12 90 0A\n"
        );
    }

    #[test]
    fn torn_last_line_is_dropped_silently() {
        let log = crate::raw_log::parse(b"1 90 3C 40\n2 90 3C 4");

        assert_eq!(log.events, vec![event(1, &[0x90, 0x3C, 0x40])]);
        assert_eq!(log.bad_lines, 0);
    }

    #[test]
    fn corrupt_lines_are_counted_and_later_events_survive() {
        let log = crate::raw_log::parse(b"1 90 3C 40\n\0\0\xFF\n3\n4 80 3C 00\n");

        assert_eq!(
            log.events,
            vec![event(1, &[0x90, 0x3C, 0x40]), event(4, &[0x80, 0x3C, 0x00])]
        );
        assert_eq!(log.bad_lines, 2);
    }
}
