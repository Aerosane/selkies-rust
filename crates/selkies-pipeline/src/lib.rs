//! selkies-pipeline — GStreamer WebRTC pipeline
//!
//! Architecture:
//!   nvfbcenc (NvFBC capture + NVENC encode, zero-copy) → h264/h265parse
//!   → rtph264/5pay → webrtcbin → ICE/DTLS → client
//!
//!   Audio: pulsesrc → opusenc → rtpopuspay → webrtcbin
//!
//! Ported from: gstwebrtc_app.py (GSTWebRTCApp, 1917 lines)

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use selkies_core::sdp::munge_offer_sdp;
use selkies_stats::EncoderType;
use tracing::{debug, error, info, warn};

// ── Video codec ────────────────────────────────────────────────────────────────

/// Video codec selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

impl VideoCodec {
    /// `codec` property value for `nvfbcenc`.
    pub fn nvfbcenc_codec_id(self) -> i32 {
        match self {
            VideoCodec::H264 => 0,
            VideoCodec::H265 => 1,
        }
    }

    /// Encoding name used in RTP caps / SDP.
    pub fn encoding_name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H264",
            VideoCodec::H265 => "H265",
        }
    }

    /// Map to selkies_stats EncoderType for FEC/bitrate calculations.
    pub fn encoder_type(self) -> EncoderType {
        match self {
            VideoCodec::H264 => EncoderType::NvH264,
            VideoCodec::H265 => EncoderType::NvH265,
        }
    }
}

// ── PipelineConfig ─────────────────────────────────────────────────────────────

/// Configuration passed to [`SelkiesPipeline::new`].
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Video codec (H264 or H265 via nvfbcenc).
    pub codec: VideoCodec,
    /// Target video bitrate in kbps.
    pub video_bitrate: i32,
    /// Capture framerate.
    pub framerate: i32,
    /// Show mouse cursor in capture.
    pub show_pointer: bool,
    /// PulseAudio source device name (None = default sink monitor).
    pub audio_device: Option<String>,
    /// Audio channels (default: 2).
    pub audio_channels: u32,
    /// Target audio bitrate in bps (default: 128 000).
    pub audio_bitrate: i32,
    /// STUN server URI, e.g. "stun://stun.l.google.com:19302"
    pub stun_server: Option<String>,
    /// TURN server URIs with credentials embedded.
    pub turn_servers: Vec<String>,
    /// Keyframe distance in seconds (-1.0 = no forced keyframes).
    pub keyframe_distance: f64,
    /// Enable GCC congestion control / bandwidth estimation.
    pub congestion_control: bool,
    /// Expected video packet loss % for FEC bitrate compensation.
    pub video_packetloss_percent: f64,
    /// Expected audio packet loss % for FEC bitrate compensation.
    pub audio_packetloss_percent: f64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H265,
            video_bitrate: 8000,
            framerate: 60,
            show_pointer: true,
            audio_device: None,
            audio_channels: 2,
            audio_bitrate: 128_000,
            stun_server: None,
            turn_servers: Vec::new(),
            keyframe_distance: -1.0,
            congestion_control: false,
            video_packetloss_percent: 0.0,
            audio_packetloss_percent: 0.0,
        }
    }
}

// ── DerivedParams ──────────────────────────────────────────────────────────────

/// Derived pipeline parameters computed from [`PipelineConfig`].
#[derive(Debug, Clone)]
pub struct DerivedParams {
    /// Encoder type (for FEC / bitrate-table lookups).
    pub encoder_type: EncoderType,
    /// FEC-compensated video bitrate (kbps).
    pub fec_video_bitrate: u32,
    /// FEC-compensated audio bitrate (bps).
    pub fec_audio_bitrate: u32,
    /// Keyframe frame distance (-1 = auto/disabled).
    pub keyframe_frame_distance: i32,
    /// Minimum allowed keyframe interval in frames.
    pub min_keyframe_frame_distance: u32,
    /// VBV buffer size multiplier (1.5× optimal, 3.0× when periodic keyframes).
    pub vbv_multiplier: f64,
}

