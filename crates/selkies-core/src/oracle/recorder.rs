use std::fs::File;
use std::io::{BufWriter, Write, BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

use crate::error::OracleError;
use super::{OracleEvent, OracleEventType};

/// Records oracle events to a JSONL file for later replay and comparison.
///
/// Original Python equivalent: none — this is new test infrastructure.
///
/// Usage:
/// ```ignore
/// let mut recorder = Recorder::new("/tmp/oracle_recording.jsonl").unwrap();
/// recorder.record(OracleEventType::Input { raw: "kd,65507".into() }).unwrap();
/// recorder.flush().unwrap();
/// ```
pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
}

impl Recorder {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, OracleError> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            start: Instant::now(),
        })
    }

    /// Record an event with automatic timestamping
    pub fn record(&mut self, event: OracleEventType) -> Result<(), OracleError> {
        let oracle_event = OracleEvent {
            ts: self.start.elapsed().as_secs_f64(),
            event,
        };
        let line = serde_json::to_string(&oracle_event)?;
        writeln!(self.writer, "{}", line)?;
        Ok(())
    }

    /// Flush buffered writes to disk
    pub fn flush(&mut self) -> Result<(), OracleError> {
        self.writer.flush()?;
        Ok(())
    }
}

/// Reads a JSONL recording file into a vector of oracle events.
pub fn load_recording<P: AsRef<Path>>(path: P) -> Result<Vec<OracleEvent>, OracleError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: OracleEvent = serde_json::from_str(&line)?;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_record_and_load() {
        let tmpfile = NamedTempFile::new().unwrap();
        let path = tmpfile.path().to_path_buf();

        // Record some events
        {
            let mut recorder = Recorder::new(&path).unwrap();
            recorder
                .record(OracleEventType::Input {
                    raw: "kd,65507".into(),
                })
                .unwrap();
            recorder
                .record(OracleEventType::Input {
                    raw: "m,500,300,1,0".into(),
                })
                .unwrap();
            recorder
                .record(OracleEventType::BweUpdate {
                    bitrate_bps: 25000000,
                })
                .unwrap();
            recorder.flush().unwrap();
        }

        // Load and verify
        let events = load_recording(&path).unwrap();
        assert_eq!(events.len(), 3);

        // Verify timestamps are monotonically increasing
        assert!(events[0].ts <= events[1].ts);
        assert!(events[1].ts <= events[2].ts);

        // Verify event types
        match &events[0].event {
            OracleEventType::Input { raw } => assert_eq!(raw, "kd,65507"),
            _ => panic!("Expected Input event"),
        }
        match &events[2].event {
            OracleEventType::BweUpdate { bitrate_bps } => assert_eq!(*bitrate_bps, 25000000),
            _ => panic!("Expected BweUpdate event"),
        }
    }

    #[test]
    fn test_record_empty_file() {
        let tmpfile = NamedTempFile::new().unwrap();
        let path = tmpfile.path().to_path_buf();

        // Write empty
        {
            let mut recorder = Recorder::new(&path).unwrap();
            recorder.flush().unwrap();
        }

        let events = load_recording(&path).unwrap();
        assert_eq!(events.len(), 0);
    }

    // Verify this test has a failure mode: swap assert_eq args
    #[test]
    fn test_event_serialization_roundtrip() {
        let event = OracleEvent {
            ts: 1.234,
            event: OracleEventType::EncoderProp {
                property: "bitrate".into(),
                value: "25000".into(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: OracleEvent = serde_json::from_str(&json).unwrap();
        assert!((parsed.ts - 1.234).abs() < f64::EPSILON);
        match parsed.event {
            OracleEventType::EncoderProp { property, value } => {
                assert_eq!(property, "bitrate");
                assert_eq!(value, "25000");
            }
            _ => panic!("Wrong event type after roundtrip"),
        }
    }
}
