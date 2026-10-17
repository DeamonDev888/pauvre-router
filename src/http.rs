//! HTTP/1.1 minimal — parse de requête + écriture de réponse.
//! Suffisant pour un client OpenAI local : Content-Length, pas de chunked
//! entrant, réponses en `Connection: close` (légal en 1.1, toléré par
//! tous les clients HTTP).

use std::io::{Read, Write};

const MAX_HEADER: usize = 64 * 1024;

#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>, // noms en minuscules
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lname = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lname)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub enum HttpError {
    Closed,
    BadRequest(String),
    TooLarge,
    Io(std::io::Error),
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// Lit une requête complète (head + body Content-Length).
/// Gère `Expect: 100-continue` (curl l'envoie au-delà de 1 Ko).
pub fn read_request(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    max_body: usize,
) -> Result<Request, HttpError> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    // head jusqu'à \r\n\r\n
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Err(HttpError::Closed),
            Ok(_) => {
                buf.push(byte[0]);
                if buf.len() > MAX_HEADER {
                    return Err(HttpError::TooLarge);
                }
                if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n\r\n") {
                    break;
                }
            }
            Err(e) => return Err(HttpError::Io(e)),
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.split("\r\n").flat_map(|l| l.split('\n'));
    let request_line = lines.next().unwrap_or("").trim().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts.next().unwrap_or("").to_string();
    if method.is_empty() || path.is_empty() {
        return Err(HttpError::BadRequest("request line invalide".into()));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut expect_continue = false;
    let mut content_length: usize = 0;
    for line in lines {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_length = v
                    .parse()
                    .map_err(|_| HttpError::BadRequest("content-length invalide".into()))?;
            }
            if k == "expect" && v.to_ascii_lowercase().contains("100-continue") {
                expect_continue = true;
            }
            headers.push((k, v));
        }
    }
    if content_length > max_body {
        return Err(HttpError::TooLarge);
    }
    if expect_continue {
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        writer.flush()?;
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// Écrit une réponse HTTP complète et ferme la connexion.
pub fn write_response<W: Write>(
    w: &mut W,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, x-api-key\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reader de test à partir d'un buffer.
    fn parse(raw: &[u8], max_body: usize) -> Result<Request, HttpError> {
        let mut sink = Vec::new();
        let mut reader = std::io::Cursor::new(raw.to_vec());
        read_request(&mut reader, &mut sink, max_body)
    }

    #[test]
    fn parse_simple_get() {
        let raw = b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n";
        let r = parse(raw, 1024).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/health");
        assert_eq!(r.header("host"), Some("x"));
        assert!(r.body.is_empty());
    }

    #[test]
    fn parse_post_with_body() {
        let raw = b"POST /v1/chat/completions HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n{\"a\":1234}";
        let r = parse(raw, 1024).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, b"{\"a\":1234}");
    }

    #[test]
    fn body_trop_gros() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 999999\r\n\r\n";
        assert!(matches!(parse(raw, 1024), Err(HttpError::TooLarge)));
    }

    #[test]
    fn request_line_invalide() {
        assert!(matches!(
            parse(b"\r\n\r\n", 1024),
            Err(HttpError::BadRequest(_))
        ));
    }

    #[test]
    fn response_format() {
        let mut out = Vec::new();
        write_response(
            &mut out,
            429,
            "Too Many Requests",
            "application/json",
            b"{}",
            &[("Retry-After", "7")],
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 429 Too Many Requests\r\n"));
        assert!(s.contains("Retry-After: 7"));
        assert!(s.contains("Content-Length: 2"));
        assert!(s.contains("Access-Control-Allow-Origin: *"));
    }
}
