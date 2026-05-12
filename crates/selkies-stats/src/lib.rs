//! Bitrate management, FEC compensation, and encoder property mapping.
//!
//! Ported from: gstwebrtc_app.py set_video_bitrate (L1353-1415),
//!              set_audio_bitrate (L1417-1469)
//!
//! The actual GStreamer property-setting lives in the pipeline crate.
//! This module provides the pure calculations and encoder metadata.
//!
// TODO(port): metrics.py — Prometheus export (optional, low priority)
// TODO(port): gpu_monitor.py — GPUMonitor (49 lines)
// TODO(port): system_monitor.py — SystemMonitor (39 lines)

use serde::{Deserialize, Serialize};

/// Encoder types supported by Selkies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EncoderType {
    NvH264,
    NvH265,
    VaH264,
    VaH265,
    X264,
    X265,
    OpenH264,
    Vp8,
    Vp9,
    SvtAv1,
    Av1,
    Rav1,
}

impl EncoderType {
    /// Parse encoder type from the GStreamer element name.
    pub fn from_encoder_name(name: &str) -> Option<Self> {
        let lower = name.to_lowercase();
        if lower == "x264enc" {
            Some(Self::X264)
        } else if lower == "x265enc" {
            Some(Self::X265)
        } else if lower == "openh264enc" {
            Some(Self::OpenH264)
        } else if lower == "vp8enc" {
            Some(Self::Vp8)
        } else if lower == "vp9enc" {
            Some(Self::Vp9)
        } else if lower == "svtav1enc" {
            Some(Self::SvtAv1)
        } else if lower == "av1enc" {
            Some(Self::Av1)
        } else if lower == "rav1enc" {
            Some(Self::Rav1)
        } else if lower.starts_with("nv") && lower != "nvfbcenc" {
            if lower.contains("h265") || lower.contains("hevc") {
                Some(Self::NvH265)
            } else {
                Some(Self::NvH264)
            }
        } else if lower.starts_with("va") {
            if lower.contains("h265") || lower.contains("hevc") {
                Some(Self::VaH265)
            } else {
                Some(Self::VaH264)
            }
        } else {
            None
        }
    }

    /// GStreamer element name used in Gst.Bin.get_by_name().
    pub fn pipeline_element_name(&self) -> &'static str {
        match self {
            Self::NvH264 | Self::NvH265 => "nvenc",
            Self::VaH264 | Self::VaH265 => "vaenc",
            Self::X264 => "x264enc",
            Self::X265 => "x265enc",
            Self::OpenH264 => "openh264enc",
            Self::Vp8 | Self::Vp9 => "vpenc",
            Self::SvtAv1 => "svtav1enc",
            Self::Av1 => "av1enc",
            Self::Rav1 => "rav1enc",
        }
    }

    /// GStreamer property name for setting bitrate.
    pub fn bitrate_property(&self) -> &'static str {
        match self {
            Self::NvH264 | Self::NvH265 | Self::X264 | Self::X265 | Self::SvtAv1 | Self::Av1
            | Self::VaH264 | Self::VaH265 => "bitrate",
            Self::OpenH264 | Self::Rav1 | Self::Vp8 | Self::Vp9 => "target-bitrate",
        }
    }

    /// Whether the encoder expects bitrate in kbps (true) or bps (false).
    pub fn bitrate_in_kbps(&self) -> bool {
        match self {
            Self::NvH264 | Self::NvH265 | Self::VaH264 | Self::VaH265 | Self::X264
            | Self::X265 | Self::SvtAv1 | Self::Av1 => true,
            Self::OpenH264 | Self::Rav1 | Self::Vp8 | Self::Vp9 => false,
        }
    }

    pub fn is_h264(&self) -> bool {
        matches!(self, Self::NvH264 | Self::VaH264 | Self::X264 | Self::OpenH264)
    }

    pub fn is_h265(&self) -> bool {
        matches!(self, Self::NvH265 | Self::VaH265 | Self::X265)
    }
}

/// Compute FEC-compensated video bitrate.
///
/// Reduces target bitrate to account for FEC overhead, preventing overshoot.
pub fn fec_video_bitrate(bitrate_kbps: u32, packetloss_percent: f64) -> u32 {
    (bitrate_kbps as f64 / (1.0 + (packetloss_percent / 100.0))) as u32
}

/// Compute FEC-compensated audio bitrate.
///
/// Audio keeps exact bitrate and increases effective bitrate after FEC.
pub fn fec_audio_bitrate(bitrate_bps: u32, packetloss_percent: f64) -> u32 {
    (bitrate_bps as f64 * (1.0 + (packetloss_percent / 100.0))) as u32
}

/// Compute VBV buffer size for NVENC (in kbps).
///
/// Uses fec_bitrate // 2 which gives ~500ms buffer.
pub fn nvenc_vbv_buffer_size(fec_bitrate_kbps: u32) -> u32 {
    fec_bitrate_kbps / 2
}

/// Compute CPB size for VA-API encoders.
///
/// Formula: ceil(fec_bitrate / framerate) * multiplier
pub fn vaapi_cpb_size(fec_bitrate_kbps: u32, framerate: u32, vbv_multiplier: f64) -> u32 {
    let per_frame = fec_bitrate_kbps.div_ceil(framerate);
    (per_frame as f64 * vbv_multiplier) as u32
}

