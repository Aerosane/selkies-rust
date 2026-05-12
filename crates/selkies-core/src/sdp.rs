//! SDP munging and HEVC SPS rewriting for WebRTC compatibility.
//!
//! Ported from: gstwebrtc_app.py __on_offer_created (L1582-1688)
//!              gstwebrtc_app.py _hevc_level_probe (L445-496)
//!
//! These are pure string/byte operations with no GStreamer dependency.
//! The GStreamer integration (set_sdp, set_ice, pad probes) lives in
//! the pipeline crate.

use base64::Engine;
use regex::Regex;
use tracing;

/// Inject or fix `rtx-time=125` in SDP text.
///
/// rtx-time needs to be 125ms for optimal retransmission performance.
pub fn inject_rtx_time(sdp: &str) -> String {
    if !sdp.contains("rtx-time") {
        tracing::warn!("injecting rtx-time to SDP");
        let re = Regex::new(r"(apt=\d+)").unwrap();
        re.replace_all(sdp, "${1};rtx-time=125").into_owned()
    } else if !sdp.contains("rtx-time=125") {
        tracing::warn!("injecting modified rtx-time to SDP");
        let re = Regex::new(r"rtx-time=\d+").unwrap();
        re.replace_all(sdp, "rtx-time=125").into_owned()
    } else {
        sdp.to_string()
    }
}

/// Inject or fix H.264 `profile-level-id=42e01f` and `level-asymmetry-allowed=1`.
///
/// Firefox needs profile-level-id in the offer; webrtcbin doesn't add it.
/// See: https://gitlab.freedesktop.org/gstreamer/gstreamer/-/issues/1106
pub fn inject_h264_profile(sdp: &str) -> String {
    let mut result = sdp.to_string();

    // profile-level-id
    if !result.contains("profile-level-id") {
        tracing::warn!("injecting profile-level-id to SDP");
        result = result.replace(
            "packetization-mode=",
            "profile-level-id=42e01f;packetization-mode=",
        );
    } else if !result.contains("profile-level-id=42e01f") {
        tracing::warn!("injecting modified profile-level-id to SDP");
        let re = Regex::new(r"profile-level-id=\w+").unwrap();
        result = re
            .replace_all(&result, "profile-level-id=42e01f")
            .into_owned();
    }

    // level-asymmetry-allowed
    if !result.contains("level-asymmetry-allowed") {
        tracing::warn!("injecting level-asymmetry-allowed to SDP");
        result = result.replace(
            "packetization-mode=",
            "level-asymmetry-allowed=1;packetization-mode=",
        );
    } else if !result.contains("level-asymmetry-allowed=1") {
        tracing::warn!("injecting modified level-asymmetry-allowed to SDP");
        let re = Regex::new(r"level-asymmetry-allowed=\d+").unwrap();
        result = re
            .replace_all(&result, "level-asymmetry-allowed=1")
            .into_owned();
    }

    result
}

/// Inject or fix `sps-pps-idr-in-keyframe=1` for H.264/H.265.
pub fn inject_sps_pps_idr(sdp: &str) -> String {
    if !sdp.contains("sps-pps-idr-in-keyframe") {
        tracing::warn!("injecting sps-pps-idr-in-keyframe to SDP");
        sdp.replace(
            "packetization-mode=",
            "sps-pps-idr-in-keyframe=1;packetization-mode=",
        )
    } else if !sdp.contains("sps-pps-idr-in-keyframe=1") {
        tracing::warn!("injecting modified sps-pps-idr-in-keyframe to SDP");
        let re = Regex::new(r"sps-pps-idr-in-keyframe=\d+").unwrap();
        re.replace_all(sdp, "sps-pps-idr-in-keyframe=1")
            .into_owned()
    } else {
        sdp.to_string()
    }
}

/// Inject Opus `ptime=10` after sprop lines.
pub fn inject_opus_ptime(sdp: &str) -> String {
    if sdp.to_lowercase().contains("opus/") {
        let re = Regex::new(r"([^-]sprop-[^\r\n]+)").unwrap();
        re.replace_all(sdp, "${1}\r\na=ptime:10").into_owned()
    } else {
        sdp.to_string()
    }
}

