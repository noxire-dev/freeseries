//! The door an AI agent comes in through: an MCP endpoint (Streamable HTTP, plain JSON responses,
//! no streams) on 127.0.0.1. This file is only the HTTP edge. It decides who may talk, and hands
//! every JSON-RPC message to a `Handler`; what the tools are and what they do lives in the UI
//! (`src/agent.ts`), next to the state they change.
//!
//! Who may talk: only this machine (bound to loopback), only with the token the user copied out
//! of Settings, and never a web page. A page in the user's browser can reach 127.0.0.1, so a
//! foreign `Origin` or `Host` (DNS rebinding) is refused, as the MCP spec requires.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub enum Reply {
    /// The JSON-RPC response to a request.
    Json(String),
    /// A notification: taken, nothing to say.
    Accepted,
    /// Nobody answered in time.
    Unavailable,
}

pub trait Handler: Send + Sync {
    fn handle(&self, message: String) -> Reply;
}

const PATH: &str = "/mcp";
/// The versions `src/agent.ts` negotiates. A request that names another one is refused, as the spec asks.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

/// Listens until dropped.
pub struct Server {
    stop: Arc<AtomicBool>,
    listening: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

impl Server {
    /// `port` 0 picks a free one.
    pub fn start(port: u16, token: String, handler: Arc<dyn Handler>) -> io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?; // so the loop can notice `stop`
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let listening = std::thread::Builder::new().name("agent-http".into()).spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (token, handler) = (token.clone(), handler.clone());
                        std::thread::spawn(move || {
                            if let Err(e) = serve(stream, port, &token, handler.as_ref()) {
                                log::warn!("agent connection: {e}");
                            }
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                    Err(e) => {
                        log::warn!("agent endpoint stopped listening: {e}");
                        break;
                    }
                }
            }
        })?;
        Ok(Server { stop, listening: Some(listening), port })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{PATH}", self.port)
    }
}

impl Drop for Server {
    /// Returns once the port is free, so it can be taken again straight away.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(listening) = self.listening.take() {
            let _ = listening.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buffer.len() > MAX_HEADER {
            return Err(bad("headers too long"));
        }
        match stream.read(&mut chunk)? {
            0 => return Err(bad("closed before the headers ended")),
            n => buffer.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_lowercase(), v.trim().to_string())).collect();
    let mut request = Request { method, path, headers, body: String::new() };

    if request.header("transfer-encoding").is_some() {
        return Err(bad("chunked bodies are not supported"));
    }
    let length: usize = request.header("content-length").map_or(Ok(0), str::parse).map_err(|_| bad("bad content-length"))?;
    if length > MAX_BODY {
        return Err(bad("body too long"));
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk)? {
            0 => return Err(bad("closed before the body ended")),
            n => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);
    request.body = String::from_utf8(body).map_err(|_| bad("body is not UTF-8"))?;
    Ok(request)
}

/// Loopback under either of its names, on our port.
fn is_us(authority: &str, port: u16) -> bool {
    authority == format!("127.0.0.1:{port}") || authority == format!("localhost:{port}")
}

/// Equal without stopping at the first difference, so timing says nothing about the token.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn serve(mut stream: TcpStream, port: u16, token: &str, handler: &dyn Handler) -> io::Result<()> {
    stream.set_nonblocking(false)?; // Windows hands the listener's mode down to the connection
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (status, extra, body) = match read_request(&mut stream) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => ("400 Bad Request", "", e.to_string()),
        Err(e) => return Err(e),
        Ok(request) => respond(&request, port, token, handler),
    };
    let kind = if body.starts_with('{') || body.starts_with('[') { "application/json" } else { "text/plain; charset=utf-8" };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len())?;
    stream.flush()
}

fn respond(request: &Request, port: u16, token: &str, handler: &dyn Handler) -> (&'static str, &'static str, String) {
    let refuse = |status, why: &str| (status, "", why.to_string());
    if !request.header("host").is_some_and(|host| is_us(host, port)) {
        return refuse("403 Forbidden", "this endpoint only answers to 127.0.0.1");
    }
    if let Some(origin) = request.header("origin") {
        if !origin.strip_prefix("http://").is_some_and(|authority| is_us(authority, port)) {
            return refuse("403 Forbidden", "web pages may not talk to freeseries");
        }
    }
    if request.path != PATH {
        return refuse("404 Not Found", "the MCP endpoint is /mcp");
    }
    if request.method != "POST" {
        return ("405 Method Not Allowed", "Allow: POST\r\n", "freeseries answers POST only: it has no event stream".into());
    }
    if !request.header("authorization").and_then(|v| v.strip_prefix("Bearer ")).is_some_and(|given| same(given, token)) {
        return ("401 Unauthorized", "WWW-Authenticate: Bearer\r\n", "copy the connection command from freeseries Settings: it carries the token".into());
    }
    if request.header("mcp-protocol-version").is_some_and(|v| !PROTOCOL_VERSIONS.contains(&v)) {
        return refuse("400 Bad Request", "unsupported MCP-Protocol-Version");
    }
    match handler.handle(request.body.clone()) {
        Reply::Json(json) => ("200 OK", "", json),
        Reply::Accepted => ("202 Accepted", "", String::new()),
        Reply::Unavailable => refuse("503 Service Unavailable", "freeseries did not answer in time"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Handler for Echo {
        fn handle(&self, message: String) -> Reply {
            if message.contains("\"id\"") { Reply::Json(message) } else { Reply::Accepted }
        }
    }

    fn ask(port: u16, request: String) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    fn post(port: u16, headers: &str, body: &str) -> String {
        ask(port, format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()))
    }

    #[test]
    fn answers_the_token_holder_and_nobody_else() {
        let server = Server::start(0, "secret".into(), Arc::new(Echo)).unwrap();
        let port = server.port;
        let auth = "Authorization: Bearer secret\r\n";
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

        let ok = post(port, auth, request);
        assert!(ok.starts_with("HTTP/1.1 200") && ok.ends_with(request), "{ok}");
        assert!(post(port, auth, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).starts_with("HTTP/1.1 202"));

        assert!(post(port, "", request).starts_with("HTTP/1.1 401"), "no token");
        assert!(post(port, "Authorization: Bearer secres\r\n", request).starts_with("HTTP/1.1 401"), "wrong token");
        assert!(post(port, &format!("{auth}Origin: https://evil.example\r\n"), request).starts_with("HTTP/1.1 403"), "a web page");
        assert!(post(port, &format!("{auth}Origin: http://localhost:{port}\r\n"), request).starts_with("HTTP/1.1 200"), "our own origin");
        assert!(post(port, &format!("{auth}MCP-Protocol-Version: 1999-01-01\r\n"), request).starts_with("HTTP/1.1 400"), "unknown protocol version");
        let rebound = ask(port, format!("POST /mcp HTTP/1.1\r\nHost: evil.example:{port}\r\n{auth}Content-Length: 0\r\n\r\n"));
        assert!(rebound.starts_with("HTTP/1.1 403"), "DNS rebinding: {rebound}");
        assert!(ask(port, format!("GET /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n")).starts_with("HTTP/1.1 405"));
        assert!(ask(port, format!("POST /other HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n")).starts_with("HTTP/1.1 404"));

        drop(server);
        let again = Server::start(port, "secret".into(), Arc::new(Echo));
        assert!(again.is_ok(), "a dropped server frees its port at once");
    }
}
