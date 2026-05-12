//! Dual-runtime test oracle for Python↔Rust comparison.
//!
//! Components:
//! - Recorder: captures events from live Python Selkies to JSONL file
//! - Replayer: feeds recorded events into either implementation
//! - Differ: compares two replay outputs and reports divergences

pub mod recorder;
pub mod differ;

use serde::{Deserialize, Serialize};

/// A single recorded oracle event with timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleEvent {
    /// Monotonic timestamp in seconds since recording start
    pub ts: f64,
    /// Event type tag
    #[serde(flatten)]
    pub event: OracleEventType,
}

/// Categorized oracle event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OracleEventType {
    /// SDP offer generated
    #[serde(rename = "sdp_offer")]
    SdpOffer { sdp: String },

    /// SDP answer received
    #[serde(rename = "sdp_answer")]
    SdpAnswer { sdp: String },

    /// ICE candidate generated or received
    #[serde(rename = "ice_candidate")]
    IceCandidate {
        candidate: String,
        sdp_m_line_index: u32,
        direction: String,
    },

    /// Input event received from data channel
    #[serde(rename = "input")]
    Input { raw: String },

    /// Bandwidth estimate update
    #[serde(rename = "bwe_update")]
    BweUpdate { bitrate_bps: u64 },

    /// Encoder property change
    #[serde(rename = "encoder_prop")]
    EncoderProp { property: String, value: String },

    /// Data channel message sent to client
    #[serde(rename = "dc_send")]
    DataChannelSend { message: String },

    /// Stats report
    #[serde(rename = "stats")]
    Stats { json: String },

    /// Pipeline state change
    #[serde(rename = "pipeline_state")]
    PipelineState { state: String },

    /// Signaling message sent or received
    #[serde(rename = "signaling")]
    Signaling {
        direction: String,
        message: String,
    },
}
