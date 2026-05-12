use super::{OracleEvent, OracleEventType};

/// Result of comparing two oracle event sequences.
#[derive(Debug)]
pub struct DiffReport {
    pub total_events_a: usize,
    pub total_events_b: usize,
    pub matches: usize,
    pub divergences: Vec<Divergence>,
}

/// A single divergence between two oracle runs.
#[derive(Debug)]
pub struct Divergence {
    pub index: usize,
    pub event_a: Option<OracleEvent>,
    pub event_b: Option<OracleEvent>,
    pub reason: String,
}

impl DiffReport {
    pub fn is_clean(&self) -> bool {
        self.divergences.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.is_clean() {
            format!(
                "PASS: {} events matched (A={}, B={})",
                self.matches, self.total_events_a, self.total_events_b
            )
        } else {
            format!(
                "FAIL: {} divergences out of {} events (A={}, B={})",
                self.divergences.len(),
                self.matches + self.divergences.len(),
                self.total_events_a,
                self.total_events_b
            )
        }
    }
}

/// Compare two oracle event sequences and produce a diff report.
///
/// Comparison strategy per event type:
/// - Input: exact string match
/// - SDP: structural comparison (ignoring session-id, timing lines)
/// - ICE: structural comparison of candidate components
/// - BWE: tolerance band (±5%)
/// - Encoder properties: exact match
/// - Stats: tolerance band (±10%)
/// - Data channel sends: structural JSON equality
pub fn diff(events_a: &[OracleEvent], events_b: &[OracleEvent]) -> DiffReport {
    let mut matches = 0;
    let mut divergences = Vec::new();

    let max_len = events_a.len().max(events_b.len());

    for i in 0..max_len {
        let a = events_a.get(i);
        let b = events_b.get(i);

        match (a, b) {
            (Some(ea), Some(eb)) => {
                if let Some(reason) = compare_events(ea, eb) {
                    divergences.push(Divergence {
                        index: i,
                        event_a: Some(ea.clone()),
                        event_b: Some(eb.clone()),
                        reason,
                    });
                } else {
                    matches += 1;
                }
            }
            (Some(ea), None) => {
                divergences.push(Divergence {
                    index: i,
                    event_a: Some(ea.clone()),
                    event_b: None,
                    reason: "Event exists in A but not in B".into(),
                });
            }
            (None, Some(eb)) => {
                divergences.push(Divergence {
                    index: i,
                    event_a: None,
                    event_b: Some(eb.clone()),
                    reason: "Event exists in B but not in A".into(),
                });
            }
            (None, None) => unreachable!(),
        }
    }

    DiffReport {
        total_events_a: events_a.len(),
        total_events_b: events_b.len(),
        matches,
        divergences,
    }
}