/// Congestion control bitrate range for rtpgccbwe.
#[derive(Debug, Clone)]
pub struct CongestionControlRange {
    pub min_bitrate: u32,
    pub max_bitrate: u32,
    pub estimated_bitrate: u32,
}

/// Compute congestion control range when video bitrate changes.
pub fn cc_range_for_video(
    video_bitrate_kbps: u32,
    fec_audio_bitrate_bps: u32,
) -> CongestionControlRange {
    let floor = 100_000u64 + fec_audio_bitrate_bps as u64;
    let proportional = video_bitrate_kbps as u64 * 100 + fec_audio_bitrate_bps as u64;
    let min = std::cmp::max(floor, proportional) as u32;
    let max = video_bitrate_kbps * 1000 + fec_audio_bitrate_bps;
    CongestionControlRange {
        min_bitrate: min,
        max_bitrate: max,
        estimated_bitrate: max,
    }
}

/// Compute congestion control range when audio bitrate changes.
pub fn cc_range_for_audio(
    video_bitrate_kbps: u32,
    fec_audio_bitrate_bps: u32,
) -> CongestionControlRange {
    cc_range_for_video(video_bitrate_kbps, fec_audio_bitrate_bps)
}

/// Convert bitrate to the value the encoder expects.
pub fn bitrate_for_encoder(encoder: &EncoderType, fec_bitrate_kbps: u32) -> u32 {
    if encoder.bitrate_in_kbps() {
        fec_bitrate_kbps
    } else {
        fec_bitrate_kbps * 1000
    }
}

/// Log a bitrate change.
pub fn log_bitrate_change(bitrate_kbps: u32, is_cc: bool) {
    if is_cc {
        tracing::debug!("video bitrate set with congestion control to: {}", bitrate_kbps);
    } else {
        tracing::info!("video bitrate set to: {}", bitrate_kbps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_type_from_name() {
        assert_eq!(EncoderType::from_encoder_name("nvh264enc"), Some(EncoderType::NvH264));
        assert_eq!(EncoderType::from_encoder_name("nvh265enc"), Some(EncoderType::NvH265));
        assert_eq!(EncoderType::from_encoder_name("x264enc"), Some(EncoderType::X264));
        assert_eq!(EncoderType::from_encoder_name("vp8enc"), Some(EncoderType::Vp8));
        assert_eq!(EncoderType::from_encoder_name("vp9enc"), Some(EncoderType::Vp9));
        assert_eq!(EncoderType::from_encoder_name("svtav1enc"), Some(EncoderType::SvtAv1));
        assert_eq!(EncoderType::from_encoder_name("rav1enc"), Some(EncoderType::Rav1));
        assert_eq!(EncoderType::from_encoder_name("nvfbcenc"), None);
        assert_eq!(EncoderType::from_encoder_name("unknown"), None);
    }

    #[test]
    fn test_fec_video_bitrate() {
        let result = fec_video_bitrate(25000, 5.0);
        assert_eq!(result, 23809); // 25000 / 1.05
    }

    #[test]
    fn test_fec_video_bitrate_zero_loss() {
        assert_eq!(fec_video_bitrate(25000, 0.0), 25000);
    }

    #[test]
    fn test_fec_audio_bitrate() {
        let result = fec_audio_bitrate(96000, 5.0);
        assert_eq!(result, 100800); // 96000 * 1.05
    }

    #[test]
    fn test_nvenc_vbv() {
        assert_eq!(nvenc_vbv_buffer_size(23809), 11904);
    }

    #[test]
    fn test_vaapi_cpb() {
        let cpb = vaapi_cpb_size(25000, 144, 1.5);
        assert_eq!(cpb, 261); // ceil(25000/144) * 1.5
    }

    #[test]
    fn test_cc_range() {
        let range = cc_range_for_video(25000, 100800);
        assert_eq!(range.min_bitrate, 2600800);
        assert_eq!(range.max_bitrate, 25100800);
    }

    #[test]
    fn test_cc_range_low_bitrate() {
        let range = cc_range_for_video(500, 96000);
        assert_eq!(range.min_bitrate, 196000); // max(196000, 146000)
    }

    #[test]
    fn test_bitrate_for_encoder() {
        assert_eq!(bitrate_for_encoder(&EncoderType::NvH264, 25000), 25000);
        assert_eq!(bitrate_for_encoder(&EncoderType::Vp8, 25000), 25000000);
        assert_eq!(bitrate_for_encoder(&EncoderType::OpenH264, 2000), 2000000);
    }

    #[test]
    fn test_encoder_pipeline_names() {
        assert_eq!(EncoderType::NvH264.pipeline_element_name(), "nvenc");
        assert_eq!(EncoderType::NvH265.pipeline_element_name(), "nvenc");
        assert_eq!(EncoderType::VaH264.pipeline_element_name(), "vaenc");
        assert_eq!(EncoderType::Vp8.pipeline_element_name(), "vpenc");
        assert_eq!(EncoderType::Vp9.pipeline_element_name(), "vpenc");
    }

    #[test]
    fn test_encoder_is_h264_h265() {
        assert!(EncoderType::NvH264.is_h264());
        assert!(!EncoderType::NvH264.is_h265());
        assert!(EncoderType::NvH265.is_h265());
        assert!(!EncoderType::NvH265.is_h264());
        assert!(!EncoderType::Vp8.is_h264());
        assert!(!EncoderType::Vp8.is_h265());
    }
}
