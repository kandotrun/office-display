//! Bounded loopback MJPEG preview; current JPEG only, no queue/disk/device access.
use crate::{decode, http::Response};
use chrono::{DateTime, Utc};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub const MAX_PART_BYTES: usize = 512 * 1024;
const MAX_LINE: usize = 2048;
const MAX_HEADERS: usize = 8192;
const FRAME_DEADLINE: Duration = Duration::from_secs(1);
pub struct PreviewStream {
    reader: BufReader<TcpStream>,
}
pub struct PreviewFrame {
    pub photo: decode::DecodedPhoto,
    pub captured: Instant,
    pub sequence: u64,
}
fn line(
    reader: &mut impl BufRead,
    started: Instant,
    stop: &AtomicBool,
) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("preview_stopped");
        }
        if started.elapsed() >= FRAME_DEADLINE {
            return Err("preview_deadline");
        }
        let chunk = reader.fill_buf().map_err(|_| "preview_read_timeout")?;
        if chunk.is_empty() {
            return Err("preview_incomplete_line");
        }
        let newline = chunk.iter().position(|b| *b == b'\n');
        let n = newline.map_or(chunk.len(), |i| i + 1);
        if bytes.len() + n > MAX_LINE {
            return Err("preview_line_limit");
        }
        bytes.extend_from_slice(&chunk[..n]);
        reader.consume(n);
        if newline.is_some() {
            if !bytes.ends_with(b"\r\n") {
                return Err("preview_invalid_line");
            }
            bytes.truncate(bytes.len() - 2);
            return Ok(bytes);
        }
    }
}