impl DerivedParams {
    /// Compute derived parameters from a pipeline config.
    pub fn from_config(config: &PipelineConfig) -> Self {
        let encoder_type = config.codec.encoder_type();

        let fec_video = selkies_stats::fec_video_bitrate(
            config.video_bitrate as u32,
            config.video_packetloss_percent,
        );
        let fec_audio = selkies_stats::fec_audio_bitrate(
            config.audio_bitrate as u32,
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

        let vbv_mult = if config.keyframe_distance < 0.0 { 1.5 } else { 3.0 };

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

// ── Pipeline internals ─────────────────────────────────────────────────────────

/// Callbacks invoked by the pipeline on the calling task.
pub struct PipelineCallbacks {
    /// Called when webrtcbin produces a local SDP offer (already munged).
    pub on_offer: Box<dyn Fn(String) + Send + Sync>,
    /// Called for each local ICE candidate (mline_index, candidate string).
    pub on_ice_candidate: Box<dyn Fn(u32, String) + Send + Sync>,
}

struct PipelineState {
    bitrate: i32,
}

/// The main GStreamer pipeline.
///
/// Wraps `nvfbcenc ! parse ! rtppay ! webrtcbin` + `pulsesrc ! opusenc ! rtpopuspay ! webrtcbin`.
pub struct SelkiesPipeline {
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    /// The `nvfbcenc` element — held for dynamic bitrate/framerate updates.
    encoder: gst::Element,
    /// Retained for codec/framerate queries.
    pub config: PipelineConfig,
    state: Arc<Mutex<PipelineState>>,
}

impl SelkiesPipeline {
    /// Build the full pipeline. Does NOT set state to PLAYING yet.
    pub fn new(config: PipelineConfig, callbacks: Arc<PipelineCallbacks>) -> Result<Self> {
        gst::init().context("GStreamer init failed")?;

        let pipeline = gst::Pipeline::new();

        // ── Video branch ───────────────────────────────────────────────────
        let encoder = gst::ElementFactory::make("nvfbcenc")
            .name("enc")
            .property("framerate", config.framerate)
            .property("bitrate", config.video_bitrate)
            .property("codec", config.codec.nvfbcenc_codec_id())
            .property("show-pointer", config.show_pointer)
            .property("push-model", true)
            .build()
            .context("nvfbcenc not found — build gpu-streaming/gstreamer-plugin first")?;

        let (video_parse, video_pay) = match config.codec {
            VideoCodec::H264 => {
                let parse = gst::ElementFactory::make("h264parse")
                    .property("config-interval", -1i32)
                    .build()
                    .context("h264parse")?;
                let pay = gst::ElementFactory::make("rtph264pay")
                    .name("videopay")
                    .property("config-interval", -1i32)
                    .property("aggregate-mode", "zero-latency")
                    .build()
                    .context("rtph264pay")?;
                (parse, pay)
            }
            VideoCodec::H265 => {
                let parse = gst::ElementFactory::make("h265parse")
                    .property("config-interval", -1i32)
                    .build()
                    .context("h265parse")?;
                let pay = gst::ElementFactory::make("rtph265pay")
                    .name("videopay")
                    .property("config-interval", -1i32)
                    .property("aggregate-mode", "zero-latency")
                    .build()
                    .context("rtph265pay")?;
                (parse, pay)
            }
        };

        // ── Audio branch ───────────────────────────────────────────────────
        let mut pulsesrc_builder = gst::ElementFactory::make("pulsesrc").name("audiosrc");
        if let Some(ref dev) = config.audio_device {
            pulsesrc_builder = pulsesrc_builder.property("device", dev.as_str());
        }
        let pulsesrc = pulsesrc_builder.build().context("pulsesrc")?;

        let audioconvert = gst::ElementFactory::make("audioconvert")
            .build()
            .context("audioconvert")?;
        let audioresample = gst::ElementFactory::make("audioresample")
            .build()
            .context("audioresample")?;

        let opusenc = gst::ElementFactory::make("opusenc")
            .name("opusenc")
            .property("bitrate", config.audio_bitrate)
            .property("audio-type", "voice")
            .property("frame-size", 10i32)
            .build()
            .context("opusenc")?;

        let rtpopuspay = gst::ElementFactory::make("rtpopuspay")
            .name("audiopay")
            .property("pt", 111u32)
            .build()
            .context("rtpopuspay")?;

        // ── WebRTCbin ──────────────────────────────────────────────────────
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .name("sendrecv")
            .property_from_str("bundle-policy", "max-bundle")
            .build()
            .context("webrtcbin")?;

        if let Some(ref stun) = config.stun_server {
            webrtcbin.set_property("stun-server", stun.as_str());
        }
        for turn in &config.turn_servers {
            webrtcbin.emit_by_name::<bool>("add-turn-server", &[&turn.as_str()]);
        }

        // Capsfilter elements for pay → webrtcbin (avoids Pad::link_filtered trait ambiguity)
        let enc_name = config.codec.encoding_name();
        let video_caps = gst::Caps::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", enc_name)
            .field("payload", 96i32)
            .build();
        let video_capsfilter = gst::ElementFactory::make("capsfilter")
            .name("videocapsfilter")
            .property("caps", &video_caps)
            .build()
            .context("capsfilter (video)")?;

        let audio_caps = gst::Caps::builder("application/x-rtp")
            .field("media", "audio")
            .field("encoding-name", "OPUS")
            .field("payload", 111i32)
            .build();
        let audio_capsfilter = gst::ElementFactory::make("capsfilter")
            .name("audiocapsfilter")
            .property("caps", &audio_caps)
            .build()
            .context("capsfilter (audio)")?;

        // Add all elements
        pipeline.add_many([
            &encoder, &video_parse, &video_pay, &video_capsfilter,
            &pulsesrc, &audioconvert, &audioresample, &opusenc, &rtpopuspay, &audio_capsfilter,
            &webrtcbin,
        ])?;

        // Link video: nvfbcenc → parse → pay → capsfilter
        gst::Element::link_many([&encoder, &video_parse, &video_pay, &video_capsfilter])?;
        // Link audio: pulsesrc → convert → resample → opusenc → pay → capsfilter
        gst::Element::link_many([
            &pulsesrc, &audioconvert, &audioresample, &opusenc, &rtpopuspay, &audio_capsfilter,
        ])?;

        // capsfilter src → webrtcbin sink (request pads)
        {
            let sink = webrtcbin
                .request_pad_simple("sink_%u")
                .ok_or_else(|| anyhow!("webrtcbin: no sink pad for video"))?;
            video_capsfilter
                .static_pad("src")
                .ok_or_else(|| anyhow!("video_capsfilter: no src pad"))?
                .link(&sink)?;
        }
        {
            let sink = webrtcbin
                .request_pad_simple("sink_%u")
                .ok_or_else(|| anyhow!("webrtcbin: no sink pad for audio"))?;
            audio_capsfilter
                .static_pad("src")
                .ok_or_else(|| anyhow!("audio_capsfilter: no src pad"))?
                .link(&sink)?;
        }

        // ── WebRTC signal handlers ─────────────────────────────────────────
        let cb = Arc::clone(&callbacks);
        let codec = config.codec;
        let video_bitrate = config.video_bitrate;

        webrtcbin.connect("on-negotiation-needed", false, move |values| {
            let webrtc = values[0].get::<gst::Element>().unwrap();
            let cb2 = Arc::clone(&cb);
            let webrtc_promise = webrtc.clone();

            let promise = gst::Promise::with_change_func(move |reply| {
                let reply = match reply {
                    Ok(Some(s)) => s,
                    _ => { error!("create-offer promise failed"); return; }
                };
                let offer = match reply.get::<gst_webrtc::WebRTCSessionDescription>("offer") {
                    Ok(o) => o,
                    Err(e) => { error!("no offer in reply: {e}"); return; }
                };

                let raw_sdp = offer.sdp().as_text().unwrap_or_default();
                let munged = munge_offer_sdp(&raw_sdp, &format!("{:?}", codec).to_lowercase());
                debug!("munged offer SDP:\n{munged}");

                let munged_sdp = gst_sdp::SDPMessage::parse_buffer(munged.as_bytes())
                    .unwrap_or_else(|_| offer.sdp().to_owned());
                let local_desc = gst_webrtc::WebRTCSessionDescription::new(
                    gst_webrtc::WebRTCSDPType::Offer,
                    munged_sdp,
                );
                webrtc_promise.emit_by_name::<()>(
                    "set-local-description",
                    &[&local_desc, &None::<gst::Promise>],
                );
                (cb2.on_offer)(munged);
            });

            webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
            None
        });

        let cb_ice = Arc::clone(&callbacks);
        webrtcbin.connect("on-ice-candidate", false, move |values| {
            let sdp_mline_index = values[1].get::<u32>().unwrap_or(0);
            let candidate = values[2].get::<String>().unwrap_or_default();
            (cb_ice.on_ice_candidate)(sdp_mline_index, candidate);
            None
        });

        // Bus watcher — guard must be retained to keep the watch alive
        let bus = pipeline.bus().unwrap();
        let _bus_watch = bus.add_watch(move |_, msg| {
            use gst::MessageView;
            match msg.view() {
                MessageView::Error(e) => {
                    error!("GStreamer bus error: {} — {:?}", e.error(), e.debug());
                }
                MessageView::Warning(w) => {
                    warn!("GStreamer bus warning: {} — {:?}", w.error(), w.debug());
                }
                MessageView::Eos(_) => {
                    warn!("GStreamer EOS");
                }
                MessageView::StateChanged(s)
                    if s.src().map(|e| e.name()) == Some("pipeline0".into()) =>
                {
                    info!("Pipeline state: {:?} → {:?}", s.old(), s.current());
                }
                _ => {}
            }
            gst::glib::ControlFlow::Continue
        })?;

        Ok(Self {
            pipeline,
            webrtcbin,
            encoder,
            config,
            state: Arc::new(Mutex::new(PipelineState { bitrate: video_bitrate })),
        })
    }

    /// Start streaming — transitions pipeline to PLAYING.
    pub fn start(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| anyhow!("Failed to set pipeline to PLAYING"))?;
        info!("Pipeline started (PLAYING)");
        Ok(())
    }

    /// Stop streaming — transitions pipeline to NULL.
    pub fn stop(&self) -> Result<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map_err(|_| anyhow!("Failed to set pipeline to NULL"))?;
        info!("Pipeline stopped (NULL)");
        Ok(())
    }

    /// Set remote SDP answer from the peer.
    pub fn set_remote_description(&self, sdp_text: &str) -> Result<()> {
        let sdp = gst_sdp::SDPMessage::parse_buffer(sdp_text.as_bytes())
            .map_err(|e| anyhow!("SDP parse error: {e:?}"))?;
        let answer = gst_webrtc::WebRTCSessionDescription::new(
            gst_webrtc::WebRTCSDPType::Answer,
            sdp,
        );
        self.webrtcbin
            .emit_by_name::<()>("set-remote-description", &[&answer, &None::<gst::Promise>]);
        Ok(())
    }

    /// Add a remote ICE candidate from the peer.
    pub fn add_ice_candidate(&self, sdp_mline_index: u32, candidate: &str) -> Result<()> {
        self.webrtcbin
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_mline_index, &candidate]);
        Ok(())
    }

    /// Dynamically update video bitrate (kbps). Takes effect on the next encoded frame.
    pub fn set_bitrate(&self, kbps: i32) {
        let kbps = kbps.clamp(500, 50_000);
        self.encoder.set_property("bitrate", kbps);
        self.state.lock().unwrap().bitrate = kbps;
        debug!("Bitrate updated → {kbps} kbps");
    }

    /// Send a force-IDR / keyframe request upstream to nvfbcenc.
    /// Call this when a PLI or FIR RTCP packet is received from the peer.
    pub fn request_keyframe(&self) {
        use gstreamer_video::UpstreamForceKeyUnitEvent;
        let event = UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        if let Some(src_pad) = self.encoder.static_pad("src") {
            src_pad.send_event(event);
            debug!("Force-IDR event sent");
        }
    }

    /// Update TURN server list (e.g. after Cloudflare credential refresh).
    pub fn update_turn_servers(&self, servers: &[String]) {
        for s in servers {
            self.webrtcbin.emit_by_name::<bool>("add-turn-server", &[&s.as_str()]);
        }
    }

    /// Current video bitrate in kbps.
    pub fn bitrate(&self) -> i32 {
        self.state.lock().unwrap().bitrate
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let c = PipelineConfig::default();
        assert_eq!(c.framerate, 60);
        assert_eq!(c.video_bitrate, 8000);
        assert_eq!(c.audio_bitrate, 128_000);
        assert_eq!(c.codec, VideoCodec::H265);
    }

    #[test]
    fn test_derived_params_defaults() {
        let c = PipelineConfig::default();
        let d = DerivedParams::from_config(&c);
        assert_eq!(d.encoder_type, EncoderType::NvH265);
        assert_eq!(d.fec_video_bitrate, 8000);
        assert_eq!(d.keyframe_frame_distance, -1);
        assert_eq!(d.vbv_multiplier, 1.5);
    }

    #[test]
    fn test_derived_params_with_fec() {
        let c = PipelineConfig {
            video_bitrate: 25000,
            video_packetloss_percent: 5.0,
            audio_bitrate: 96000,
            audio_packetloss_percent: 5.0,
            ..PipelineConfig::default()
        };
        let d = DerivedParams::from_config(&c);
        assert_eq!(d.fec_video_bitrate, 23809);
        assert_eq!(d.fec_audio_bitrate, 100800);
    }

    #[test]
    fn test_derived_params_with_keyframes() {
        let c = PipelineConfig {
            framerate: 144,
            keyframe_distance: 2.0,
            ..PipelineConfig::default()
        };
        let d = DerivedParams::from_config(&c);
        assert_eq!(d.keyframe_frame_distance, 288);
        assert_eq!(d.vbv_multiplier, 3.0);
    }

    #[test]
    fn test_derived_params_min_keyframe_distance() {
        let c = PipelineConfig {
            framerate: 30,
            keyframe_distance: 0.5,
            ..PipelineConfig::default()
        };
        let d = DerivedParams::from_config(&c);
        assert_eq!(d.keyframe_frame_distance, 60);
    }

    #[test]
    fn test_codec_h264_props() {
        assert_eq!(VideoCodec::H264.nvfbcenc_codec_id(), 0);
        assert_eq!(VideoCodec::H264.encoding_name(), "H264");
        assert_eq!(VideoCodec::H264.encoder_type(), EncoderType::NvH264);
    }

    #[test]
    fn test_codec_h265_props() {
        assert_eq!(VideoCodec::H265.nvfbcenc_codec_id(), 1);
        assert_eq!(VideoCodec::H265.encoding_name(), "H265");
        assert_eq!(VideoCodec::H265.encoder_type(), EncoderType::NvH265);
    }
}
