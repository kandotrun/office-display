use serde::Serialize;
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
pub struct Status {
    pub native_mode: bool,
    pub renderer: &'static str,
    pub renderer_initialized: bool,
    pub render_callback_frames: u64,
    pub frame_count: u64,
    pub foreground_contain: bool,
    pub camera_foreground_overlap: bool,
    pub image_rendering_frames: u64,
    pub photo_draw_calls: u64,
    pub camera_rendering_frames: u64,
    pub evidence_kind: &'static str,
    pub content_generation: u64,
    pub playlist_generation: u64,
    pub photos_available: usize,
    pub photos_loaded_total: u64,
    pub photos_in_scene: usize,
    pub skipped_videos: usize,
    pub rejected_entries: usize,
    pub photo_decode_failures: u64,
    pub content_state: &'static str,
    pub source_state: &'static str,
    pub layout: &'static str,
    pub fade_active: bool,
    pub camera_enabled: bool,
    pub camera_state: &'static str,
    pub camera_age_seconds: Option<f64>,
    pub camera_timestamp_verified: bool,
    pub camera_updates: u64,
    pub camera_transport: &'static str,
    pub camera_target_fps: u32,
    pub camera_dimensions: [usize; 2],
    pub photo_texture_count: usize,
    pub photo_texture_bytes: usize,
    pub max_photo_textures: usize,
    pub max_photo_texture_bytes: usize,
    pub camera_texture_count: usize,
    pub photo_port: u16,
    pub camera_port: Option<u16>,
    pub status_port: u16,
    pub fullscreen_requested: bool,
    pub pixels_per_point: f32,
    pub viewport_points: [f32; 2],
    pub video_enabled: bool,
    pub weather_enabled: bool,
    pub hand_enabled: bool,
    pub gesture_overlay_enabled: bool,
    pub camera_mirrored: bool,
    pub hand_api_live: bool,
    pub hand_count: usize,
    pub hand_candidate_state: String,
    pub hand_guidance_state: String,
    pub hand_ready_for_motion: bool,
    pub gesture_effect_direction: Option<&'static str>,
    pub gesture_effect_flashes: u64,
    pub hand_reports_received: u64,
    pub hand_diagnostics: crate::hand_diagnostics::Summary,
    pub hand_event_counter: u64,
    pub hand_tracking_state: String,
    pub hand_search_active: bool,
    pub hand_full_frame_detections: usize,
    pub hand_roi_detections: usize,
    pub hand_last_confirmed_age_ms: Option<u64>,
    pub japanese_font_loaded: bool,
    pub gaze_enabled: bool,
    /// HTTP remains read-only even when native gesture navigation is opted in.
    pub write_controls_enabled: bool,
    pub gesture_navigation_enabled: bool,
    pub navigation_pending: bool,
    pub navigation_requests: u64,
    pub navigation_dispatched: u64,
    pub navigation_commits: u64,
    pub navigation_ignored: u64,
    pub navigation_failures: u64,
    pub navigation_last_action: Option<&'static str>,
    pub navigation_last_result: &'static str,
    pub history_length: usize,
    pub history_position: usize,
    pub history_capacity: usize,
    pub scene_id: u64,
    pub swipe_left_action: &'static str,
    pub swipe_right_action: &'static str,
    pub last_error: Option<&'static str>,
    pub uptime_seconds: f64,
    #[serde(skip)]
    pub last_frame: Option<Instant>,
}
impl Status {
    pub fn new(
        photo_port: u16,
        camera: bool,
        status_port: u16,
        fullscreen: bool,
        scale: f32,
    ) -> Self {
        Self {
            native_mode: true,
            renderer: "eframe-egui-glow-x11",
            renderer_initialized: false,
            render_callback_frames: 0,
            frame_count: 0,
            foreground_contain: true,
            camera_foreground_overlap: false,
            image_rendering_frames: 0,
            photo_draw_calls: 0,
            camera_rendering_frames: 0,
            evidence_kind: "egui_image_draw_submissions_not_gpu_screenshot",
            content_generation: 0,
            playlist_generation: 0,
            photos_available: 0,
            photos_loaded_total: 0,
            photos_in_scene: 0,
            skipped_videos: 0,
            rejected_entries: 0,
            photo_decode_failures: 0,
            content_state: "starting",
            source_state: "starting",
            layout: "none",
            fade_active: false,
            camera_enabled: camera,
            camera_state: if camera { "starting" } else { "disabled" },
            camera_age_seconds: None,
            camera_timestamp_verified: false,
            camera_updates: 0,
            camera_transport: if camera {
                "shared_loopback_mjpeg"
            } else {
                "disabled"
            },
            camera_target_fps: if camera { 15 } else { 0 },
            camera_dimensions: [0, 0],
            photo_texture_count: 0,
            photo_texture_bytes: 0,
            max_photo_textures: 16,
            max_photo_texture_bytes: 8 * 1600 * 1200 * 4 + 8 * 160 * 160 * 4,
            camera_texture_count: 0,
            photo_port,
            camera_port: camera.then_some(8082),
            status_port,
            fullscreen_requested: fullscreen,
            pixels_per_point: scale,
            viewport_points: [0., 0.],
            video_enabled: false,
            weather_enabled: false,
            hand_enabled: false,
            gesture_overlay_enabled: false,
            camera_mirrored: false,
            hand_api_live: false,
            hand_count: 0,
            hand_candidate_state: "disabled".into(),
            hand_guidance_state: "disabled".into(),
            hand_ready_for_motion: false,
            gesture_effect_direction: None,
            gesture_effect_flashes: 0,
            hand_reports_received: 0,
            hand_diagnostics: crate::hand_diagnostics::Summary::default(),
            hand_event_counter: 0,
            hand_tracking_state: "untracked".into(),
            hand_search_active: false,
            hand_full_frame_detections: 0,
            hand_roi_detections: 0,
            hand_last_confirmed_age_ms: None,
            japanese_font_loaded: false,
            gaze_enabled: false,
            write_controls_enabled: false,
            gesture_navigation_enabled: false,
            navigation_pending: false,
            navigation_requests: 0,
            navigation_dispatched: 0,
            navigation_commits: 0,
            navigation_ignored: 0,
            navigation_failures: 0,
            navigation_last_action: None,
            navigation_last_result: "idle",
            history_length: 0,
            history_position: 0,
            history_capacity: 32,
            scene_id: 0,
            swipe_left_action: "next",
            swipe_right_action: "previous",
            last_error: None,
            uptime_seconds: 0.,
            last_frame: None,
        }
    }
    pub fn healthy(&self) -> bool {
        self.renderer_initialized
            && self
                .last_frame
                .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
    }
}
pub type SharedStatus = Arc<Mutex<Status>>;
pub fn start(
    port: u16,
    state: SharedStatus,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    thread::Builder::new()
        .name("readonly-status".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &state),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(_) => break,
                }
            }
        })
}
fn serve(mut stream: TcpStream, state: &SharedStatus) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
    let start = Instant::now();
    let mut request = Vec::new();
    let mut buf = [0_u8; 1024];
    while request.len() < 4096 && start.elapsed() < Duration::from_millis(250) {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    if request.len() > 4096 || !request.windows(4).any(|w| w == b"\r\n\r\n") {
        return;
    }
    let (code, body) = if let Ok(status) = state.lock() {
        reply(&request, &status)
    } else {
        (503, b"{\"error\":\"unavailable\"}".to_vec())
    };
    let reason = match code {
        200 => "OK",
        405 => "Method Not Allowed",
        404 => "Not Found",
        _ => "Service Unavailable",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}
pub fn reply(request: &[u8], status: &Status) -> (u16, Vec<u8>) {
    let first = std::str::from_utf8(request)
        .unwrap_or("")
        .split("\r\n")
        .next()
        .unwrap_or("");
    let parts: Vec<_> = first.split_whitespace().collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
        return (404, b"{\"error\":\"not_found\"}".to_vec());
    }
    if !matches!(parts[1], "/health" | "/status") {
        return (404, b"{\"error\":\"not_found\"}".to_vec());
    }
    if parts[0] != "GET" {
        return (405, b"{\"error\":\"read_only\"}".to_vec());
    }
    if parts[1] == "/health" {
        let healthy = status.healthy();
        return (
            if healthy { 200 } else { 503 },
            format!(
                "{{\"healthy\":{healthy},\"native_mode\":true,\"renderer_initialized\":{}}}",
                status.renderer_initialized
            )
            .into_bytes(),
        );
    }
    (
        200,
        serde_json::to_vec(status).unwrap_or_else(|_| b"{}".to_vec()),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_endpoints_have_no_content_or_controls() {
        let s = Status::new(8081, false, 8096, false, 1.);
        assert_eq!(reply(b"GET /health HTTP/1.1\r\n\r\n", &s).0, 503);
        for path in ["/photos/x.jpg", "/snapshot.jpg", "/status?x=1", "/control"] {
            let r = format!("GET {path} HTTP/1.1\r\n\r\n");
            assert_eq!(reply(r.as_bytes(), &s).0, 404);
        }
        assert_eq!(reply(b"POST /status HTTP/1.1\r\n\r\n", &s).0, 405);
        let body = reply(b"GET /status HTTP/1.1\r\n\r\n", &s).1;
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["write_controls_enabled"], false);
        assert_eq!(json["hand_enabled"], false);
        assert!(json.get("photos").is_none());
        assert!(json.get("names").is_none());
        assert!(json.get("last_frame").is_none());
    }
    #[test]
    fn actual_tcp_service_is_readonly_json_no_store_no_cors() {
        let shared = Arc::new(Mutex::new(Status::new(8081, false, 8096, false, 1.)));
        for (request, expected) in [
            ("GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n", 200),
            ("POST /status HTTP/1.1\r\nContent-Length: 0\r\n\r\n", 405),
            ("GET /snapshot.jpg HTTP/1.1\r\n\r\n", 404),
        ] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            assert!(address.ip().is_loopback());
            let state = shared.clone();
            let worker = thread::spawn(move || {
                let (s, _) = listener.accept().unwrap();
                serve(s, &state);
            });
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            worker.join().unwrap();
            let response = String::from_utf8(bytes).unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {expected} ")));
            assert!(response.contains("Cache-Control: no-store"));
            assert!(!response.contains("Access-Control-Allow-Origin"));
            let body = response.split_once("\r\n\r\n").unwrap().1;
            assert!(serde_json::from_str::<serde_json::Value>(body).is_ok());
        }
    }
    #[test]
    fn health_reports_renderer_not_content_claim() {
        let mut s = Status::new(8081, false, 8096, false, 1.);
        s.renderer_initialized = true;
        s.last_frame = Some(Instant::now());
        assert_eq!(reply(b"GET /health HTTP/1.1\r\n\r\n", &s).0, 200);
        s.last_frame = Some(Instant::now() - Duration::from_secs(5));
        assert!(!s.healthy());
    }
    #[test]
    fn diagnosis_is_readonly_bounded_and_survives_current_no_hand() {
        let mut status = Status::new(8081, true, 8096, true, 2.);
        let mut d = crate::hand_diagnostics::Diagnostics::default();
        d.epoch();
        for state in [
            "armed_geometric_only",
            "horizontal_motion_candidate",
            "ambiguous_hand_association",
            "no_hand",
        ] {
            d.observe(state, "locked");
        }
        status.hand_diagnostics = d.summary;
        let (code, body) = reply(b"GET /status HTTP/1.1\r\n\r\n", &status);
        assert_eq!(code, 200);
        assert!(body.len() < 8192);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["hand_diagnostics"]["last_interruption"]["observed_reason"],
            "ambiguous_hand_association"
        );
        assert_eq!(json["hand_diagnostics"]["state_counts"]["no_hand"], 1);
        assert_eq!(json["write_controls_enabled"], false);
        assert_eq!(json["gesture_navigation_enabled"], false);
        assert_eq!(reply(b"POST /status HTTP/1.1\r\n\r\n", &status).0, 405);
        assert_eq!(reply(b"GET /control HTTP/1.1\r\n\r\n", &status).0, 404);
    }
}
