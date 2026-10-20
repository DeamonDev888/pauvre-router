//! Couche HTTP/1.1 — serveur à workers fixes, timeouts systématiques,
//! keep-alive et chunked encoding. Aucun thread par connexion ne peut
//! fuiter : chaque socket vit sous deadline, les workers récoltent les
//! morts et repassent au pool. `Connection: close` est honoré, le
//! client peut réutiliser la connexion sinon.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Timeouts par défaut (côté serveur).
pub const READ_TIMEOUT: Duration = Duration::from_secs(120);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Garde-fou absolu : une connexion ne peut pas dépasser cette durée
/// totale, même en keep-alive avec requêtes en rafale.
pub const MAX_CONN_AGE: Duration = Duration::from_secs(600);
const MAX_HEADER: usize = 32 * 1024;
const MAX_IDLE: Duration = Duration::from_secs(65);

#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>, // noms en minuscules
    pub body: Vec<u8>,
    /// false => le client demande la fermeture après la réponse.
    pub keep_alive: bool,
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
    /// Pair parti / timeout / socket morte en lisant — silence attendu.
    Closed,
    BadRequest(String),
    TooLarge,
    /// La deadline totale de connexion est atteinte (fermeture propre).
    ConnExpired,
    Io(std::io::Error),
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted => HttpError::Closed,
            _ => HttpError::Io(e),
        }
    }
}

/// Vue bufferisée sur la socket : lit au moins à la demande, garde les
/// surplus pour la requête suivante (keep-alive).
pub struct HttpConn {
    reader: BufReader<TcpStream>,
}

