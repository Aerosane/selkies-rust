use serde::{Deserialize, Serialize};

/// Data channel message sent from server to client (JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    #[serde(rename = "pipeline")]
    Pipeline { status: String },

    #[serde(rename = "clipboard")]
    Clipboard { content: String },

    #[serde(rename = "cursor")]
    Cursor {
        curdata: String,
        handle: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        r#override: Option<String>,
        hotspot: CursorHotspot,
    },

    #[serde(rename = "gpu_stats")]
    GpuStats {
        load: f64,
        memory_total: f64,
        memory_used: f64,
    },

    #[serde(rename = "system")]
    System { action: String },

    #[serde(rename = "ping")]
    Ping { start_time: f64 },

    #[serde(rename = "latency_measurement")]
    LatencyMeasurement { latency_ms: f64 },

    #[serde(rename = "system_stats")]
    SystemStats {
        cpu_percent: f64,
        mem_total: u64,
        mem_used: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorHotspot {
    pub x: i32,
    pub y: i32,
}

/// Data channel message from client to server (CSV-encoded text)
#[derive(Debug, Clone)]
pub enum ClientMessage {
    KeyDown { keysym: u32 },
    KeyUp { keysym: u32 },
    KeyReset,
    MouseAbsolute { x: i32, y: i32, button_mask: u8, scroll_mag: i32 },
    MouseRelative { dx: i32, dy: i32, button_mask: u8, scroll_mag: i32 },
    PointerVisibility { visible: bool },
    SetVideoBitrate { bitrate_kbps: u32 },
    SetAudioBitrate { bitrate_bps: u32 },
    ClipboardRead,
    ClipboardWrite { data_b64: String },
    Resize { resolution: String },
    DpiScale { scale: f64 },
    JoystickConnect { num: u8, name_b64: String, num_axes: u8, num_btns: u8 },
    JoystickDisconnect { num: u8 },
    JoystickButton { js_num: u8, btn_num: u8, btn_val: i16 },
    JoystickAxis { js_num: u8, axis_num: u8, axis_val: f64 },
    Pong,
    SetFramerate { fps: u32 },
    SetResize { enabled: bool, resolution: String },
    ClientFps { fps: f64 },
    ClientLatency { latency_ms: f64 },
    StatsVideo { json: String },
    StatsAudio { json: String },
}

impl ClientMessage {
    /// Parse a CSV-encoded client message
    // Original Python: webrtc_input.py:577 on_message()
    // toks = msg.split(",")
    pub fn parse(msg: &str) -> Result<Self, String> {
        let toks: Vec<&str> = msg.splitn(5, ',').collect();
        if toks.is_empty() {
            return Err("Empty message".into());
        }

        match toks[0] {
            "kd" => {
                let keysym = toks.get(1).ok_or("kd: missing keysym")?
                    .parse::<u32>().map_err(|e| format!("kd: invalid keysym: {e}"))?;
                Ok(ClientMessage::KeyDown { keysym })
            }
            "ku" => {
                let keysym = toks.get(1).ok_or("ku: missing keysym")?
                    .parse::<u32>().map_err(|e| format!("ku: invalid keysym: {e}"))?;
                Ok(ClientMessage::KeyUp { keysym })
            }
            "kr" => Ok(ClientMessage::KeyReset),
            "m" => {
                // m,x,y,button_mask,scroll_mag
                let parts: Vec<&str> = msg.splitn(5, ',').collect();
                let x = parts.get(1).ok_or("m: missing x")?.parse::<i32>().map_err(|e| format!("m: {e}"))?;
                let y = parts.get(2).ok_or("m: missing y")?.parse::<i32>().map_err(|e| format!("m: {e}"))?;
                let button_mask = parts.get(3).ok_or("m: missing button_mask")?.parse::<u8>().map_err(|e| format!("m: {e}"))?;
                let scroll_mag = parts.get(4).map(|s| s.parse::<i32>().unwrap_or(0)).unwrap_or(0);
                Ok(ClientMessage::MouseAbsolute { x, y, button_mask, scroll_mag })
            }
            "m2" => {
                let parts: Vec<&str> = msg.splitn(5, ',').collect();
                let dx = parts.get(1).ok_or("m2: missing dx")?.parse::<i32>().map_err(|e| format!("m2: {e}"))?;
                let dy = parts.get(2).ok_or("m2: missing dy")?.parse::<i32>().map_err(|e| format!("m2: {e}"))?;
                let button_mask = parts.get(3).ok_or("m2: missing button_mask")?.parse::<u8>().map_err(|e| format!("m2: {e}"))?;
                let scroll_mag = parts.get(4).map(|s| s.parse::<i32>().unwrap_or(0)).unwrap_or(0);
                Ok(ClientMessage::MouseRelative { dx, dy, button_mask, scroll_mag })
            }
            "p" => {
                let visible = toks.get(1).ok_or("p: missing value")? == &"1";
                Ok(ClientMessage::PointerVisibility { visible })
            }
            "vb" => {
                let bitrate = toks.get(1).ok_or("vb: missing bitrate")?
                    .parse::<u32>().map_err(|e| format!("vb: {e}"))?;
                Ok(ClientMessage::SetVideoBitrate { bitrate_kbps: bitrate })
            }
            "ab" => {
                let bitrate = toks.get(1).ok_or("ab: missing bitrate")?
                    .parse::<u32>().map_err(|e| format!("ab: {e}"))?;
                Ok(ClientMessage::SetAudioBitrate { bitrate_bps: bitrate })
            }
            "cr" => Ok(ClientMessage::ClipboardRead),
            "cw" => {
                let data = toks.get(1).ok_or("cw: missing data")?.to_string();
                Ok(ClientMessage::ClipboardWrite { data_b64: data })
            }
            "r" => {
                let res = toks.get(1).ok_or("r: missing resolution")?.to_string();
                Ok(ClientMessage::Resize { resolution: res })
            }
            "s" => {
                let scale = toks.get(1).ok_or("s: missing scale")?
                    .parse::<f64>().map_err(|e| format!("s: {e}"))?;
                Ok(ClientMessage::DpiScale { scale })
            }
            "pong" => Ok(ClientMessage::Pong),
            "_arg_fps" => {
                let fps = toks.get(1).ok_or("_arg_fps: missing fps")?
                    .parse::<u32>().map_err(|e| format!("_arg_fps: {e}"))?;
                Ok(ClientMessage::SetFramerate { fps })
            }
            "_f" => {
                let fps = toks.get(1).ok_or("_f: missing fps")?
                    .parse::<f64>().map_err(|e| format!("_f: {e}"))?;
                Ok(ClientMessage::ClientFps { fps })
            }
            "_l" => {
                let latency = toks.get(1).ok_or("_l: missing latency")?
                    .parse::<f64>().map_err(|e| format!("_l: {e}"))?;
                Ok(ClientMessage::ClientLatency { latency_ms: latency })
            }
            "js" => {
                let subcmd = toks.get(1).ok_or("js: missing subcommand")?;
                let all_parts: Vec<&str> = msg.splitn(6, ',').collect();
                match *subcmd {
                    "c" => {
                        let num = all_parts.get(2).ok_or("js,c: missing num")?.parse::<u8>().map_err(|e| format!("js,c: {e}"))?;
                        let name_b64 = all_parts.get(3).ok_or("js,c: missing name")?.to_string();
                        let num_axes = all_parts.get(4).ok_or("js,c: missing axes")?.parse::<u8>().map_err(|e| format!("js,c: {e}"))?;
                        let num_btns = all_parts.get(5).ok_or("js,c: missing btns")?.parse::<u8>().map_err(|e| format!("js,c: {e}"))?;
                        Ok(ClientMessage::JoystickConnect { num, name_b64, num_axes, num_btns })
                    }
                    "d" => {
                        let num = all_parts.get(2).ok_or("js,d: missing num")?.parse::<u8>().map_err(|e| format!("js,d: {e}"))?;
                        Ok(ClientMessage::JoystickDisconnect { num })
                    }
                    "b" => {
                        let js_num = all_parts.get(2).ok_or("js,b: missing js_num")?.parse::<u8>().map_err(|e| format!("js,b: {e}"))?;
                        let btn_num = all_parts.get(3).ok_or("js,b: missing btn_num")?.parse::<u8>().map_err(|e| format!("js,b: {e}"))?;
                        let btn_val = all_parts.get(4).ok_or("js,b: missing btn_val")?
                            .parse::<f64>().map_err(|e| format!("js,b: {e}"))? as i16;
                        Ok(ClientMessage::JoystickButton { js_num, btn_num, btn_val })
                    }
                    "a" => {
                        let js_num = all_parts.get(2).ok_or("js,a: missing js_num")?.parse::<u8>().map_err(|e| format!("js,a: {e}"))?;
                        let axis_num = all_parts.get(3).ok_or("js,a: missing axis_num")?.parse::<u8>().map_err(|e| format!("js,a: {e}"))?;
                        let axis_val = all_parts.get(4).ok_or("js,a: missing axis_val")?
                            .parse::<f64>().map_err(|e| format!("js,a: {e}"))?;
                        Ok(ClientMessage::JoystickAxis { js_num, axis_num, axis_val })
                    }
                    _ => Err(format!("js: unknown subcommand: {subcmd}")),
                }
            }
            "_stats_video" => {
                let json = toks.get(1).ok_or("_stats_video: missing json")?.to_string();
                Ok(ClientMessage::StatsVideo { json })
            }
            "_stats_audio" => {
                let json = toks.get(1).ok_or("_stats_audio: missing json")?.to_string();
                Ok(ClientMessage::StatsAudio { json })
            }
            "_arg_resize" => {
                let enabled = toks.get(1).ok_or("_arg_resize: missing enabled")? == &"true";
                let resolution = toks.get(2).unwrap_or(&"").to_string();
                Ok(ClientMessage::SetResize { enabled, resolution })
            }
            other => Err(format!("Unknown command: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_down() {
        let msg = ClientMessage::parse("kd,65507").unwrap();
        match msg {
            ClientMessage::KeyDown { keysym } => assert_eq!(keysym, 65507),
            _ => panic!("Expected KeyDown, got {:?}", msg),
        }
    }

    #[test]
    fn test_parse_key_up() {
        let msg = ClientMessage::parse("ku,65507").unwrap();
        match msg {
            ClientMessage::KeyUp { keysym } => assert_eq!(keysym, 65507),
            _ => panic!("Expected KeyUp"),
        }
    }

    #[test]
    fn test_parse_mouse_absolute() {
        let msg = ClientMessage::parse("m,500,300,1,0").unwrap();
        match msg {
            ClientMessage::MouseAbsolute { x, y, button_mask, scroll_mag } => {
                assert_eq!(x, 500);
                assert_eq!(y, 300);
                assert_eq!(button_mask, 1);
                assert_eq!(scroll_mag, 0);
            }
            _ => panic!("Expected MouseAbsolute"),
        }
    }

    #[test]
    fn test_parse_mouse_relative() {
        let msg = ClientMessage::parse("m2,-5,10,0,0").unwrap();
        match msg {
            ClientMessage::MouseRelative { dx, dy, button_mask, scroll_mag } => {
                assert_eq!(dx, -5);
                assert_eq!(dy, 10);
                assert_eq!(button_mask, 0);
                assert_eq!(scroll_mag, 0);
            }
            _ => panic!("Expected MouseRelative"),
        }
    }

    #[test]
    fn test_parse_joystick_button() {
        let msg = ClientMessage::parse("js,b,0,3,1").unwrap();
        match msg {
            ClientMessage::JoystickButton { js_num, btn_num, btn_val } => {
                assert_eq!(js_num, 0);
                assert_eq!(btn_num, 3);
                assert_eq!(btn_val, 1);
            }
            _ => panic!("Expected JoystickButton"),
        }
    }

    #[test]
    fn test_parse_joystick_axis() {
        let msg = ClientMessage::parse("js,a,0,2,0.75").unwrap();
        match msg {
            ClientMessage::JoystickAxis { js_num, axis_num, axis_val } => {
                assert_eq!(js_num, 0);
                assert_eq!(axis_num, 2);
                assert!((axis_val - 0.75).abs() < f64::EPSILON);
            }
            _ => panic!("Expected JoystickAxis"),
        }
    }

    #[test]
    fn test_parse_joystick_connect() {
        let msg = ClientMessage::parse("js,c,0,WGJveCBDb250cm9sbGVy,4,11").unwrap();
        match msg {
            ClientMessage::JoystickConnect { num, name_b64, num_axes, num_btns } => {
                assert_eq!(num, 0);
                assert_eq!(name_b64, "WGJveCBDb250cm9sbGVy");
                assert_eq!(num_axes, 4);
                assert_eq!(num_btns, 11);
            }
            _ => panic!("Expected JoystickConnect"),
        }
    }

    #[test]
    fn test_parse_pong() {
        let msg = ClientMessage::parse("pong").unwrap();
        assert!(matches!(msg, ClientMessage::Pong));
    }

    #[test]
    fn test_parse_set_bitrate() {
        let msg = ClientMessage::parse("vb,25000").unwrap();
        match msg {
            ClientMessage::SetVideoBitrate { bitrate_kbps } => assert_eq!(bitrate_kbps, 25000),
            _ => panic!("Expected SetVideoBitrate"),
        }
    }

    #[test]
    fn test_parse_clipboard_write() {
        let msg = ClientMessage::parse("cw,SGVsbG8gV29ybGQ=").unwrap();
        match msg {
            ClientMessage::ClipboardWrite { data_b64 } => assert_eq!(data_b64, "SGVsbG8gV29ybGQ="),
            _ => panic!("Expected ClipboardWrite"),
        }
    }

    #[test]
    fn test_parse_unknown_command() {
        let result = ClientMessage::parse("xyz,123");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_message() {
        let result = ClientMessage::parse("");
        assert!(result.is_err());
    }

    #[test]
    fn test_server_message_serialize() {
        let msg = ServerMessage::Pipeline { status: "ready".into() };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"pipeline\""));
        assert!(json.contains("\"status\":\"ready\""));
    }

    #[test]
    fn test_server_message_cursor_serialize() {
        let msg = ServerMessage::Cursor {
            curdata: "base64data".into(),
            handle: 42,
            r#override: None,
            hotspot: CursorHotspot { x: 5, y: 5 },
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"cursor\""));
        assert!(json.contains("\"handle\":42"));
    }
}
