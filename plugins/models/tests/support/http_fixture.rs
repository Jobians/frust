//! An in-process HTTP/1.1 server standing in for the Hub and its CDN.
//!
//! Two listeners on `127.0.0.1:0` share one handler: `hub` plays huggingface.co and `cdn` the
//! host its file URLs redirect to. Being on different ports they are different origins, so a
//! test can see which one received an `Authorization` header. Every request is logged.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use sha2::{Digest, Sha256};

/// Repository the fixture serves.
pub const REPO: &str = "acme/tiny";
/// The 3 MiB file.
pub const BLOB: &str = "model.gguf";
/// A nested 1.5 MiB file.
pub const NESTED: &str = "onnx/model.onnx";
/// A small file served without LFS, so the listing carries no SHA-256 for it.
pub const TOKENIZER: &str = "tokenizer.json";

/// One file of the fixture repository.
pub struct FixtureFile {
    pub path: &'static str,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub lfs: bool,
}

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct Logged {
    pub server: &'static str,
    pub target: String,
    pub range: Option<String>,
    pub authorization: Option<String>,
}

struct State {
    files: Vec<FixtureFile>,
    log: Mutex<Vec<Logged>>,
    ignore_range: AtomicBool,
    hub: OnceLock<String>,
    cdn: OnceLock<String>,
}

/// A running fixture. Its threads live until the test process exits.
pub struct Fixture {
    pub hub: String,
    pub cdn: String,
    state: Arc<State>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Deterministic pseudo-random bytes.
pub fn blob(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

fn file(path: &'static str, bytes: Vec<u8>, lfs: bool) -> FixtureFile {
    FixtureFile {
        path,
        sha256: sha256_hex(&bytes),
        bytes,
        lfs,
    }
}

impl Fixture {
    pub fn start() -> Self {
        let tokenizer = br#"{"version":"1.0","model":{"type":"BPE","vocab":{"a":0,"b":1}}}"#;
        let state = Arc::new(State {
            files: vec![
                file(BLOB, blob(3 * 1024 * 1024, 1), true),
                file(NESTED, blob(3 * 512 * 1024, 2), true),
                file(TOKENIZER, tokenizer.to_vec(), false),
            ],
            log: Mutex::new(Vec::new()),
            ignore_range: AtomicBool::new(false),
            hub: OnceLock::new(),
            cdn: OnceLock::new(),
        });
        let hub = listen(&state, "hub");
        let cdn = listen(&state, "cdn");
        let _ = state.hub.set(hub.clone());
        let _ = state.cdn.set(cdn.clone());
        Self { hub, cdn, state }
    }

    pub fn file(&self, path: &str) -> &FixtureFile {
        self.state.files.iter().find(|f| f.path == path).unwrap()
    }

    /// Makes every file route answer `200` with the whole body, whatever `Range` asked for.
    pub fn set_ignore_range(&self, on: bool) {
        self.state.ignore_range.store(on, Ordering::SeqCst);
    }

    pub fn log(&self) -> Vec<Logged> {
        self.state.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.state.log.lock().unwrap().clear();
    }
}

fn listen(state: &Arc<State>, server: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = state.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = state.clone();
            thread::spawn(move || {
                let _ = handle(&state, server, stream);
            });
        }
    });
    base
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    content_length: bool,
}

fn reply(status: u16, body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: body.into(),
        content_length: true,
    }
}

fn with(mut r: Reply, name: &'static str, value: impl Into<String>) -> Reply {
    r.headers.push((name, value.into()));
    r
}

fn handle(state: &State, server: &'static str, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let (mut range, mut authorization) = (None, None);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim().to_owned();
            match name.trim().to_ascii_lowercase().as_str() {
                "range" => range = Some(value),
                "authorization" => authorization = Some(value),
                _ => {}
            }
        }
    }
    state.log.lock().unwrap().push(Logged {
        server,
        target: target.clone(),
        range: range.clone(),
        authorization,
    });
    let response = route(state, &target, range.as_deref());
    write_reply(stream, response)
}