/// Rewrite HEVC SPS base64 blob to set `general_level_idc=180` (Level 6.0).
///
/// NVENC auto-selects Level 5.0 (150) which caps Chrome decode at ~20fps@1080p.
/// This rewrites the level byte in the SPS NAL unit, handling emulation prevention bytes.
fn rewrite_sps_level_b64(sps_b64: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut sps = engine.decode(sps_b64).ok()?;

    // Skip 2-byte NAL header, then walk to raw byte 12 (general_level_idc)
    // accounting for emulation prevention bytes (00 00 03)
    let mut raw_idx = 0;
    let mut pos = 2; // skip NAL header
    while raw_idx < 12 && pos + 2 < sps.len() {
        if sps[pos] == 0 && sps[pos + 1] == 0 && pos + 2 < sps.len() && sps[pos + 2] == 3 {
            raw_idx += 2;
            pos += 3; // skip EPB
        } else {
            raw_idx += 1;
            pos += 1;
        }
    }

    if raw_idx == 12 && pos < sps.len() && sps[pos] < 180 {
        sps[pos] = 180; // Level 6.0
        Some(engine.encode(&sps))
    } else {
        None
    }
}

/// Inject HEVC `level-id=180` and rewrite `sprop-sps` in the video m= section.
///
/// Only modifies H.265 fmtp lines (skipping RTX apt= lines).
pub fn inject_hevc_level(sdp: &str) -> String {
    let mut lines: Vec<String> = sdp.split("\r\n").map(|s| s.to_string()).collect();
    let mut in_video = false;
    let fmtp_re = Regex::new(r"^(a=fmtp:\d+ )(.*)$").unwrap();
    let sps_re = Regex::new(r"sprop-sps=([A-Za-z0-9+/=]+)").unwrap();
    let level_re = Regex::new(r"level-id=\d+").unwrap();

    for line in &mut lines {
        if line.starts_with("m=video") {
            in_video = true;
            continue;
        } else if line.starts_with("m=") {
            in_video = false;
            continue;
        }

        if in_video && line.starts_with("a=fmtp:") && !line.contains("apt=") {
            if let Some(caps) = fmtp_re.captures(line) {
                let prefix = caps.get(1).unwrap().as_str().to_string();
                let mut rest = caps.get(2).unwrap().as_str().to_string();

                // Inject or fix level-id
                if !rest.contains("level-id=") {
                    rest = format!("level-id=180;profile-id=1;tier-flag=0;{rest}");
                } else {
                    rest = level_re.replace_all(&rest, "level-id=180").into_owned();
                }

                // Rewrite sprop-sps level byte
                if let Some(sps_match) = sps_re.captures(&rest) {
                    let old_sps = sps_match.get(1).unwrap().as_str();
                    if let Some(new_sps) = rewrite_sps_level_b64(old_sps) {
                        rest = rest.replace(
                            sps_match.get(0).unwrap().as_str(),
                            &format!("sprop-sps={new_sps}"),
                        );
                    }
                }

                *line = format!("{prefix}{rest}");
            }
        }
    }

    tracing::info!("injected HEVC level-id=180 + SPS level rewrite into SDP offer");
    lines.join("\r\n")
}

/// Rewrite HEVC SPS NAL units in a raw bitstream buffer.
///
/// Finds SPS NALs (type 33) and sets `general_level_idc` to 180 (Level 6.0).
/// This is the pad probe equivalent — operates on raw bytes, not SDP text.
///
/// Returns true if any SPS was modified.
pub fn rewrite_hevc_sps_in_buffer(data: &mut [u8]) -> bool {
    let mut modified = false;
    let len = data.len();
    if len < 20 {
        return false;
    }

    let mut i = 0;
    while i + 20 < len {
        // Detect start code (00 00 01 or 00 00 00 01)
        let sc_len = if i + 3 < len && data[i] == 0 && data[i + 1] == 0 {
            if i + 3 < len && data[i + 2] == 0 && data[i + 3] == 1 {
                4
            } else if data[i + 2] == 1 {
                3
            } else {
                0
            }
        } else {
            0
        };

        if sc_len > 0 {
            let nal_pos = i + sc_len;
            if nal_pos + 1 < len {
                let nal_type = (data[nal_pos] >> 1) & 0x3F;
                if nal_type == 33 {
                    // SPS NAL — walk to general_level_idc (raw byte 12 after NAL header)
                    // Note: Python pad probe (L479) has `raw_idx < 13` which is a latent
                    // bug — it exits at 13 and the `== 12` check never matches. The SDP
                    // version (L1637) correctly uses `< 12`. We use the correct version.
                    let base = nal_pos + 2;
                    let mut raw_idx = 0;
                    let mut pos = base;
                    while raw_idx < 12 && pos + 2 < len {
                        if data[pos] == 0
                            && data[pos + 1] == 0
                            && pos + 2 < len
                            && data[pos + 2] == 3
                        {
                            raw_idx += 2;
                            pos += 3;
                        } else {
                            raw_idx += 1;
                            pos += 1;
                        }
                    }
                    if raw_idx == 12 && pos < len && data[pos] < 180 {
                        data[pos] = 180;
                        modified = true;
                    }
                }
            }
            i = nal_pos + 1;
            continue;
        }
        i += 1;
    }

    modified
}

