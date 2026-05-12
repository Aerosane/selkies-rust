//! Pipeline configuration and lifecycle management.
//!
//! Ported from: gstwebrtc_app.py GSTWebRTCApp.__init__ (L68-133)
//!
//! This module contains the pipeline configuration data structures
//! and builder pattern. The actual GStreamer pipeline construction
//! (build_video_pipeline, build_audio_pipeline, build_webrtcbin_pipeline)
//! will be implemented when gstreamer-rs is integrated.
//!
// TODO(port): build_video_pipeline — 15 encoder variants, ~400 lines
// TODO(port): build_audio_pipeline — ~50 lines
// TODO(port): build_webrtcbin_pipeline — webrtcbin + congestion control
// TODO(port): pipeline lifecycle — start, stop, restart, handle_bus_calls

use selkies_stats::EncoderType;
use serde::{Deserialize, Serialize};

/// Pipeline configuration — all parameters needed to build GStreamer pipelines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// STUN server URIs (e.g., "stun:stun.l.google.com:19302")
    pub stun_servers: Vec<String>,
    /// TURN server URIs (e.g., "turn://user:pass@host:port")
    pub turn_servers: Vec<String>,
    /// Audio channels (default: 2)
    pub audio_channels: u32,
    /// Target framerate (default: 30)
    pub framerate: u32,
    /// Encoder element name (e.g., "nvh265enc")
    pub encoder: String,
    /// GPU device ID for hardware encoders
    pub gpu_id: u32,
    /// Target video bitrate in kbps
    pub video_bitrate: u32,
    /// Target audio bitrate in bps
    pub audio_bitrate: u32,
    /// Keyframe distance in seconds (-1.0 = auto/no forced keyframes)
    pub keyframe_distance: f64,
    /// Enable GCC bandwidth estimation / congestion control
    pub congestion_control: bool,
    /// Expected video packet loss percentage for FEC compensation
    pub video_packetloss_percent: f64,
    /// Expected audio packet loss percentage for FEC compensation
    pub audio_packetloss_percent: f64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            stun_servers: Vec::new(),
            turn_servers: Vec::new(),
            audio_channels: 2,
            framerate: 30,
            encoder: "nvh264enc".to_string(),
            gpu_id: 0,
            video_bitrate: 2000,
            audio_bitrate: 96000,
            keyframe_distance: -1.0,
            congestion_control: false,
            video_packetloss_percent: 0.0,
            audio_packetloss_percent: 0.0,
        }
    }
}

/// Derived pipeline parameters computed from PipelineConfig.
#[derive(Debug, Clone)]
pub struct DerivedParams {
    /// Parsed encoder type
    pub encoder_type: Option<EncoderType>,
    /// FEC-compensated video bitrate (kbps)
    pub fec_video_bitrate: u32,
    /// FEC-compensated audio bitrate (bps)
    pub fec_audio_bitrate: u32,
    /// Keyframe frame distance (-1 = auto)
    pub keyframe_frame_distance: i32,
    /// Minimum keyframe interval in frames
    pub min_keyframe_frame_distance: u32,
    /// VBV/HRD multipliers per encoder family
    pub vbv_multiplier: f64,
}

impl DerivedParams {
    /// Compute derived parameters from a pipeline config.
    pub fn from_config(config: &PipelineConfig) -> Self {
        let encoder_type = EncoderType::from_encoder_name(&config.encoder);

        let fec_video = selkies_stats::fec_video_bitrate(
            config.video_bitrate,
            config.video_packetloss_percent,
        );
        let fec_audio = selkies_stats::fec_audio_bitrate(
            config.audio_bitrate,
            config.audio_packetloss_percent,
        );

        let min_kf = 60u32;
        let kf_distance = if config.keyframe_distance < 0.0 {
            -1
        } else {
            std::cmp::max(
                min_kf as i32,
                (config.framerate as f64 * config.keyframe_distance) as i32,
            )
        };

        // VBV multiplier: 1.5 when no keyframes (optimal), 3.0 when periodic keyframes
        let vbv_mult = if config.keyframe_distance < 0.0 {
            1.5
        } else {
            3.0
        };

        Self {
            encoder_type,
            fec_video_bitrate: fec_video,
            fec_audio_bitrate: fec_audio,
            keyframe_frame_distance: kf_distance,
            min_keyframe_frame_distance: min_kf,
            vbv_multiplier: vbv_mult,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = PipelineConfig::default();
        assert_eq!(config.framerate, 30);
        assert_eq!(config.video_bitrate, 2000);
        assert_eq!(config.audio_bitrate, 96000);
        assert_eq!(config.encoder, "nvh264enc");
    }

    #[test]
    fn test_derived_params_defaults() {
        let config = PipelineConfig::default();
        let derived = DerivedParams::from_config(&config);
        assert_eq!(derived.encoder_type, Some(EncoderType::NvH264));
        assert_eq!(derived.fec_video_bitrate, 2000);
        assert_eq!(derived.fec_audio_bitrate, 96000);
        assert_eq!(derived.keyframe_frame_distance, -1);
        assert_eq!(derived.vbv_multiplier, 1.5);
    }

    #[test]
    fn test_derived_params_with_fec() {
        let config = PipelineConfig {
            video_bitrate: 25000,
            video_packetloss_percent: 5.0,
            audio_bitrate: 96000,
            audio_packetloss_percent: 5.0,
            ..PipelineConfig::default()
        };
        let derived = DerivedParams::from_config(&config);
        assert_eq!(derived.fec_video_bitrate, 23809);
        assert_eq!(derived.fec_audio_bitrate, 100800);
    }

    #[test]
    fn test_derived_params_with_keyframes() {
        let config = PipelineConfig {
            framerate: 144,
            keyframe_distance: 2.0,
            ..PipelineConfig::default()
        };
        let derived = DerivedParams::from_config(&config);
        assert_eq!(derived.keyframe_frame_distance, 288);
        assert_eq!(derived.vbv_multiplier, 3.0);
    }

    #[test]
    fn test_derived_params_min_keyframe_distance() {
        let config = PipelineConfig {
            framerate: 30,
            keyframe_distance: 0.5,
            ..PipelineConfig::default()
        };
        let derived = DerivedParams::from_config(&config);
        assert_eq!(derived.keyframe_frame_distance, 60);
    }

    #[test]
    fn test_derived_hevc_encoder() {
        let config = PipelineConfig {
            encoder: "nvh265enc".to_string(),
            ..PipelineConfig::default()
        };
        let derived = DerivedParams::from_config(&config);
        assert_eq!(derived.encoder_type, Some(EncoderType::NvH265));
    }
}