fn route(state: &State, target: &str, range: Option<&str>) -> Reply {
    let path = target.split('?').next().unwrap_or("/");
    let cdn = state.cdn.get().cloned().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').collect();
    for segment in &segments {
        match *segment {
            "gated" => {
                return with(reply(401, "gated"), "X-Error-Code", "GatedRepo");
            }
            "private" => {
                return with(reply(401, "missing"), "X-Error-Code", "RepoNotFound");
            }
            "forbidden" => return reply(403, "forbidden"),
            "ratelimit" => return with(reply(429, "slow down"), "Retry-After", "7"),
            "teapot" => return reply(418, "t".repeat(1000)),
            _ => {}
        }
    }
    if path == "/api/models" {
        let next = format!(
            "<{}/api/models?cursor=2>; rel=\"next\"",
            state.hub.get().unwrap()
        );
        return with(
            with(
                reply(200, search_json()),
                "Content-Type",
                "application/json",
            ),
            "Link",
            next,
        );
    }
    if path == format!("/api/models/{REPO}/tree/main") {
        return with(
            reply(200, tree_json(&state.files)),
            "Content-Type",
            "application/json",
        );
    }
    let find = |p: &str| state.files.iter().find(|f| f.path == p);
    if let Some(rest) = path.strip_prefix(&format!("/{REPO}/resolve/main/")) {
        return match find(rest) {
            Some(f) if f.lfs => with(reply(302, ""), "Location", format!("{cdn}/blobs/{rest}")),
            Some(f) => serve(state, &f.bytes, range),
            None => reply(404, "no such file"),
        };
    }
    if let Some(rest) = path.strip_prefix("/redirect/") {
        return with(reply(302, ""), "Location", format!("{cdn}/blobs/{rest}"));
    }
    if let Some(rest) = path.strip_prefix("/blobs/") {
        return match find(rest) {
            Some(f) => serve(state, &f.bytes, range),
            None => reply(404, "no such file"),
        };
    }
    if let Some(rest) = path.strip_prefix("/nolength/")
        && let Some(f) = find(rest)
    {
        let mut r = reply(200, f.bytes.clone());
        r.content_length = false;
        return r;
    }
    reply(404, "not found")
}

fn serve(state: &State, bytes: &[u8], range: Option<&str>) -> Reply {
    let len = bytes.len();
    let start = range
        .filter(|_| !state.ignore_range.load(Ordering::SeqCst))
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|s| s.parse::<usize>().ok());
    match start {
        None => reply(200, bytes.to_vec()),
        Some(start) if start >= len => {
            with(reply(416, ""), "Content-Range", format!("bytes */{len}"))
        }
        Some(start) => with(
            reply(206, bytes[start..].to_vec()),
            "Content-Range",
            format!("bytes {start}-{}/{len}", len - 1),
        ),
    }
}

fn write_reply(mut stream: TcpStream, r: Reply) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\n", r.status);
    for (name, value) in &r.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if r.content_length {
        head.push_str(&format!("Content-Length: {}\r\n", r.body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()?;
    stream.shutdown(std::net::Shutdown::Write)
}

/// Shaped after a live `GET /api/models?...&full=true` answer, trimmed to the fields read.
fn search_json() -> String {
    r#"[
  {"_id":"66a0c0ffee0000000000aaaa","id":"acme/tiny","author":"acme","gated":false,
   "lastModified":"2026-10-01T12:00:00.000Z","likes":12,"downloads":3400,"private":false,
   "tags":["gguf","text-generation","license:apache-2.0"],"pipeline_tag":"text-generation",
   "library_name":"gguf","createdAt":"2026-09-01T12:00:00.000Z","modelId":"acme/tiny",
   "siblings":[{"rfilename":".gitattributes"},{"rfilename":"model.gguf"},
               {"rfilename":"onnx/model.onnx"},{"rfilename":"tokenizer.json"}]},
  {"_id":"66a0c0ffee0000000000bbbb","id":"acme/big","author":"acme","gated":"manual",
   "lastModified":"2026-08-01T00:00:00.000Z","likes":900,"downloads":120000,"private":false,
   "tags":["gguf"],"modelId":"acme/big","siblings":[{"rfilename":"big.gguf"}]},
  {"_id":"66a0c0ffee0000000000cccc","id":"other/auto","gated":"auto","likes":1,"downloads":7,
   "private":false,"tags":["onnx"],"modelId":"other/auto"}
]"#
    .to_owned()
}

/// Shaped after a live `GET /api/models/{repo}/tree/main?recursive=true` answer.
fn tree_json(files: &[FixtureFile]) -> String {
    let mut entries = vec![
        r#"{"type":"file","oid":"a6344aac8c09253b3b630fb776ae94478aa0275b","size":1519,"path":".gitattributes"}"#.to_owned(),
        r#"{"type":"directory","oid":"5d5c4a1f8a3c22bb57a1a1d3c0a4a5f1a1d2e3f4","size":0,"path":"onnx"}"#.to_owned(),
    ];
    for f in files {
        let oid = &sha256_hex(f.path.as_bytes())[..40];
        let size = f.bytes.len();
        if f.lfs {
            entries.push(format!(
                r#"{{"type":"file","oid":"{oid}","size":{size},"lfs":{{"oid":"{}","size":{size},"pointerSize":135}},"xetHash":"{}","path":"{}"}}"#,
                f.sha256,
                &f.sha256[..32],
                f.path
            ));
        } else {
            entries.push(format!(
                r#"{{"type":"file","oid":"{oid}","size":{size},"path":"{}"}}"#,
                f.path
            ));
        }
    }
    format!("[{}]", entries.join(","))
}