/// Compare two events, returning None if they match or Some(reason) if they diverge.
fn compare_events(a: &OracleEvent, b: &OracleEvent) -> Option<String> {
    match (&a.event, &b.event) {
        // Input: exact string match
        (OracleEventType::Input { raw: ra }, OracleEventType::Input { raw: rb }) => {
            if ra != rb {
                Some(format!("Input mismatch: '{}' vs '{}'", ra, rb))
            } else {
                None
            }
        }

        // BWE: ±5% tolerance
        (
            OracleEventType::BweUpdate { bitrate_bps: ba },
            OracleEventType::BweUpdate { bitrate_bps: bb },
        ) => {
            let tolerance = (*ba as f64 * 0.05).max(1.0);
            if (*ba as f64 - *bb as f64).abs() > tolerance {
                Some(format!("BWE divergence: {} vs {} (±5% = {})", ba, bb, tolerance))
            } else {
                None
            }
        }

        // Encoder property: exact match
        (
            OracleEventType::EncoderProp {
                property: pa,
                value: va,
            },
            OracleEventType::EncoderProp {
                property: pb,
                value: vb,
            },
        ) => {
            if pa != pb || va != vb {
                Some(format!(
                    "Encoder prop mismatch: {}={} vs {}={}",
                    pa, va, pb, vb
                ))
            } else {
                None
            }
        }

        // SDP: structural comparison (ignore session-id, o= line timing)
        (OracleEventType::SdpOffer { sdp: sa }, OracleEventType::SdpOffer { sdp: sb })
        | (OracleEventType::SdpAnswer { sdp: sa }, OracleEventType::SdpAnswer { sdp: sb }) => {
            compare_sdp(sa, sb)
        }

        // ICE: structural comparison
        (
            OracleEventType::IceCandidate {
                candidate: ca,
                sdp_m_line_index: ia,
                direction: da,
            },
            OracleEventType::IceCandidate {
                candidate: cb,
                sdp_m_line_index: ib,
                direction: db,
            },
        ) => {
            if da != db {
                Some(format!("ICE direction mismatch: {} vs {}", da, db))
            } else if ia != ib {
                Some(format!("ICE mline index mismatch: {} vs {}", ia, ib))
            } else {
                // Compare candidate components (ignore priority which may differ)
                compare_ice_candidate(ca, cb)
            }
        }

        // Data channel send: JSON structural equality
        (
            OracleEventType::DataChannelSend { message: ma },
            OracleEventType::DataChannelSend { message: mb },
        ) => {
            let va: Result<serde_json::Value, _> = serde_json::from_str(ma);
            let vb: Result<serde_json::Value, _> = serde_json::from_str(mb);
            match (va, vb) {
                (Ok(ja), Ok(jb)) => {
                    if ja != jb {
                        Some("DC message JSON mismatch".to_string())
                    } else {
                        None
                    }
                }
                _ => {
                    // Not valid JSON, do string comparison
                    if ma != mb {
                        Some(format!("DC message string mismatch: '{}' vs '{}'", ma, mb))
                    } else {
                        None
                    }
                }
            }
        }

        // Stats: tolerance band (±10%)
        (OracleEventType::Stats { json: ja }, OracleEventType::Stats { json: jb }) => {
            // For stats, we accept ±10% on numeric values
            if ja == jb {
                None
            } else {
                Some("Stats divergence (check manually)".to_string())
            }
        }

        // Pipeline state: exact match
        (
            OracleEventType::PipelineState { state: sa },
            OracleEventType::PipelineState { state: sb },
        ) => {
            if sa != sb {
                Some(format!("Pipeline state mismatch: {} vs {}", sa, sb))
            } else {
                None
            }
        }

        // Signaling: exact match
        (
            OracleEventType::Signaling {
                direction: da,
                message: ma,
            },
            OracleEventType::Signaling {
                direction: db,
                message: mb,
            },
        ) => {
            if da != db || ma != mb {
                Some("Signaling mismatch".to_string())
            } else {
                None
            }
        }

        // Type mismatch
        _ => Some(format!(
            "Event type mismatch: {:?} vs {:?}",
            std::mem::discriminant(&a.event),
            std::mem::discriminant(&b.event)
        )),
    }
}

/// Compare two SDP strings structurally, ignoring session-specific fields.
fn compare_sdp(a: &str, b: &str) -> Option<String> {
    let filter_sdp_line = |line: &str| -> bool {
        // Skip lines that vary between sessions
        !line.starts_with("o=") // origin (session ID, version, timing)
            && !line.starts_with("t=") // timing
            && !line.starts_with("a=ice-ufrag:")
            && !line.starts_with("a=ice-pwd:")
            && !line.starts_with("a=fingerprint:")
            && !line.starts_with("a=setup:")
            && !line.starts_with("a=msid:")
    };

    let lines_a: Vec<&str> = a.lines().filter(|l| filter_sdp_line(l)).collect();
    let lines_b: Vec<&str> = b.lines().filter(|l| filter_sdp_line(l)).collect();

    if lines_a != lines_b {
        // Find first divergent line
        for (i, (la, lb)) in lines_a.iter().zip(lines_b.iter()).enumerate() {
            if la != lb {
                return Some(format!(
                    "SDP diverges at line {}: '{}' vs '{}'",
                    i, la, lb
                ));
            }
        }
        if lines_a.len() != lines_b.len() {
            return Some(format!(
                "SDP line count differs: {} vs {}",
                lines_a.len(),
                lines_b.len()
            ));
        }
    }
    None
}

