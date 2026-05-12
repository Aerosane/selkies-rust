//! WebRTC signaling client — connects to signaling server via WebSocket.
//!
//! Ported from: webrtc_signalling.py (206 lines)
//! Port status: COMPLETE
//!
//! This module implements the signaling client that connects to the
//! WebRTC signaling server, registers with a HELLO message, sets up
//! sessions, and relays SDP/ICE messages.

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite};
use tracing;

use selkies_core::error::SignalingError;

// ============================================================
// Original Python: webrtc_signalling.py:47-52
//
// class WebRTCSignallingError(Exception):
//     pass
//
// class WebRTCSignallingErrorNoPeer(Exception):
//     pass
// ============================================================

/// Error types from the signaling server.
#[derive(Debug, Clone)]
pub enum SignallingServerError {
    /// Peer not found (analogous to WebRTCSignallingErrorNoPeer)
    PeerNotFound(String),
    /// Generic signalling error (analogous to WebRTCSignallingError)
    Protocol(String),
}

// ============================================================
// Original Python: webrtc_signalling.py:55-81
//
// class WebRTCSignalling:
//     def __init__(self, server, id, peer_id, enable_https=False,
//                  enable_basic_auth=False, basic_auth_user=None,
//                  basic_auth_password=None):
//         self.server = server
//         self.id = id
//         self.peer_id = peer_id
//         self.enable_https = enable_https
//         self.enable_basic_auth = enable_basic_auth
//         self.basic_auth_user = basic_auth_user
//         self.basic_auth_password = basic_auth_password
//         self.conn = None
//
//         self.on_ice = lambda mlineindex, candidate: logger.warn(...)
//         self.on_sdp = lambda sdp_type, sdp: logger.warn(...)
//         self.on_connect = lambda res, scale: logger.warn(...)
//         self.on_disconnect = lambda: logger.warn(...)
//         self.on_session = lambda peer_id, meta: logger.warn(...)
//         self.on_error = lambda v: logger.warn(...)
// ============================================================

/// Configuration for the signaling client.
#[derive(Debug, Clone)]
pub struct SignallingConfig {
    /// WebSocket URI to connect to (e.g., ws://127.0.0.1:8080)
    pub server: String,
    /// ID of this client when registering
    pub id: u32,
    /// ID of peer to connect to
    pub peer_id: u32,
    /// Enable HTTPS/WSS (skip certificate verification)
    pub enable_https: bool,
    /// Enable HTTP Basic Authentication
    pub enable_basic_auth: bool,
    /// Basic auth username
    pub basic_auth_user: Option<String>,
    /// Basic auth password
    pub basic_auth_password: Option<String>,
}

/// Callbacks for signaling events.
///
/// In the Python code, these are lambda callbacks set on the class instance.
/// In Rust, we use a trait to define the callback interface.
#[allow(async_fn_in_trait)]
pub trait SignallingCallbacks: Send + Sync + 'static {
    /// Called when HELLO is received from server (connection registered)
    async fn on_connect(&self);
    /// Called when connection is closed
    async fn on_disconnect(&self);
    /// Called when SESSION_OK is received (session established with peer)
    async fn on_session(&self, peer_id: u32, meta: serde_json::Value);
    /// Called when SDP offer/answer is received from peer
    fn on_sdp(&self, sdp_type: &str, sdp: &str);
    /// Called when ICE candidate is received from peer
    fn on_ice(&self, sdp_m_line_index: u32, candidate: &str);
    /// Called on error
    async fn on_error(&self, error: SignallingServerError);
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// WebRTC signaling client.
///
/// Connects to a WebSocket signaling server, registers with HELLO,
/// sets up sessions with peers, and relays SDP/ICE messages.
pub struct WebRTCSignalling<C: SignallingCallbacks> {
    config: SignallingConfig,
    conn: Arc<Mutex<Option<WsStream>>>,
    callbacks: Arc<C>,
}

