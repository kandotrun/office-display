//! Fixed loopback address, finite bytes and absolute deadline. No URL redirects/proxy/env.
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
pub const HTTP_DEADLINE: Duration = Duration::from_secs(4);
#[derive(Debug)]
pub struct Response {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Response {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}
pub fn get(port: u16, path: &str, max_body: usize) -> Result<Response, &'static str> {
    get_with_deadline(port, path, max_body, HTTP_DEADLINE)
}
pub fn hand_status() -> Result<Response, &'static str> {
    get_with_deadline(8097, "/status", 32768, Duration::from_millis(250))
}
fn get_with_deadline(
    port: u16,
    path: &str,
    max_body: usize,
    total: Duration,
) -> Result<Response, &'static str> {
    if !(path == "/api/photos"
        || path == "/snapshot.jpg"
        || path.starts_with("/photos/")
        || (port == 8097 && path == "/status"))
        || !path.is_ascii()
        || path.contains(['\r', '\n', '?', '#', '\\'])
        || path.contains("..")
    {
        return Err("invalid_request_path");
    }
    let started = Instant::now();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, total.min(Duration::from_millis(500)))
        .map_err(|_| "connect_failed")?;
    stream
        .set_write_timeout(Some(total.min(Duration::from_millis(500))))
        .map_err(|_| "timeout_setup")?;
    write!(stream,"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nCache-Control: no-cache\r\n\r\n").map_err(|_|"request_failed")?;
    let mut data = Vec::new();
    let mut block = [0_u8; 8192];
    let mut header_end = None;
    let mut expected = None;
    loop {
        let remaining = total
            .checked_sub(started.elapsed())
            .ok_or("http_deadline")?;
        stream
            .set_read_timeout(Some(remaining.min(Duration::from_millis(500))))
            .map_err(|_| "timeout_setup")?;
        match stream.read(&mut block) {
            Ok(0) => break,
            Ok(n) => {
                if data.len() + n > max_body + MAX_HEADER_BYTES + 4 {
                    return Err("response_too_large");
                }
                data.extend_from_slice(&block[..n]);
                if header_end.is_none() {
                    if let Some(offset) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        if offset > MAX_HEADER_BYTES {
                            return Err("headers_too_large");
                        }
                        let headers = parse_headers(&data[..offset])?;
                        expected = content_length(&headers, max_body)?;
                        header_end = Some(offset + 4);
                    } else if data.len() > MAX_HEADER_BYTES {
                        return Err("headers_too_large");
                    }
                }
                if let Some(end) = header_end {
                    if data.len() - end > max_body {
                        return Err("body_too_large");
                    }
                    if let Some(len) = expected {
                        if data.len() - end > len {
                            return Err("length_mismatch");
                        }
                        if data.len() - end == len {
                            break;
                        }
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return Err("response_read_failed"),
        }
    }
    parse_response(&data, max_body)
}
fn parse_headers(bytes: &[u8]) -> Result<Vec<(String, String)>, &'static str> {
    if bytes.len() > MAX_HEADER_BYTES {
        return Err("headers_too_large");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid_headers")?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or("missing_status")?;
    let mut parts = status.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) || parts.next() != Some("200") {
        return Err("http_not_200");
    }
    let mut headers = Vec::new();
    for line in lines {
        let (k, v) = line.split_once(':').ok_or("invalid_header")?;
        if headers
            .iter()
            .any(|(old, _): &(String, String)| old.eq_ignore_ascii_case(k))
        {
            return Err("duplicate_header");
        }
        headers.push((k.to_string(), v.trim().to_string()));
    }
    if headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding")
            || (k.eq_ignore_ascii_case("content-encoding") && v != "identity")
    }) {
        return Err("unsupported_encoding");
    }
    Ok(headers)
}
fn content_length(
    headers: &[(String, String)],
    max_body: usize,
) -> Result<Option<usize>, &'static str> {
    let n = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.parse::<usize>().map_err(|_| "invalid_content_length"))
        .transpose()?;
    if n.is_some_and(|n| n > max_body) {
        return Err("body_too_large");
    }
    Ok(n)
}
pub fn parse_response(bytes: &[u8], max_body: usize) -> Result<Response, &'static str> {
    let offset = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("missing_headers")?;
    let headers = parse_headers(&bytes[..offset])?;
    let body = &bytes[offset + 4..];
    if body.len() > max_body {
        return Err("body_too_large");
    }
    if content_length(&headers, max_body)?.is_some_and(|n| n != body.len()) {
        return Err("length_mismatch");
    }
    Ok(Response {
        headers,
        body: body.to_vec(),
    })
}
pub fn require_type(response: &Response, allowed: &[&str]) -> Result<(), &'static str> {
    let mime = response
        .header("content-type")
        .and_then(|s| s.split(';').next())
        .ok_or("missing_content_type")?;
    if allowed.iter().any(|a| mime.eq_ignore_ascii_case(a)) {
        Ok(())
    } else {
        Err("invalid_content_type")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_redirect_chunked_truncation_and_limits() {
        for bytes in [
            &b"HTTP/1.1 302 Found\r\nLocation: http://example.test/\r\n\r\nx"[..],
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx"[..],
        ] {
            assert!(parse_response(bytes, 10).is_err());
        }
        assert!(
            parse_response(
                b"HTTP/1.0 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 3\r\n\r\nabc",
                2
            )
            .is_err()
        );
        assert!(parse_response(b"HTTP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nabc", 3).is_ok());
    }
    #[test]
    fn slow_response_has_absolute_deadline() {
        use std::net::TcpListener;
        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = server.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n");
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(30));
                if s.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        let start = Instant::now();
        assert!(get_with_deadline(port, "/api/photos", 100, Duration::from_millis(120)).is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
        handle.join().unwrap();
    }
    #[test]
    fn rejects_arbitrary_paths() {
        for path in [
            "http://evil/",
            "/api/photos?q=1",
            "/photos/../x.jpg",
            "/photos/x.jpg\r\n",
        ] {
            assert_eq!(get(1, path, 100).unwrap_err(), "invalid_request_path");
        }
    }
}