/// Apply all SDP munging for an outgoing offer based on encoder type.
///
/// This is the top-level function that replaces the SDP manipulation
/// in `__on_offer_created`. The caller provides the encoder name to
/// determine which transformations to apply.
pub fn munge_offer_sdp(sdp: &str, encoder: &str) -> String {
    let mut result = inject_rtx_time(sdp);

    let enc_lower = encoder.to_lowercase();
    let is_h264 = enc_lower.contains("h264") || enc_lower.contains("x264");
    let is_h265 = enc_lower.contains("h265") || enc_lower.contains("x265");

    if is_h264 {
        result = inject_h264_profile(&result);
    }

    if is_h265 {
        result = inject_hevc_level(&result);
    }

    if is_h264 || is_h265 {
        result = inject_sps_pps_idr(&result);
    }

    result = inject_opus_ptime(&result);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SDP: &str = "v=0\r\n\
        o=- 123 1 IN IP4 0.0.0.0\r\n\
        s=-\r\n\
        t=0 0\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
        a=rtpmap:96 H264/90000\r\n\
        a=fmtp:96 packetization-mode=1\r\n\
        a=rtpmap:97 rtx/90000\r\n\
        a=fmtp:97 apt=96\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
        a=rtpmap:111 opus/48000/2\r\n\
        a=fmtp:111 minptime=10;sprop-stereo=0\r\n";

    #[test]
    fn test_rtx_time_injection() {
        let result = inject_rtx_time(SAMPLE_SDP);
        assert!(result.contains("apt=96;rtx-time=125"));
    }

    #[test]
    fn test_rtx_time_already_correct() {
        let sdp = SAMPLE_SDP.replace("apt=96", "apt=96;rtx-time=125");
        let result = inject_rtx_time(&sdp);
        assert_eq!(result, sdp);
    }

    #[test]
    fn test_rtx_time_wrong_value() {
        let sdp = SAMPLE_SDP.replace("apt=96", "apt=96;rtx-time=500");
        let result = inject_rtx_time(&sdp);
        assert!(result.contains("rtx-time=125"));
        assert!(!result.contains("rtx-time=500"));
    }

    #[test]
    fn test_h264_profile_injection() {
        let result = inject_h264_profile(SAMPLE_SDP);
        assert!(result.contains("profile-level-id=42e01f"));
        assert!(result.contains("level-asymmetry-allowed=1"));
        assert!(result.contains("packetization-mode=1"));
    }

    #[test]
    fn test_h264_profile_already_present() {
        let sdp = SAMPLE_SDP.replace(
            "packetization-mode=1",
            "profile-level-id=42e01f;level-asymmetry-allowed=1;packetization-mode=1",
        );
        let result = inject_h264_profile(&sdp);
        // Should not double-inject
        assert_eq!(
            result.matches("profile-level-id").count(),
            1
        );
    }

    #[test]
    fn test_sps_pps_idr_injection() {
        let result = inject_sps_pps_idr(SAMPLE_SDP);
        assert!(result.contains("sps-pps-idr-in-keyframe=1;packetization-mode="));
    }

    #[test]
    fn test_opus_ptime_injection() {
        let result = inject_opus_ptime(SAMPLE_SDP);
        assert!(result.contains("a=ptime:10"));
    }

    #[test]
    fn test_opus_ptime_no_opus() {
        let sdp = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";
        let result = inject_opus_ptime(sdp);
        assert!(!result.contains("ptime"));
    }

    #[test]
    fn test_hevc_level_injection() {
        let sdp = "v=0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
            a=rtpmap:96 H265/90000\r\n\
            a=fmtp:96 tx-mode=SRST\r\n\
            a=fmtp:97 apt=96\r\n";
        let result = inject_hevc_level(sdp);
        assert!(result.contains("level-id=180"));
        assert!(result.contains("profile-id=1"));
        // apt= line should NOT be modified
        assert!(result.contains("a=fmtp:97 apt=96"));
    }

    #[test]
    fn test_hevc_level_existing_value() {
        let sdp = "v=0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
            a=fmtp:96 level-id=150;tx-mode=SRST\r\n";
        let result = inject_hevc_level(sdp);
        assert!(result.contains("level-id=180"));
        assert!(!result.contains("level-id=150"));
    }

    #[test]
    fn test_sps_level_rewrite_b64() {
        // Construct a fake SPS with level_idc=150 at raw byte 12
        // NAL header (2 bytes) + 12 bytes profile_tier_level + level byte
        let mut sps = vec![0x42, 0x01]; // NAL header
        sps.extend_from_slice(&[0u8; 12]); // 12 bytes of profile_tier
        sps.push(150); // general_level_idc = 150 (Level 5.0)
        sps.push(0); // extra

        let b64 = base64::engine::general_purpose::STANDARD.encode(&sps);
        let result = rewrite_sps_level_b64(&b64).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&result)
            .unwrap();
        assert_eq!(decoded[14], 180); // byte 2+12 = 14
    }

    #[test]
    fn test_sps_level_already_180() {
        let mut sps = vec![0x42, 0x01];
        sps.extend_from_slice(&[0u8; 12]);
        sps.push(180); // already Level 6.0
        sps.push(0);

        let b64 = base64::engine::general_purpose::STANDARD.encode(&sps);
        assert!(rewrite_sps_level_b64(&b64).is_none());
    }

    #[test]
    fn test_hevc_sps_buffer_rewrite() {
        // Build a buffer with start code + SPS NAL (type 33)
        let mut buf = vec![0, 0, 0, 1]; // start code
        buf.push(0x42); // nal_type = (0x42 >> 1) & 0x3F = 33 (SPS)
        buf.push(0x01); // NAL header byte 2
        buf.extend_from_slice(&[0u8; 12]); // profile_tier_level padding
        buf.push(150); // general_level_idc
        buf.extend_from_slice(&[0u8; 20]); // sufficient padding

        assert!(rewrite_hevc_sps_in_buffer(&mut buf));
        // level_idc is at offset 4(start code) + 2(NAL header) + 12 = 18
        assert_eq!(buf[18], 180);
    }

    #[test]
    fn test_hevc_sps_buffer_with_epb() {
        // Buffer with emulation prevention bytes in the profile_tier_level area
        let mut buf = vec![0, 0, 0, 1]; // start code
        buf.push(0x42); // SPS NAL type
        buf.push(0x01); // NAL header byte 2
        // Insert EPB sequence: put 00 00 03 at bytes 4-5 of raw stream
        buf.extend_from_slice(&[0, 0, 0, 0]); // raw bytes 0-3
        buf.extend_from_slice(&[0, 0, 3]); // EPB (counts as raw bytes 4-5, skip byte)
        buf.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // raw bytes 6-11
        buf.push(150); // raw byte 12 = general_level_idc
        buf.extend_from_slice(&[0u8; 20]); // sufficient padding

        assert!(rewrite_hevc_sps_in_buffer(&mut buf));
        // Position: 4(sc) + 2(nal) + 4 + 3(epb) + 6 = 19
        assert_eq!(buf[19], 180);
    }

    #[test]
    fn test_munge_offer_h264() {
        let result = munge_offer_sdp(SAMPLE_SDP, "nvh264enc");
        assert!(result.contains("rtx-time=125"));
        assert!(result.contains("profile-level-id=42e01f"));
        assert!(result.contains("level-asymmetry-allowed=1"));
        assert!(result.contains("sps-pps-idr-in-keyframe=1"));
        assert!(result.contains("a=ptime:10"));
        // Should NOT contain HEVC level-id
        assert!(!result.contains("level-id=180"));
    }

    #[test]
    fn test_munge_offer_h265() {
        let sdp = "v=0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
            a=rtpmap:96 H265/90000\r\n\
            a=fmtp:96 packetization-mode=1\r\n\
            a=fmtp:97 apt=96\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=fmtp:111 minptime=10;sprop-stereo=0\r\n";
        let result = munge_offer_sdp(sdp, "nvh265enc");
        assert!(result.contains("rtx-time=125"));
        assert!(result.contains("level-id=180"));
        assert!(result.contains("sps-pps-idr-in-keyframe=1"));
        assert!(result.contains("a=ptime:10"));
        // Should NOT contain H264 profile
        assert!(!result.contains("profile-level-id=42e01f"));
    }

    #[test]
    fn test_munge_offer_vp8() {
        // VP8/VP9 — should only get rtx-time and opus ptime
        let sdp = "v=0\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
            a=rtpmap:96 VP8/90000\r\n\
            a=fmtp:97 apt=96\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=fmtp:111 minptime=10;sprop-stereo=0\r\n";
        let result = munge_offer_sdp(sdp, "vp8enc");
        assert!(result.contains("rtx-time=125"));
        assert!(result.contains("a=ptime:10"));
        assert!(!result.contains("profile-level-id"));
        assert!(!result.contains("level-id=180"));
        assert!(!result.contains("sps-pps-idr-in-keyframe"));
    }
}