impl HttpConn {
    pub fn new(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self {
            reader: BufReader::with_capacity(16 * 1024, stream),
        })
    }

    /// Socket clonable pour l'écriture (même fd, timeouts déjà posés).
    pub fn try_write_half(&self) -> std::io::Result<TcpStream> {
        self.reader.get_ref().try_clone()
    }

    /// Lit UNE requête. `idle_wait` : temps max au repos avant la
    /// prochaine requête en keep-alive. Retourne Closed si le client est
    /// parti (normal, silencieux).
    pub fn read_request(
        &mut self,
        max_body: usize,
        idle_wait: Duration,
    ) -> Result<Request, HttpError> {
        // --- head ---
        let mut head: Vec<u8> = Vec::with_capacity(1024);
        let mut byte = [0u8; 1];
        // 1er octet : au repos on tolère MAX_IDLE, ensuite READ_TIMEOUT
        self.reader.get_ref().set_read_timeout(Some(
            if head.is_empty() && idle_wait > READ_TIMEOUT {
                MAX_IDLE
            } else {
                READ_TIMEOUT
            },
        ))?;
        match self.reader.read(&mut byte) {
            Ok(0) => return Err(HttpError::Closed),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(HttpError::from(e)),
        }
        self.reader.get_ref().set_read_timeout(Some(READ_TIMEOUT))?;
        loop {
            match self.reader.read(&mut byte) {
                Ok(0) => return Err(HttpError::Closed),
                Ok(_) => {
                    head.push(byte[0]);
                    if head.len() > MAX_HEADER {
                        return Err(HttpError::TooLarge);
                    }
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    // tolère aussi les clients \n-only (curl GNU, tests)
                    if head.ends_with(b"\n\n") {
                        break;
                    }
                }
                Err(e) => return Err(HttpError::from(e)),
            }
        }
        let head_str = String::from_utf8_lossy(&head).to_string();
        let mut lines = head_str.split("\r\n").flat_map(|l| l.split('\n'));
        let request_line = lines.next().unwrap_or("").trim().to_string();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_uppercase();
        let path = parts.next().unwrap_or("").to_string();
        let version = parts.next().unwrap_or("HTTP/1.1").to_string();
        if method.is_empty() || path.is_empty() {
            return Err(HttpError::BadRequest("request line invalide".into()));
        }
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut content_length: usize = 0;
        let mut chunked = false;
        let mut expect_continue = false;
        let mut conn_close = version.eq_ignore_ascii_case("HTTP/1.0");
        for line in lines {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            if let Some((k, v)) = l.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                let v = v.trim().to_string();
                match k.as_str() {
                    "content-length" => {
                        content_length = v
                            .parse()
                            .map_err(|_| HttpError::BadRequest("content-length invalide".into()))?;
                    }
                    "transfer-encoding" => {
                        if v.to_ascii_lowercase().contains("chunked") {
                            chunked = true;
                        }
                    }
                    "connection" => {
                        let vl = v.to_ascii_lowercase();
                        if vl.contains("close") {
                            conn_close = true;
                        } else if vl.contains("keep-alive") {
                            conn_close = false;
                        }
                    }
                    "expect" => {
                        expect_continue = v.to_ascii_lowercase().contains("100-continue");
                    }
                    _ => {}
                }
                headers.push((k, v));
            }
        }
        // Body trop grand : ne PAS répondre tout de suite. Si on renvoie le
        // 413 sans lire le body, les octets restants restent en route vers un
        // socket qu'on ferme aussitôt → RST → le client voit « Connection
        // error » au lieu du 413 (vu en prod : sessions Hermes de ~850
        // messages frappant la limite de façon intermittente). On lit et
        // jette le body (borne anti-DoS : 64 Mo), PUIS on répond.
        let oversized = content_length > max_body;
        if oversized && !chunked {
            // drain avec DEADLINE TOTALE (pas par read) : sur une machine
            // chargée, le body peut arriver en retard — un timeout par read
            // abandonnait le drain avec des Mo non lus → RST au close
            // (vu sur la CI Linux/macOS). On poll toutes les 250 ms,
            // 10 s max au total, puis on répond 413 quoi qu'il arrive.
            let deadline = Instant::now() + Duration::from_secs(10);
            let _ = self
                .reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(250)));
            let mut remaining = content_length.min(64 * 1024 * 1024);
            let mut sink = [0u8; 16384];
            while remaining > 0 && Instant::now() < deadline {
                match self.reader.read(&mut sink) {
                    Ok(0) => break,
                    Ok(n) => remaining -= n,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue; // pas de données ENCORE : on re-poll
                    }
                    Err(_) => break,
                }
            }
            let _ = self.reader.get_ref().set_read_timeout(Some(READ_TIMEOUT));
            return Err(HttpError::TooLarge);
        }

        if expect_continue {
            let mut w = self.try_write_half()?;
            w.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
            w.flush()?;
        }

        // --- body ---
        let body = if chunked {
            self.read_chunked(max_body)?
        } else {
            let mut body = vec![0u8; content_length];
            self.reader.read_exact(&mut body)?;
            body
        };

        Ok(Request {
            method,
            path,
            headers,
            body,
            keep_alive: !conn_close,
        })
    }

    /// Lit un corps chunked (HTTP/1.1). Rejette au-delà de `max_body`.
    fn read_chunked(&mut self, max_body: usize) -> Result<Vec<u8>, HttpError> {
        let mut body = Vec::new();
        loop {
            // ligne de taille (hex)
            let mut size_line = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match self.reader.read(&mut byte) {
                    Ok(0) => return Err(HttpError::Closed),
                    Ok(_) => {
                        size_line.push(byte[0]);
                        if size_line.ends_with(b"\r\n") || size_line.ends_with(b"\n") {
                            break;
                        }
                        if size_line.len() > 16 {
                            return Err(HttpError::BadRequest("taille chunk illisible".into()));
                        }
                    }
                    Err(e) => return Err(HttpError::from(e)),
                }
            }
            let hex = String::from_utf8_lossy(&size_line)
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let size = usize::from_str_radix(&hex, 16)
                .map_err(|_| HttpError::BadRequest("taille chunk invalide".into()))?;
            if size == 0 {
                // dernier chunk : consommer les trailers jusqu'à la ligne vide
                let mut tail = Vec::new();
                loop {
                    match self.reader.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => {
                            tail.push(byte[0]);
                            if tail.ends_with(b"\r\n")
                                && (tail == b"\r\n" || tail.ends_with(b"\n\r\n"))
                            {
                                break;
                            }
                            if tail.len() > 8 * 1024 {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                return Ok(body);
            }
            if body.len() + size > max_body {
                return Err(HttpError::TooLarge);
            }
            let start = body.len();
            body.resize(start + size, 0);
            self.reader.read_exact(&mut body[start..])?;
            // CRLF après le chunk
            let mut crlf = [0u8; 2];
            let _ = self.reader.read_exact(&mut crlf);
        }
    }

    /// Écrit une réponse complète. `stream` = true → chunked (le client
    /// voit les octets arriver) ; false → Content-Length classique.
    pub fn write_response(
        &mut self,
        status: u16,
        reason: &str,
        content_type: &str,
        body: &[u8],
        extra: &[(&str, &str)],
        keep_alive: bool,
    ) -> std::io::Result<()> {
        let mut w = self.try_write_half()?;
        let mut head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (k, v) in extra {
            head.push_str(k);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        head.push_str(if keep_alive {
            "Connection: keep-alive\r\n\r\n"
        } else {
            "Connection: close\r\n\r\n"
        });
        w.write_all(head.as_bytes())?;
        w.write_all(body)?;
        w.flush()
    }

    /// Réponse en streaming chunked : callback appelé avec un writer,
    /// chaque write part sur le réseau (flush après chaque morceau).
    pub fn write_stream<F>(
        &mut self,
        status: u16,
        reason: &str,
        content_type: &str,
        mut emit: F,
    ) -> std::io::Result<()>
    where
        F: FnMut(&mut dyn Write) -> std::io::Result<()>,
    {
        let mut w = self.try_write_half()?;
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        w.write_all(head.as_bytes())?;
        w.flush()?;
        let result = stream_chunks(&mut w, &mut emit);
        // terminal chunk (si le client est encore là)
        if result.is_ok() {
            w.write_all(b"0\r\n\r\n")?;
            w.flush()?;
        }
        result
    }
}

/// Émet les chunks du callback vers la socket (fonction libre : évite
/// le double borrow &mut TcpStream du sink + terminal).
fn stream_chunks<F>(w: &mut TcpStream, emit: &mut F) -> std::io::Result<()>
where
    F: FnMut(&mut dyn Write) -> std::io::Result<()>,
{
    let mut sink = ChunkSink { w, err: None };
    let r = emit(&mut sink);
    if let Some(e) = sink.err.take() {
        return Err(e);
    }
    r
}

/// Adaptateur `Write` qui découpe en chunks HTTP vers la socket.
struct ChunkSink<'a> {
    w: &'a mut TcpStream,
    err: Option<std::io::Error>,
}

impl Write for ChunkSink<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // chunk framing : taille hex + CRLF + data + CRLF, flush immédiat
        let res = (|| -> std::io::Result<()> {
            if buf.is_empty() {
                return Ok(());
            }
            write!(self.w, "{:x}\r\n", buf.len())?;
            self.w.write_all(buf)?;
            self.w.write_all(b"\r\n")?;
            self.w.flush()
        })();
        if let Err(e) = res {
            self.err = Some(e);
            return Err(std::io::Error::other("sink"));
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse direct depuis un buffer ( lecture d'une requête unique ).
    fn parse(raw: &[u8], max_body: usize) -> Result<Request, HttpError> {
        let stream = test_pair(raw);
        let mut conn = HttpConn::new(stream).unwrap();
        conn.read_request(max_body, READ_TIMEOUT)
    }

    /// TcpStream en mémoire via paire de sockets locale (loopback).
    fn test_pair(raw: &[u8]) -> TcpStream {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut c = client;
        c.write_all(raw).unwrap();
        // garde `client` vivant le temps du test via mem::forget volontaire
        std::mem::forget(c);
        server
    }

    #[test]
    fn parse_simple_get() {
        let r = parse(b"GET /health HTTP/1.1\r\nHost: x\r\n\r\n", 1024).unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/health");
        assert_eq!(r.header("host"), Some("x"));
        assert!(r.body.is_empty());
        assert!(r.keep_alive);
    }

    #[test]
    fn parse_post_content_length() {
        let raw = b"POST /v1/chat/completions HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 10\r\n\r\n{\"a\":1234}";
        let r = parse(raw, 1024).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, b"{\"a\":1234}");
    }

    #[test]
    fn parse_chunked() {
        let raw = b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let r = parse(raw, 1024).unwrap();
        assert_eq!(r.body, b"Wikipedia");
    }

    #[test]
    fn connection_close_honore() {
        let r = parse(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n", 1024).unwrap();
        assert!(!r.keep_alive);
        let r2 = parse(b"GET / HTTP/1.0\r\n\r\n", 1024).unwrap();
        assert!(!r2.keep_alive);
    }

    #[test]
    fn body_trop_gros() {
        assert!(matches!(
            parse(b"POST /x HTTP/1.1\r\nContent-Length: 999999\r\n\r\n", 1024),
            Err(HttpError::TooLarge)
        ));
    }

    #[test]
    fn chunked_trop_gros() {
        let big = format!("{:x}\r\n", 50_000);
        let raw = format!("POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n{big}");
        let _ = raw;
        assert!(matches!(
            parse(
                b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFF\r\nxx\r\n",
                1024
            ),
            Err(HttpError::TooLarge) | Err(HttpError::BadRequest(_)) | Err(HttpError::Closed)
        ));
    }

    #[test]
    fn request_line_invalide() {
        assert!(matches!(
            parse(b"\r\n\r\n", 1024),
            Err(HttpError::BadRequest(_) | HttpError::Closed)
        ));
    }

    /// Keep-alive réel : deux requêtes sur la même connexion.
    #[test]
    fn keep_alive_deux_requetes() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        let mut conn = HttpConn::new(server).unwrap();

        client.write_all(b"GET /a HTTP/1.1\r\n\r\n").unwrap();
        let r1 = conn.read_request(1024, READ_TIMEOUT).unwrap();
        assert_eq!(r1.path, "/a");
        conn.write_response(200, "OK", "text/plain", b"un", &[], true)
            .unwrap();

        client
            .write_all(b"GET /b HTTP/1.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let r2 = conn.read_request(1024, READ_TIMEOUT).unwrap();
        assert_eq!(r2.path, "/b");
        assert!(!r2.keep_alive);
    }
}
