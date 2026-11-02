//! Converts a session log to a Standard MIDI File.
//!
//! Timing is 480 PPQ at 480000 us per quarter, so one tick is exactly 1 ms. A consumer can recover
//! each event's absolute time as `session_start_utc` (in the Text meta event) + ticks.

const TICKS_PER_QUARTER: u16 = 480;
const MICROS_PER_QUARTER: u32 = 480_000;
const MICROS_PER_TICK: u64 = 1_000;

pub fn write(log: &crate::raw_log::RawLog, path: &std::path::Path) -> std::io::Result<()> {
    let bytes = encode(log)?;
    // A crash during the write must never leave a truncated .mid looking like a finished one.
    let temp_path = path.with_extension("mid.tmp");
    std::fs::write(&temp_path, bytes)?;
    std::fs::rename(&temp_path, path)
}

fn encode(log: &crate::raw_log::RawLog) -> std::io::Result<Vec<u8>> {
    let arena = midly::Arena::new();
    let start_micros = log.events.first().map_or(0, |event| event.unix_micros);
    let session_start = format!("session_start_utc={}", format_utc(start_micros));

    let meta = |message| midly::TrackEvent {
        delta: 0.into(),
        kind: midly::TrackEventKind::Meta(message),
    };
    let mut track = vec![
        meta(midly::MetaMessage::TrackName(log.device.as_bytes())),
        meta(midly::MetaMessage::Text(session_start.as_bytes())),
        meta(midly::MetaMessage::Tempo(midly::num::u24::new(
            MICROS_PER_QUARTER,
        ))),
    ];

    let mut previous_tick = 0;
    for event in &log.events {
        // Deltas come from absolute ticks so sub-millisecond rounding can't accumulate. The clock
        // may also step backwards (time sync), and a delta can't be negative.
        let tick =
            (event.unix_micros.saturating_sub(start_micros) / MICROS_PER_TICK).max(previous_tick);
        track.push(midly::TrackEvent {
            delta: delta_from_ticks(tick - previous_tick),
            kind: track_event_kind(&event.bytes, &arena),
        });
        previous_tick = tick;
    }
    track.push(meta(midly::MetaMessage::EndOfTrack));

    let mut smf = midly::Smf::new(midly::Header::new(
        midly::Format::SingleTrack,
        midly::Timing::Metrical(midly::num::u15::new(TICKS_PER_QUARTER)),
    ));
    smf.tracks.push(track);

    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)?;
    Ok(bytes)
}

fn track_event_kind<'a>(bytes: &'a [u8], arena: &'a midly::Arena) -> midly::TrackEventKind<'a> {
    match midly::live::LiveEvent::parse(bytes) {
        Ok(event) => event.as_track_event(arena),
        // Escape holds arbitrary bytes (e.g. a SysEx split across chunks), so nothing is lost.
        Err(_) => midly::TrackEventKind::Escape(bytes),
    }
}

fn delta_from_ticks(ticks: u64) -> midly::num::u28 {
    let max = u64::from(midly::num::u28::max_value().as_int());
    midly::num::u28::new(ticks.min(max) as u32)
}

fn format_utc(unix_micros: u64) -> String {
    i64::try_from(unix_micros)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_micros)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[cfg(test)]
mod tests {
    fn log(events: &[(u64, &[u8])]) -> crate::raw_log::RawLog {
        crate::raw_log::RawLog {
            device: "FP-90X".to_string(),
            events: events
                .iter()
                .map(|(unix_micros, bytes)| crate::raw_log::Event {
                    unix_micros: *unix_micros,
                    bytes: bytes.to_vec(),
                })
                .collect(),
            bad_lines: 0,
        }
    }

    fn deltas(smf: &midly::Smf) -> Vec<u32> {
        smf.tracks[0]
            .iter()
            .filter(|event| !matches!(event.kind, midly::TrackEventKind::Meta(_)))
            .map(|event| event.delta.as_int())
            .collect()
    }

    #[test]
    fn one_tick_is_one_millisecond() {
        let bytes = crate::midi_file::encode(&log(&[(0, &[0x90, 60, 64])])).unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();

        assert_eq!(
            smf.header.timing,
            midly::Timing::Metrical(midly::num::u15::new(480))
        );
        assert!(smf.tracks[0].iter().any(|event| matches!(
            event.kind,
            midly::TrackEventKind::Meta(midly::MetaMessage::Tempo(tempo)) if tempo.as_int() == 480_000
        )));
    }

    #[test]
    fn session_start_and_device_are_stored_as_meta() {
        let bytes =
            crate::midi_file::encode(&log(&[(1_793_620_092_123_456, &[0x90, 60, 64])])).unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();

        let kinds: Vec<_> = smf.tracks[0].iter().map(|event| event.kind).collect();
        assert!(
            kinds.contains(&midly::TrackEventKind::Meta(midly::MetaMessage::TrackName(
                b"FP-90X"
            )))
        );
        assert!(
            kinds.contains(&midly::TrackEventKind::Meta(midly::MetaMessage::Text(
                b"session_start_utc=2026-11-02T11:48:12.123456Z"
            )))
        );
    }

    #[test]
    fn deltas_are_derived_from_absolute_time_without_accumulating_rounding() {
        let note: &[u8] = &[0x90, 60, 64];
        let bytes = crate::midi_file::encode(&log(&[
            (1_000_000, note),
            (1_001_900, note),
            (1_003_800, note),
        ]))
        .unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();

        assert_eq!(deltas(&smf), vec![0, 1, 2]);
    }

    #[test]
    fn clock_stepping_backwards_yields_zero_delta() {
        let note: &[u8] = &[0x90, 60, 64];
        let bytes = crate::midi_file::encode(&log(&[
            (10_000, note),
            (15_000, note),
            (12_000, note),
            (16_000, note),
        ]))
        .unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();

        assert_eq!(deltas(&smf), vec![0, 5, 0, 1]);
    }

    #[test]
    fn channel_sysex_realtime_and_garbage_all_survive() {
        let sysex: &[u8] = &[0xF0, 0x41, 0x10, 0xF7];
        let split_sysex: &[u8] = &[0xF0, 0x41, 0x10];
        let bytes = crate::midi_file::encode(&log(&[
            (0, &[0xB0, 64, 127]),
            (1_000, sysex),
            (2_000, &[0xFA]),
            (3_000, split_sysex),
        ]))
        .unwrap();
        let smf = midly::Smf::parse(&bytes).unwrap();

        let kinds: Vec<_> = smf.tracks[0]
            .iter()
            .map(|event| event.kind)
            .filter(|kind| !matches!(kind, midly::TrackEventKind::Meta(_)))
            .collect();
        assert_eq!(kinds.len(), 4);
        assert!(matches!(kinds[0], midly::TrackEventKind::Midi { .. }));
        assert_eq!(kinds[1], midly::TrackEventKind::SysEx(&[0x41, 0x10, 0xF7]));
        assert_eq!(kinds[2], midly::TrackEventKind::Escape(&[0xFA]));
        assert!(matches!(
            kinds[3],
            midly::TrackEventKind::Escape(_) | midly::TrackEventKind::SysEx(_)
        ));
    }
}
