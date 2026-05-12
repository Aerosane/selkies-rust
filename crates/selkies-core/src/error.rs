use thiserror::Error;

#[derive(Error, Debug)]
pub enum SignalingError {
    #[error("WebSocket connection failed: {0}")]
    ConnectionFailed(String),
    #[error("No peer connected")]
    NoPeer,
    #[error("Protocol error: {msg}")]
    Protocol { msg: String },
    #[error("Authentication failed")]
    AuthFailed,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Error, Debug)]
pub enum PipelineError {
    #[error("GStreamer error: {0}")]
    Gst(String),
    #[error("Element not found: {0}")]
    ElementNotFound(String),
    #[error("Encoder {encoder} does not support property {property}")]
    UnsupportedProperty { encoder: String, property: String },
    #[error("Pipeline state change failed")]
    StateChangeFailed,
}

#[derive(Error, Debug)]
pub enum InputError {
    #[error("X11 connection failed: {0}")]
    X11Connection(String),
    #[error("Invalid input message: {0}")]
    InvalidMessage(String),
    #[error("Gamepad socket error: {0}")]
    GamepadSocket(#[from] std::io::Error),
}

#[derive(Error, Debug)]
pub enum OracleError {
    #[error("Recording error: {0}")]
    Recording(String),
    #[error("Replay error: {0}")]
    Replay(String),
    #[error("Diff found {count} divergences")]
    Divergence { count: usize },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
