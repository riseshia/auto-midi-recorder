# auto-midi-recorder

A background logger for Windows that records every MIDI event from a digital piano, so you never
have to remember to press record. It has no window and no console; it just runs from logon.

Recordings are whole sessions with absolute timestamps. Splitting them into takes or matching them
to pieces is left to whatever analyzes the files later.

## How it works

- **A session is one file.** It starts at the first event after the piano connects, and ends when
  the piano disconnects (powered off) or after `idle_timeout_minutes` of silence. Powering on without
  playing creates nothing.
- **Reconnects by itself.** The port list is checked every 3 seconds, and after the PC wakes from
  sleep the port is reopened.
- **Survives crashes.** While recording, each event is appended to a `.midilog` text file. It becomes
  a `.mid` when the session closes, and logs left over from a crash or shutdown are converted at the
  next start. Exiting abruptly loses nothing, so there is no "quit" command: use
  `taskkill /IM auto-midi-recorder.exe`.
- **Filtered out:** active sensing and MIDI clock/timecode, which are not performance data. Everything
  else, including SysEx, is kept.

## Setup

1. Put `auto-midi-recorder.exe` in a folder you can write to, such as
   `%LOCALAPPDATA%\auto-midi-recorder`.
2. Copy `config.example.toml` next to it as `config.toml` and edit it.
3. Register it to run at logon:

   ```bat
   schtasks /Create /SC ONLOGON /TN auto-midi-recorder /TR "%LOCALAPPDATA%\auto-midi-recorder\auto-midi-recorder.exe"
   ```

Status and errors go to `auto-midi-recorder.log` next to the exe. Check there first if nothing
is being recorded.

On Windows without Windows MIDI Services, MIDI ports are exclusive: if another app holds the port,
the log shows `cannot open ...` and the logger keeps retrying.

## Output

Files are named by the local start time, such as `2026-11-02T204812+0900.mid`.

Each is a format 0 SMF where **one tick is exactly 1 ms** (480 PPQ, 480000 us per quarter). Tempo
here is only a timebase and says nothing about the music. Two meta events describe the session:

| Meta event | Content |
|---|---|
| Track Name | MIDI port name |
| Text | `session_start_utc=2026-11-02T11:48:12.123456Z` |

An event's absolute time is `session_start_utc` + its tick in milliseconds.

## Development

The core builds and tests on any OS; only the MIDI input in `src/app.rs` is Windows-specific.

```sh
cargo test
cargo clippy --all-targets
cargo clippy --all-targets --target x86_64-pc-windows-gnu
```

## License

[MIT](LICENSE)
