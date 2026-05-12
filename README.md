# selkies-rust

Rust rewrite of [selkies-gstreamer](https://github.com/selkies-project/selkies-gstreamer) (the Python WebRTC streaming thing). Same idea — NvFBC capture → NVENC → WebRTC to a browser — but in Rust so it doesn't eat 400MB of RAM sitting idle.

## crates

```
crates/
├── selkies-bin/        # CLI entry point
├── selkies-core/       # config, session types
├── selkies-pipeline/   # GStreamer pipeline wiring
├── selkies-signaling/  # WebSocket SDP/ICE exchange  
├── selkies-input/      # keyboard, mouse, gamepad (analog + rumble)
└── selkies-stats/      # bandwidth/fps/latency reporting
```

## build & run

```
cargo build --release

./target/release/selkies-bin \
  --display :0 \
  --signaling-port 8443 \
  --encoder nvh265enc \
  --framerate 144 \
  --bitrate 25000
```

Tested with Cloudflare TURN relay. HEVC works if the client has HW decode (most modern browsers do).

## status

WIP port — core pipeline and input handling work, signaling is functional. Not a full replacement for the Python version yet.