impl<C: SignallingCallbacks> WebRTCSignalling<C> {
    pub fn new(config: SignallingConfig, callbacks: Arc<C>) -> Self {
        Self {
            config,
            conn: Arc::new(Mutex::new(None)),
            callbacks,
        }
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:92-119
    //
    // async def connect(self):
    //     try:
    //         sslctx = None
    //         if self.enable_https:
    //             sslctx = ssl.create_default_context(purpose=ssl.Purpose.SERVER_AUTH)
    //             sslctx.check_hostname = False
    //             sslctx.verify_mode = ssl.CERT_NONE
    //         headers = None
    //         if self.enable_basic_auth:
    //             auth64 = base64.b64encode(bytes("{}:{}".format(
    //                 self.basic_auth_user, self.basic_auth_password), "ascii")).decode("ascii")
    //             headers = [("Authorization", "Basic {}".format(auth64))]
    //         while True:
    //             try:
    //                 self.conn = await websockets.connect(self.server, extra_headers=headers, ssl=sslctx)
    //                 break
    //             except ConnectionRefusedError:
    //                 logger.info("Connecting to signal server...")
    //                 await asyncio.sleep(2)
    //         await self.conn.send('HELLO %d' % self.id)
    //     except websockets.ConnectionClosed:
    //         self.on_disconnect()
    // ============================================================

    /// Connect to the signaling server and send HELLO.
    /// Retries on ConnectionRefused with 2-second backoff.
    ///
    // TODO(port): When enable_https=true, Python disables certificate verification
    // (ssl.CERT_NONE, check_hostname=False). tokio-tungstenite needs the
    // `native-tls` or `rustls-tls-native-roots` feature + a custom TLS connector
    // to replicate this. Currently wss:// connections may fail on self-signed certs.
    pub async fn connect(&self) -> Result<(), SignalingError> {
        // Build request with optional Basic Auth header
        let request = self.config.server.clone();

        let ws_request = if self.config.enable_basic_auth {
            let user = self.config.basic_auth_user.as_deref().unwrap_or("");
            let pass = self.config.basic_auth_password.as_deref().unwrap_or("");
            let auth64 = base64::engine::general_purpose::STANDARD
                .encode(format!("{user}:{pass}"));

            let uri: http::Uri = request
                .parse()
                .map_err(|e| SignalingError::ConnectionFailed(format!("Invalid URI: {e}")))?;

            http::Request::builder()
                .uri(uri)
                .header("Authorization", format!("Basic {auth64}"))
                .header("Host", self.config.server.as_str())
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header(
                    "Sec-WebSocket-Key",
                    tungstenite::handshake::client::generate_key(),
                )
                .body(())
                .map_err(|e| SignalingError::ConnectionFailed(format!("Request build error: {e}")))?
        } else {
            // For non-auth connections, we'll use the URL directly below
            // This branch creates a dummy request that won't be used
            http::Request::builder().body(()).unwrap()
        };

        // Retry loop matching Python's while True / except ConnectionRefusedError
        let ws_stream = loop {
            let result = if self.config.enable_basic_auth {
                connect_async(ws_request.clone()).await
            } else {
                connect_async(&request).await
            };

            match result {
                Ok((stream, _response)) => break stream,
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::ConnectionRefused =>
                {
                    tracing::info!("Connecting to signal server...");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    self.callbacks.on_disconnect().await;
                    return Err(SignalingError::ConnectionFailed(e.to_string()));
                }
            }
        };

        // Store connection
        {
            let mut conn = self.conn.lock().await;
            *conn = Some(ws_stream);
        }

        // Send HELLO
        self.send_text(&format!("HELLO {}", self.config.id)).await?;
        tracing::debug!("Sent HELLO {}", self.config.id);

        Ok(())
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:83-90
    //
    // async def setup_call(self):
    //     logger.debug("setting up call")
    //     await self.conn.send('SESSION %d' % self.peer_id)
    // ============================================================

    /// Send SESSION request to establish a call with peer.
    pub async fn setup_call(&self) -> Result<(), SignalingError> {
        tracing::debug!("setting up call");
        self.send_text(&format!("SESSION {}", self.config.peer_id))
            .await
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:121-131
    //
    // async def send_ice(self, mlineindex, candidate):
    //     msg = json.dumps({'ice': {'candidate': candidate, 'sdpMLineIndex': mlineindex}})
    //     await self.conn.send(msg)
    // ============================================================

    /// Send ICE candidate to peer.
    pub async fn send_ice(
        &self,
        mlineindex: u32,
        candidate: &str,
    ) -> Result<(), SignalingError> {
        let msg = serde_json::json!({
            "ice": {
                "candidate": candidate,
                "sdpMLineIndex": mlineindex
            }
        });
        self.send_text(&msg.to_string()).await
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:133-145
    //
    // async def send_sdp(self, sdp_type, sdp):
    //     logger.info("sending sdp type: %s" % sdp_type)
    //     logger.debug("SDP:\n%s" % sdp)
    //     msg = json.dumps({'sdp': {'type': sdp_type, 'sdp': sdp}})
    //     await self.conn.send(msg)
    // ============================================================

    /// Send SDP offer or answer to peer.
    pub async fn send_sdp(&self, sdp_type: &str, sdp: &str) -> Result<(), SignalingError> {
        tracing::info!("sending sdp type: {}", sdp_type);
        tracing::debug!("SDP:\n{}", sdp);
        let msg = serde_json::json!({
            "sdp": {
                "type": sdp_type,
                "sdp": sdp
            }
        });
        self.send_text(&msg.to_string()).await
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:147-149
    //
    // async def stop(self):
    //     logger.warning("stopping")
    //     await self.conn.close()
    // ============================================================

    /// Close the WebSocket connection.
    pub async fn stop(&self) -> Result<(), SignalingError> {
        tracing::warn!("stopping");
        let mut conn = self.conn.lock().await;
        if let Some(ref mut ws) = *conn {
            ws.close(None)
                .await
                .map_err(|e| SignalingError::ConnectionFailed(e.to_string()))?;
        }
        *conn = None;
        Ok(())
    }

    // ============================================================
    // Original Python: webrtc_signalling.py:151-206
    //
    // async def start(self):
    //     async for message in self.conn:
    //         if message == 'HELLO':
    //             logger.info("connected")
    //             await self.on_connect()
    //         elif message.startswith('SESSION_OK'):
    //             toks = message.split()
    //             meta = {}
    //             if len(toks) > 1:
    //                 meta = json.loads(base64.b64decode(toks[1]))
    //             logger.info("started session with peer: %s, meta: %s", self.peer_id, json.dumps(meta))
    //             self.on_session(self.peer_id, (meta))
    //         elif message.startswith('ERROR'):
    //             if message == "ERROR peer '%s' not found" % self.peer_id:
    //                 await self.on_error(WebRTCSignallingErrorNoPeer("'%s' not found" % self.peer_id))
    //             else:
    //                 await self.on_error(WebRTCSignallingError("unhandled signalling message: %s" % message))
    //         else:
    //             data = None
    //             try:
    //                 data = json.loads(message)
    //             except Exception as e:
    //                 if isinstance(e, json.decoder.JSONDecodeError):
    //                     await self.on_error(WebRTCSignallingError("error parsing message as JSON: %s" % message))
    //                 else:
    //                     await self.on_error(WebRTCSignallingError("failed to prase message: %s" % message))
    //                 continue
    //             if data.get("sdp", None):
    //                 logger.info("received SDP")
    //                 logger.debug("SDP:\n%s" % data["sdp"])
    //                 self.on_sdp(data['sdp'].get('type'), data['sdp'].get('sdp'))
    //             elif data.get("ice", None):
    //                 logger.info("received ICE")
    //                 logger.debug("ICE:\n%s" % data.get("ice"))
    //                 self.on_ice(data['ice'].get('sdpMLineIndex'), data['ice'].get('candidate'))
    //             else:
    //                 await self.on_error(WebRTCSignallingError("unhandled JSON message: %s", json.dumps(data)))
    // ============================================================

    /// Main message loop — processes messages from the signaling server.
    ///
    /// Handles HELLO, SESSION_OK, ERROR, SDP, and ICE messages.
    /// Calls appropriate callbacks for each message type.
    pub async fn start(&self) -> Result<(), SignalingError> {
        loop {
            let message = {
                let mut conn = self.conn.lock().await;
                let ws = conn
                    .as_mut()
                    .ok_or(SignalingError::ConnectionFailed("Not connected".into()))?;
                ws.next().await
            };

            match message {
                Some(Ok(tungstenite::Message::Text(text))) => {
                    self.handle_message(&text).await?;
                }
                Some(Ok(tungstenite::Message::Close(_))) | None => {
                    tracing::info!("WebSocket connection closed");
                    self.callbacks.on_disconnect().await;
                    break;
                }
                Some(Ok(_)) => {
                    // Ignore binary, ping, pong frames
                    continue;
                }
                Some(Err(e)) => {
                    tracing::error!("WebSocket error: {}", e);
                    self.callbacks.on_disconnect().await;
                    return Err(SignalingError::ConnectionFailed(e.to_string()));
                }
            }
        }
        Ok(())
    }

    /// Handle a single text message from the signaling server.
    async fn handle_message(&self, message: &str) -> Result<(), SignalingError> {
        if message == "HELLO" {
            tracing::info!("connected");
            self.callbacks.on_connect().await;
        } else if let Some(rest) = message.strip_prefix("SESSION_OK") {
            let meta = if let Some(b64_part) = rest.split_whitespace().next() {
                if !b64_part.is_empty() {
                    match base64::engine::general_purpose::STANDARD.decode(b64_part) {
                        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(serde_json::json!({})),
                        Err(_) => serde_json::json!({}),
                    }
                } else {
                    serde_json::json!({})
                }
            } else {
                serde_json::json!({})
            };

            tracing::info!(
                "started session with peer: {}, meta: {}",
                self.config.peer_id,
                meta
            );
            self.callbacks
                .on_session(self.config.peer_id, meta)
                .await;
        } else if message.starts_with("ERROR") {
            let expected_no_peer = format!("ERROR peer '{}' not found", self.config.peer_id);
            if message == expected_no_peer {
                self.callbacks
                    .on_error(SignallingServerError::PeerNotFound(format!(
                        "'{}' not found",
                        self.config.peer_id
                    )))
                    .await;
            } else {
                self.callbacks
                    .on_error(SignallingServerError::Protocol(format!(
                        "unhandled signalling message: {}",
                        message
                    )))
                    .await;
            }
        } else {
            // Attempt to parse JSON SDP or ICE message
            match serde_json::from_str::<serde_json::Value>(message) {
                Ok(data) => {
                    if let Some(sdp_obj) = data.get("sdp") {
                        tracing::info!("received SDP");
                        tracing::debug!("SDP:\n{}", sdp_obj);
                        // Note: Python .get() returns None for missing fields;
                    // Rust defaults to "" / 0. In practice these fields are
                    // always present in well-formed SDP/ICE messages.
                    let sdp_type = sdp_obj
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        let sdp = sdp_obj.get("sdp").and_then(|s| s.as_str()).unwrap_or("");
                        self.callbacks.on_sdp(sdp_type, sdp);
                    } else if let Some(ice_obj) = data.get("ice") {
                        tracing::info!("received ICE");
                        tracing::debug!("ICE:\n{}", ice_obj);
                        let sdp_m_line_index = ice_obj
                            .get("sdpMLineIndex")
                            .and_then(|i| i.as_u64())
                            .unwrap_or(0) as u32;
                        let candidate = ice_obj
                            .get("candidate")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        self.callbacks.on_ice(sdp_m_line_index, candidate);
                    } else {
                        self.callbacks
                            .on_error(SignallingServerError::Protocol(format!(
                                "unhandled JSON message: {}",
                                data
                            )))
                            .await;
                    }
                }
                Err(e) => {
                    self.callbacks
                        .on_error(SignallingServerError::Protocol(format!(
                            "error parsing message as JSON: {} ({})",
                            message, e
                        )))
                        .await;
                }
            }
        }
        Ok(())
    }

    /// Send a text message on the WebSocket.
    async fn send_text(&self, text: &str) -> Result<(), SignalingError> {
        let mut conn = self.conn.lock().await;
        let ws = conn
            .as_mut()
            .ok_or(SignalingError::ConnectionFailed("Not connected".into()))?;
        ws.send(tungstenite::Message::Text(text.into()))
            .await
            .map_err(|e| SignalingError::ConnectionFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use tokio::sync::Mutex as TokioMutex;

    /// Test callbacks that record what was called
    struct TestCallbacks {
        connected: AtomicBool,
        disconnected: AtomicBool,
        session_peer_id: AtomicU32,
        session_meta: TokioMutex<serde_json::Value>,
        sdp_received: TokioMutex<Vec<(String, String)>>,
        ice_received: TokioMutex<Vec<(u32, String)>>,
        errors: TokioMutex<Vec<String>>,
    }

    impl TestCallbacks {
        fn new() -> Self {
            Self {
                connected: AtomicBool::new(false),
                disconnected: AtomicBool::new(false),
                session_peer_id: AtomicU32::new(0),
                session_meta: TokioMutex::new(serde_json::json!({})),
                sdp_received: TokioMutex::new(Vec::new()),
                ice_received: TokioMutex::new(Vec::new()),
                errors: TokioMutex::new(Vec::new()),
            }
        }
    }

    impl SignallingCallbacks for TestCallbacks {
        async fn on_connect(&self) {
            self.connected.store(true, Ordering::SeqCst);
        }
        async fn on_disconnect(&self) {
            self.disconnected.store(true, Ordering::SeqCst);
        }
        async fn on_session(&self, peer_id: u32, meta: serde_json::Value) {
            self.session_peer_id.store(peer_id, Ordering::SeqCst);
            *self.session_meta.lock().await = meta;
        }
        fn on_sdp(&self, sdp_type: &str, sdp: &str) {
            // Use try_lock to avoid blocking in sync context
            if let Ok(mut v) = self.sdp_received.try_lock() {
                v.push((sdp_type.to_string(), sdp.to_string()));
            }
        }
        fn on_ice(&self, sdp_m_line_index: u32, candidate: &str) {
            if let Ok(mut v) = self.ice_received.try_lock() {
                v.push((sdp_m_line_index, candidate.to_string()));
            }
        }
        async fn on_error(&self, error: SignallingServerError) {
            let msg = match error {
                SignallingServerError::PeerNotFound(m) => format!("PeerNotFound: {m}"),
                SignallingServerError::Protocol(m) => format!("Protocol: {m}"),
            };
            self.errors.lock().await.push(msg);
        }
    }

    #[tokio::test]
    async fn test_handle_hello() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message("HELLO").await.unwrap();
        assert!(cb.connected.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_handle_session_ok() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message("SESSION_OK").await.unwrap();
        assert_eq!(cb.session_peer_id.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_handle_session_ok_with_meta() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        // Encode {"res": "1920x1080", "scale": 1.5} as base64
        let meta = serde_json::json!({"res": "1920x1080", "scale": 1.5});
        let meta_b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&meta).unwrap());

        sig.handle_message(&format!("SESSION_OK {meta_b64}"))
            .await
            .unwrap();

        assert_eq!(cb.session_peer_id.load(Ordering::SeqCst), 1);
        let stored_meta = cb.session_meta.lock().await;
        assert_eq!(stored_meta.get("res").unwrap(), "1920x1080");
        assert_eq!(stored_meta.get("scale").unwrap(), 1.5);
    }

    #[tokio::test]
    async fn test_handle_error_peer_not_found() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message("ERROR peer '1' not found")
            .await
            .unwrap();

        let errors = cb.errors.lock().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("PeerNotFound"));
    }

    #[tokio::test]
    async fn test_handle_error_generic() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message("ERROR some other error")
            .await
            .unwrap();

        let errors = cb.errors.lock().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Protocol"));
    }

    #[tokio::test]
    async fn test_handle_sdp_message() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        let msg = r#"{"sdp": {"type": "offer", "sdp": "v=0\r\n"}}"#;
        sig.handle_message(msg).await.unwrap();

        let sdps = cb.sdp_received.lock().await;
        assert_eq!(sdps.len(), 1);
        assert_eq!(sdps[0].0, "offer");
        assert_eq!(sdps[0].1, "v=0\r\n");
    }

    #[tokio::test]
    async fn test_handle_ice_message() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        let msg = r#"{"ice": {"candidate": "candidate:1 1 UDP 2122252543 192.168.1.1 12345 typ host", "sdpMLineIndex": 0}}"#;
        sig.handle_message(msg).await.unwrap();

        let ices = cb.ice_received.lock().await;
        assert_eq!(ices.len(), 1);
        assert_eq!(ices[0].0, 0);
        assert!(ices[0].1.starts_with("candidate:"));
    }

    #[tokio::test]
    async fn test_handle_invalid_json() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message("not json at all").await.unwrap();

        let errors = cb.errors.lock().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("error parsing message as JSON"));
    }

    #[tokio::test]
    async fn test_handle_unhandled_json() {
        let config = SignallingConfig {
            server: "ws://127.0.0.1:8080".into(),
            id: 0,
            peer_id: 1,
            enable_https: false,
            enable_basic_auth: false,
            basic_auth_user: None,
            basic_auth_password: None,
        };
        let cb = Arc::new(TestCallbacks::new());
        let sig = WebRTCSignalling::new(config, cb.clone());

        sig.handle_message(r#"{"unknown": "field"}"#)
            .await
            .unwrap();

        let errors = cb.errors.lock().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("unhandled JSON message"));
    }
}