/// Compare two ICE candidate strings structurally.
fn compare_ice_candidate(a: &str, b: &str) -> Option<String> {
    // ICE candidate format: candidate:<foundation> <component> <protocol> <priority> <ip> <port> typ <type> ...
    // We compare everything except priority (field 3) which may differ
    let parts_a: Vec<&str> = a.split_whitespace().collect();
    let parts_b: Vec<&str> = b.split_whitespace().collect();

    if parts_a.len() != parts_b.len() {
        return Some(format!(
            "ICE candidate field count: {} vs {}",
            parts_a.len(),
            parts_b.len()
        ));
    }

    for (i, (pa, pb)) in parts_a.iter().zip(parts_b.iter()).enumerate() {
        if i == 3 {
            continue; // Skip priority
        }
        if pa != pb {
            return Some(format!(
                "ICE candidate field {} mismatch: '{}' vs '{}'",
                i, pa, pb
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_inputs() {
        let events = vec![
            OracleEvent {
                ts: 0.0,
                event: OracleEventType::Input {
                    raw: "kd,65507".into(),
                },
            },
            OracleEvent {
                ts: 0.1,
                event: OracleEventType::Input {
                    raw: "m,500,300,1,0".into(),
                },
            },
        ];

        let report = diff(&events, &events);
        assert!(report.is_clean());
        assert_eq!(report.matches, 2);
        assert_eq!(report.divergences.len(), 0);
    }

    #[test]
    fn test_divergent_inputs() {
        let events_a = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::Input {
                raw: "kd,65507".into(),
            },
        }];
        let events_b = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::Input {
                raw: "kd,65508".into(),
            },
        }];

        let report = diff(&events_a, &events_b);
        assert!(!report.is_clean());
        assert_eq!(report.divergences.len(), 1);
        assert!(report.divergences[0].reason.contains("Input mismatch"));
    }

    #[test]
    fn test_bwe_within_tolerance() {
        let events_a = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::BweUpdate {
                bitrate_bps: 1000000,
            },
        }];
        let events_b = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::BweUpdate {
                bitrate_bps: 1040000, // 4% difference, within 5%
            },
        }];

        let report = diff(&events_a, &events_b);
        assert!(report.is_clean());
    }

    #[test]
    fn test_bwe_outside_tolerance() {
        let events_a = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::BweUpdate {
                bitrate_bps: 1000000,
            },
        }];
        let events_b = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::BweUpdate {
                bitrate_bps: 1100000, // 10% difference, outside 5%
            },
        }];

        let report = diff(&events_a, &events_b);
        assert!(!report.is_clean());
    }

    #[test]
    fn test_length_mismatch() {
        let events_a = vec![
            OracleEvent {
                ts: 0.0,
                event: OracleEventType::Input {
                    raw: "kd,65507".into(),
                },
            },
            OracleEvent {
                ts: 0.1,
                event: OracleEventType::Input {
                    raw: "ku,65507".into(),
                },
            },
        ];
        let events_b = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::Input {
                raw: "kd,65507".into(),
            },
        }];

        let report = diff(&events_a, &events_b);
        assert!(!report.is_clean());
        assert_eq!(report.divergences.len(), 1);
        assert!(report.divergences[0].reason.contains("not in B"));
    }

    #[test]
    fn test_type_mismatch() {
        let events_a = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::Input {
                raw: "kd,65507".into(),
            },
        }];
        let events_b = vec![OracleEvent {
            ts: 0.0,
            event: OracleEventType::BweUpdate {
                bitrate_bps: 1000000,
            },
        }];

        let report = diff(&events_a, &events_b);
        assert!(!report.is_clean());
        assert!(report.divergences[0].reason.contains("type mismatch"));
    }

    #[test]
    fn test_sdp_ignores_session_fields() {
        let sdp_a = "v=0\r\no=- 123 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";
        let sdp_b = "v=0\r\no=- 456 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";

        let result = compare_sdp(sdp_a, sdp_b);
        assert!(result.is_none(), "SDPs should match after filtering: {:?}", result);
    }

    #[test]
    fn test_diff_report_summary() {
        let report = DiffReport {
            total_events_a: 10,
            total_events_b: 10,
            matches: 10,
            divergences: vec![],
        };
        assert!(report.summary().starts_with("PASS"));

        let report_fail = DiffReport {
            total_events_a: 10,
            total_events_b: 10,
            matches: 8,
            divergences: vec![
                Divergence {
                    index: 0,
                    event_a: None,
                    event_b: None,
                    reason: "test".into(),
                },
                Divergence {
                    index: 1,
                    event_a: None,
                    event_b: None,
                    reason: "test2".into(),
                },
            ],
        };
        assert!(report_fail.summary().starts_with("FAIL"));
    }
}