fn headers(
    reader: &mut impl BufRead,
    started: Instant,
    stop: &AtomicBool,
) -> Result<Vec<(String, String)>, &'static str> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut total = 0;
    loop {
        let bytes = line(reader, started, stop)?;
        total += bytes.len() + 2;
        if total > MAX_HEADERS {
            return Err("preview_headers_limit");
        }
        if bytes.is_empty() {
            return Ok(out);
        }
        if !bytes.is_ascii() {
            return Err("preview_headers_ascii");
        }
        let s = std::str::from_utf8(&bytes).map_err(|_| "preview_headers_ascii")?;
        let (k, v) = s.split_once(':').ok_or("preview_header_invalid")?;
        if k.is_empty()
            || k.bytes().any(|b| !b.is_ascii_alphanumeric() && b != b'-')
            || out.iter().any(|(old, _)| old.eq_ignore_ascii_case(k))
        {
            return Err("preview_duplicate_or_invalid_header");
        }
        out.push((k.into(), v.trim().into()));
    }
}
fn value<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}
fn validate_http(h: &[(String, String)]) -> Result<(), &'static str> {
    if value(h, "transfer-encoding").is_some()
        || value(h, "content-encoding").is_some_and(|v| v != "identity")
        || value(h, "content-length").is_some()
    {
        return Err("preview_http_encoding");
    }
    let ty = value(h, "content-type").ok_or("preview_http_type")?;
    let mut items = ty.split(';').map(str::trim);
    if !items
        .next()
        .is_some_and(|v| v.eq_ignore_ascii_case("multipart/x-mixed-replace"))
    {
        return Err("preview_http_type");
    }
    if items.collect::<Vec<_>>() != ["boundary=frame"] {
        return Err("preview_boundary_type");
    }
    Ok(())
}
fn metadata(
    h: &[(String, String)],
    previous: Option<u64>,
) -> Result<(usize, Instant, u64), &'static str> {
    if !value(h, "content-type").is_some_and(|v| v.eq_ignore_ascii_case("image/jpeg"))
        || value(h, "transfer-encoding").is_some()
        || value(h, "content-encoding").is_some()
    {
        return Err("preview_part_type");
    }
    let n = value(h, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= MAX_PART_BYTES)
        .ok_or("preview_part_length")?;
    let sequence = value(h, "x-frame-sequence")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0 && previous.is_none_or(|old| *n > old))
        .ok_or("preview_sequence_frozen_or_reversed")?;
    let stamp = value(h, "x-frame-timestamp")
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .ok_or("preview_timestamp_missing")?;
    let delta = Utc::now().signed_duration_since(stamp).num_milliseconds();
    // Clock timestamps from the same host; permit <=100ms future rounding/skew, never guessed provenance.
    if !(-100..3000).contains(&delta) {
        return Err("preview_timestamp_stale_or_future");
    }
    let captured = Instant::now()
        .checked_sub(Duration::from_millis(delta.max(0) as u64))
        .ok_or("preview_timestamp_invalid")?;
    Ok((n, captured, sequence))
}
impl PreviewStream {
    pub fn open(port: u16, stop: &AtomicBool) -> Result<Self, &'static str> {
        let started = Instant::now();
        let mut stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_millis(400),
        )
        .map_err(|_| "preview_connect_failed")?;
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|_| "preview_timeout_setup")?;
        stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .map_err(|_| "preview_timeout_setup")?;
        write!(stream,"GET /stream.mjpg HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCache-Control: no-cache\r\n\r\n").map_err(|_|"preview_request_failed")?;
        let mut reader = BufReader::with_capacity(8192, stream);
        let status = line(&mut reader, started, stop)?;
        let text = std::str::from_utf8(&status).map_err(|_| "preview_http_status")?;
        let mut p = text.split_whitespace();
        if !matches!(p.next(), Some("HTTP/1.0" | "HTTP/1.1")) || p.next() != Some("200") {
            return Err("preview_http_status");
        }
        let h = headers(&mut reader, started, stop)?;
        validate_http(&h)?;
        Ok(Self { reader })
    }
    pub fn next(
        &mut self,
        previous: Option<u64>,
        stop: &AtomicBool,
    ) -> Result<PreviewFrame, &'static str> {
        let started = Instant::now();
        let mut boundary = line(&mut self.reader, started, stop)?;
        if boundary.is_empty() {
            boundary = line(&mut self.reader, started, stop)?;
        }
        if boundary != b"--frame" {
            return Err("preview_boundary_invalid");
        }
        let h = headers(&mut self.reader, started, stop)?;
        let (n, captured, sequence) = metadata(&h, previous)?;
        let mut jpeg = vec![0u8; n];
        let mut offset = 0;
        while offset < n {
            if stop.load(Ordering::Relaxed) {
                return Err("preview_stopped");
            }
            if started.elapsed() >= FRAME_DEADLINE {
                return Err("preview_deadline");
            }
            let read = self
                .reader
                .read(&mut jpeg[offset..])
                .map_err(|_| "preview_read_timeout")?;
            if read == 0 {
                return Err("preview_truncated_jpeg");
            }
            offset += read;
        }
        let mut end = [0u8; 2];
        self.reader
            .read_exact(&mut end)
            .map_err(|_| "preview_truncated_separator")?;
        if end != *b"\r\n" {
            return Err("preview_invalid_separator");
        }
        let response = Response {
            headers: h,
            body: jpeg,
        };
        let (w, h) = image::ImageReader::with_format(
            std::io::Cursor::new(&response.body),
            image::ImageFormat::Jpeg,
        )
        .into_dimensions()
        .map_err(|_| "preview_jpeg_dimensions")?;
        if w == 0 || h == 0 || w > 640 || h > 360 {
            return Err("preview_dimensions_limit");
        }
        let photo = decode::decode(&response.body, false, true)?;
        if captured.elapsed() >= Duration::from_secs(3) {
            return Err("preview_stale_after_decode");
        }
        Ok(PreviewFrame {
            photo,
            captured,
            sequence,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn h(sequence: &str, stamp: &str, len: &str) -> Vec<(String, String)> {
        vec![
            ("Content-Type".into(), "image/jpeg".into()),
            ("Content-Length".into(), len.into()),
            ("X-Frame-Sequence".into(), sequence.into()),
            ("X-Frame-Timestamp".into(), stamp.into()),
        ]
    }
    #[test]
    fn metadata_rejects_missing_stale_future_frozen_reversed_and_bytes() {
        let stamp = Utc::now().to_rfc3339();
        assert!(metadata(&h("7", &stamp, "4"), Some(6)).is_ok());
        for seq in ["0", "6", "5", "NaN"] {
            assert!(metadata(&h(seq, &stamp, "4"), Some(6)).is_err());
        }
        for t in [
            "invalid",
            &(Utc::now() - chrono::Duration::seconds(5)).to_rfc3339(),
            &(Utc::now() + chrono::Duration::seconds(5)).to_rfc3339(),
        ] {
            assert!(metadata(&h("7", t, "4"), None).is_err());
        }
        for n in ["0", "524289", "NaN"] {
            assert!(metadata(&h("7", &stamp, n), None).is_err());
        }
        let mut missing = h("7", &stamp, "4");
        missing.pop();
        assert!(metadata(&missing, None).is_err());
    }
    #[test]
    fn parser_limits_duplicates_and_encoding() {
        let stop = AtomicBool::new(false);
        for bytes in [
            &b"Content-Length: 1\r\ncontent-length: 2\r\n\r\n"[..],
            &vec![b'x'; MAX_LINE + 2][..],
            &b"X: z\n"[..],
        ] {
            assert!(headers(&mut std::io::Cursor::new(bytes), Instant::now(), &stop).is_err());
        }
        assert!(
            validate_http(&[(
                "Content-Type".into(),
                "multipart/x-mixed-replace; boundary=frame".into()
            )])
            .is_ok()
        );
        assert!(
            validate_http(&[(
                "Content-Type".into(),
                "multipart/x-mixed-replace; boundary=other".into()
            )])
            .is_err()
        );
        assert!(
            validate_http(&[
                (
                    "Content-Type".into(),
                    "multipart/x-mixed-replace; boundary=frame".into()
                ),
                ("Transfer-Encoding".into(), "chunked".into())
            ])
            .is_err()
        );
        let stop = AtomicBool::new(true);
        assert!(line(&mut std::io::Cursor::new(b"a\r\n"), Instant::now(), &stop).is_err());
    }
    #[test]
    fn native_multi_frame_and_frozen_stream_timeout() {
        use std::net::TcpListener;
        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = server.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            let mut req = [0; 1024];
            let _ = s.read(&mut req);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n").unwrap();
            let im = image::DynamicImage::ImageRgb8(image::RgbImage::new(640, 360));
            let mut jpeg = std::io::Cursor::new(Vec::new());
            im.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
            for seq in [10, 11] {
                let head = format!(
                    "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nX-Frame-Sequence: {seq}\r\nX-Frame-Timestamp: {}\r\n\r\n",
                    jpeg.get_ref().len(),
                    Utc::now().to_rfc3339()
                );
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(jpeg.get_ref()).unwrap();
                s.write_all(b"\r\n").unwrap();
            }
            std::thread::sleep(Duration::from_millis(450));
        });
        let stop = AtomicBool::new(false);
        let mut r = PreviewStream::open(port, &stop).unwrap();
        let a = r.next(None, &stop).unwrap();
        assert_eq!(a.photo.image.size, [640, 360]);
        let b = r.next(Some(a.sequence), &stop).unwrap();
        assert_eq!(b.sequence, 11);
        let t = Instant::now();
        assert!(r.next(Some(11), &stop).is_err());
        assert!(t.elapsed() < Duration::from_secs(1));
        handle.join().unwrap();
    }
}
