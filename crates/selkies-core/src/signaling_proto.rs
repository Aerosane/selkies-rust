use serde::{Deserialize, Serialize};

/// Signaling protocol messages (WebSocket text frames)
/// Original Python: webrtc_signalling.py, signalling_web.py
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SignalingMessage {
    /// Client registration: "HELLO <uid> [<meta_b64>]"
    Hello {
        uid: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<PeerMeta>,
    },

    /// Server acknowledgment: "HELLO"
    HelloAck,

    /// Session request: "SESSION <peer_id>"
    Session { peer_id: String },

    /// Session established: "SESSION_OK [<meta_b64>]"
    SessionOk {
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<PeerMeta>,
    },

    /// Error: "ERROR <msg>"
    Error { msg: String },

    /// SDP exchange (relayed peer-to-peer)
    Sdp { sdp_type: SdpType, sdp: String },

    /// ICE candidate exchange (relayed peer-to-peer)
    Ice {
        candidate: String,
        sdp_m_line_index: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub res: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpType {
    Offer,
    Answer,
}

impl SignalingMessage {
    /// Parse a raw signaling protocol text message
    // Original Python: webrtc_signalling.py:131-190 start() message loop
    pub fn parse_text(text: &str) -> Result<Self, String> {
        let text = text.trim();

        if text == "HELLO" {
            return Ok(SignalingMessage::HelloAck);
        }

        if let Some(rest) = text.strip_prefix("HELLO ") {
            let parts: Vec<&str> = rest.splitn(2, ' ').collect();
            let uid = parts[0].to_string();
            let meta = parts.get(1).and_then(|b64| {
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            });
            return Ok(SignalingMessage::Hello { uid, meta });
        }

        if let Some(peer_id) = text.strip_prefix("SESSION ") {
            return Ok(SignalingMessage::Session {
                peer_id: peer_id.trim().to_string(),
            });
        }

        if text == "SESSION_OK" {
            return Ok(SignalingMessage::SessionOk { meta: None });
        }

        if let Some(rest) = text.strip_prefix("SESSION_OK ") {
            let meta = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, rest.trim())
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            return Ok(SignalingMessage::SessionOk { meta });
        }

        if let Some(msg) = text.strip_prefix("ERROR ") {
            return Ok(SignalingMessage::Error {
                msg: msg.to_string(),
            });
        }

        // Try JSON (SDP or ICE)
        if text.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(sdp_obj) = v.get("sdp") {
                    let sdp_type = match sdp_obj.get("type").and_then(|t| t.as_str()) {
                        Some("offer") => SdpType::Offer,
                        Some("answer") => SdpType::Answer,
                        other => return Err(format!("Unknown SDP type: {:?}", other)),
                    };
                    let sdp = sdp_obj
                        .get("sdp")
                        .and_then(|s| s.as_str())
                        .ok_or("Missing sdp field")?
                        .to_string();
                    return Ok(SignalingMessage::Sdp { sdp_type, sdp });
                }

                if let Some(ice_obj) = v.get("ice") {
                    let candidate = ice_obj
                        .get("candidate")
                        .and_then(|c| c.as_str())
                        .ok_or("Missing candidate field")?
                        .to_string();
                    let sdp_m_line_index = ice_obj
                        .get("sdpMLineIndex")
                        .and_then(|i| i.as_u64())
                        .ok_or("Missing sdpMLineIndex")? as u32;
                    return Ok(SignalingMessage::Ice {
                        candidate,
                        sdp_m_line_index,
                    });
                }
            }
        }

        Err(format!("Unknown signaling message: {}", text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hello_ack() {
        let msg = SignalingMessage::parse_text("HELLO").unwrap();
        assert!(matches!(msg, SignalingMessage::HelloAck));
    }

    #[test]
    fn test_parse_hello_with_uid() {
        let msg = SignalingMessage::parse_text("HELLO 12345").unwrap();
        match msg {
            SignalingMessage::Hello { uid, meta } => {
                assert_eq!(uid, "12345");
                assert!(meta.is_none());
            }
            _ => panic!("Expected Hello"),
        }
    }

    #[test]
    fn test_parse_session() {
        let msg = SignalingMessage::parse_text("SESSION 1").unwrap();
        match msg {
            SignalingMessage::Session { peer_id } => assert_eq!(peer_id, "1"),
            _ => panic!("Expected Session"),
        }
    }

    #[test]
    fn test_parse_session_ok() {
        let msg = SignalingMessage::parse_text("SESSION_OK").unwrap();
        assert!(matches!(msg, SignalingMessage::SessionOk { meta: None }));
    }

    #[test]
    fn test_parse_error() {
        let msg = SignalingMessage::parse_text("ERROR peer not found").unwrap();
        match msg {
            SignalingMessage::Error { msg } => assert_eq!(msg, "peer not found"),
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_parse_sdp_offer() {
        let json = r#"{"sdp": {"type": "offer", "sdp": "v=0\r\n"}}"#;
        let msg = SignalingMessage::parse_text(json).unwrap();
        match msg {
            SignalingMessage::Sdp { sdp_type, sdp } => {
                assert!(matches!(sdp_type, SdpType::Offer));
                assert_eq!(sdp, "v=0\r\n");
            }
            _ => panic!("Expected Sdp"),
        }
    }

    #[test]
    fn test_parse_ice_candidate() {
        let json = r#"{"ice": {"candidate": "candidate:1 1 UDP 2122252543 192.168.1.1 12345 typ host", "sdpMLineIndex": 0}}"#;
        let msg = SignalingMessage::parse_text(json).unwrap();
        match msg {
            SignalingMessage::Ice { candidate, sdp_m_line_index } => {
                assert!(candidate.starts_with("candidate:"));
                assert_eq!(sdp_m_line_index, 0);
            }
            _ => panic!("Expected Ice"),
        }
    }

    #[test]
    fn test_parse_unknown() {
        let result = SignalingMessage::parse_text("GARBAGE");
        assert!(result.is_err());
    }
}
