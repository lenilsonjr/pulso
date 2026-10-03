//! End-to-end tests: each one starts the real binary on its own directories
//! and talks HTTP to it. `PULSO_TEST_BIN` points them at another binary, for
//! example the static musl build.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};

const TOKEN: &str = "secret";

fn binary() -> PathBuf {
    std::env::var_os("PULSO_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_pulso-server")))
}

// ---------------------------------------------------------------- harness

/// A scratch directory holding a data directory and, beside it, an index
/// directory, so that listing the data directory shows only the data.
struct Workspace {
    root: tempfile::TempDir,
    data: PathBuf,
    index: PathBuf,
}

impl Workspace {
    fn new() -> Workspace {
        let root = tempfile::tempdir().unwrap();
        let (data, index) = (root.path().join("data"), root.path().join("index"));
        Workspace { root, data, index }
    }

    fn command(&self, args: &[&str], token: Option<&str>) -> Command {
        let mut command = Command::new(binary());
        command
            .args(args)
            .env_clear()
            .env("PULSO_DATA", &self.data)
            .env("PULSO_INDEX", &self.index)
            .env("PULSO_BIND", "127.0.0.1")
            .env("PULSO_PORT", "0")
            .current_dir(self.root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(token) = token {
            command.env("PULSO_TOKEN", token);
        }
        command
    }

    fn start(&self) -> Server {
        match self.try_start(self.command(&[], Some(TOKEN))) {
            Started::Running(server) => server,
            Started::Exited(exit) => panic!("the server did not start: {exit:?}"),
        }
    }

    fn try_start(&self, command: Command) -> Started {
        Server::launch(command)
    }

    /// Runs `pulso-server reindex` to completion.
    fn reindex(&self) -> Exit {
        run(self.command(&["reindex"], None))
    }

    fn file(&self, stem: &str) -> PathBuf {
        self.data.join(format!("{stem}.ndjson"))
    }

    fn text(&self, stem: &str) -> String {
        fs::read_to_string(self.file(stem)).unwrap_or_default()
    }

    fn lines(&self, stem: &str) -> Vec<Value> {
        self.text(stem)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The lines of a data file that parse; damaged ones are skipped.
    fn lines_ok(&self, stem: &str) -> Vec<Value> {
        self.text(stem)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Every data file's name and bytes.
    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(&self.data)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_file())
            .map(|entry| {
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }
}

#[derive(Debug)]
struct Exit {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

enum Started {
    Running(Server),
    Exited(Exit),
}

fn run(mut command: Command) -> Exit {
    let output = command.output().unwrap();
    Exit {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

struct Server {
    child: Child,
    port: u16,
    banner: String,
    stderr: Arc<Mutex<Vec<u8>>>,
    access_log: Arc<Mutex<Vec<u8>>>,
}

fn drain(
    mut pipe: impl Read + Send + 'static,
    sink: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    })
}

impl Server {
    fn launch(mut command: Command) -> Started {
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = drain(child.stderr.take().unwrap(), Arc::clone(&stderr));

        // The first stdout line is the banner. The rest is the access log; it is
        // always drained, so the server never blocks on a full pipe.
        let access_log = Arc::new(Mutex::new(Vec::new()));
        let (first_line, banner) = mpsc::channel();
        let stdout_thread = {
            let access_log = Arc::clone(&access_log);
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                let mut first = true;
                while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
                    if first {
                        first = false;
                        let _ = first_line.send(String::from_utf8_lossy(&line).into_owned());
                    } else {
                        access_log.lock().unwrap().extend_from_slice(&line);
                    }
                    line.clear();
                }
            })
        };

        match banner.recv_timeout(Duration::from_secs(60)) {
            Ok(line) => {
                let port = line
                    .split(" — ")
                    .next()
                    .and_then(|head| head.rsplit(':').next())
                    .and_then(|port| port.trim().parse().ok())
                    .unwrap_or_else(|| panic!("no port in the banner {line:?}"));
                Started::Running(Server {
                    child,
                    port,
                    banner: line,
                    stderr,
                    access_log,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait().unwrap();
                stdout_thread.join().unwrap();
                stderr_thread.join().unwrap();
                let text = |bytes: &Mutex<Vec<u8>>| {
                    String::from_utf8_lossy(&bytes.lock().unwrap()).into_owned()
                };
                Started::Exited(Exit {
                    code: status.code(),
                    stdout: text(&access_log),
                    stderr: text(&stderr),
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("the server printed no banner and did not exit");
            }
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    /// SIGKILL: no chance to flush or clean up.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn send(&self, request: &[u8]) -> Response {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        Response::parse(&raw)
    }

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
        let mut head =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if !body.is_empty() || method == "POST" {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut request = head.into_bytes();
        request.extend_from_slice(body);
        self.send(&request)
    }

    fn get(&self, path: &str, token: Option<&str>) -> Response {
        let auth = token.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("Authorization", a.as_str())).collect();
        self.request("GET", path, &headers, b"")
    }

    fn post(&self, elements: &Value) -> Response {
        self.post_with(elements, false, Some(TOKEN))
    }

    fn post_with(&self, elements: &Value, gzipped: bool, token: Option<&str>) -> Response {
        let body = serde_json::to_vec(elements).unwrap();
        self.post_bytes(&body, gzipped, token)
    }

    fn post_bytes(&self, body: &[u8], gzipped: bool, token: Option<&str>) -> Response {
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(auth) = &auth {
            headers.push(("Authorization", auth.as_str()));
        }
        if gzipped {
            headers.push(("Content-Encoding", "gzip"));
            self.request("POST", "/ingest", &headers, &gzip(body))
        } else {
            self.request("POST", "/ingest", &headers, body)
        }
    }

    /// The reply counts of an ingest that must succeed.
    fn ingest(&self, elements: &Value) -> (u64, u64, u64) {
        let reply = self.post(elements);
        assert_eq!(reply.status, 200, "{}", reply.text());
        let body = reply.json();
        (
            body["received"].as_u64().unwrap(),
            body["new"].as_u64().unwrap(),
            body["deleted"].as_u64().unwrap(),
        )
    }

    fn latest(&self) -> Value {
        let reply = self.get("/latest", Some(TOKEN));
        assert_eq!(reply.status, 200);
        reply.json()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn parse(raw: &[u8]) -> Response {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or_else(|| panic!("no response: {raw:?}"));
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.lines();
        let status = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        Response {
            status,
            headers,
            body: raw[split + 4..].to_vec(),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// Anonymous memory of a process in bytes: `RssAnon` where the kernel has it,
/// else the sum of the `Anonymous:` lines of smaps (gVisor has no `RssAnon`).
fn anonymous_bytes(pid: u32) -> u64 {
    let kib = |text: &str| {
        text.trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .unwrap()
            * 1024
    };
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status"))
        && let Some(value) = status
            .lines()
            .find_map(|line| line.strip_prefix("RssAnon:"))
    {
        return kib(value);
    }
    let smaps = fs::read_to_string(format!("/proc/{pid}/smaps")).unwrap();
    smaps
        .lines()
        .filter_map(|line| line.strip_prefix("Anonymous:"))
        .map(kib)
        .sum()
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn samples() -> Value {
    json!([
        {
            "uuid": "AAAA-1111", "type": "sleepAnalysis",
            "start": "2026-07-06T01:12:00+01:00", "end": "2026-07-06T02:40:00+01:00",
            "value": "asleepREM", "source": "Apple Watch", "metadata": {"timeZone": "Europe/Lisbon"}
        },
        {
            "uuid": "BBBB-2222", "type": "heartRate",
            "start": "2026-07-06T08:00:00+01:00", "end": "2026-07-06T08:00:00+01:00",
            "value": 58, "unit": "count/min", "source": "Apple Watch"
        },
        {"deleted": ["CCCC-3333", "DDDD-4444"]}
    ])
}

fn sample(uuid: &str, kind: &str, end: &str) -> Value {
    json!({"uuid": uuid, "type": kind, "start": end, "end": end, "value": 1})
}

// ------------------------------------------- the cases of test_server.py
//
// Rust name = Python name without `test_`. The token is mandatory here, so
// requests carry it unless a case is about sending none; restarts kill the
// process instead of shutting down an in-process server.

#[test]
fn health() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.get("/health", None);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["status"], "ok");
}

#[test]
fn ingest_and_idempotency() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.post(&samples());
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({"received": 3, "new": 2, "deleted": 2}));

    // Re-sending the identical batch must be a no-op.
    let reply = server.post(&samples());
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({"received": 3, "new": 0, "deleted": 0}));

    let sleep = ws.lines("sleepAnalysis");
    assert_eq!(sleep.len(), 1);
    assert_eq!(sleep[0]["uuid"], "AAAA-1111");
    assert_eq!(sleep[0]["value"], "asleepREM");
    assert!(sleep[0].get("receivedAt").is_some());
    assert_eq!(ws.lines("heartRate").len(), 1);
    let tombstones = ws.lines("_deleted");
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0]["deleted"], json!(["CCCC-3333", "DDDD-4444"]));
}

#[test]
fn gzip_body() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.post_with(&samples(), true, Some(TOKEN));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["new"], 2);
}

#[test]
fn dedup_survives_restart() {
    let ws = Workspace::new();
    let server = ws.start();
    server.post(&samples());
    server.kill();
    let server = ws.start();
    assert_eq!(
        server.post(&samples()).json(),
        json!({"received": 3, "new": 0, "deleted": 0})
    );
}

#[test]
fn auth() {
    let ws = Workspace::new();
    let server = ws.start();
    assert_eq!(server.post_with(&samples(), false, None).status, 401);
    assert_eq!(
        server.post_with(&samples(), false, Some("wrong")).status,
        401
    );
    let reply = server.post_with(&samples(), false, Some("secret"));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["new"], 2);
}

#[test]
fn type_is_sanitized_for_file_name() {
    let ws = Workspace::new();
    let server = ws.start();
    let evil = json!([{"uuid": "EE-55", "type": "../../etc/passwd", "start": "x", "end": "x"}]);
    let reply = server.post(&evil);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["new"], 1);

    let names: Vec<String> = fs::read_dir(&ws.data)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 1);
    assert!(!names[0].replace(".ndjson", "").contains('/'));
    assert!(!names[0].contains(".."));

    let mut top: Vec<String> = fs::read_dir(ws.root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(
        top,
        ["data", "index"],
        "nothing was written outside the data directory"
    );
}

#[test]
fn bad_bodies() {
    let ws = Workspace::new();
    let server = ws.start();
    assert_eq!(
        server.post_bytes(b"not json", false, Some(TOKEN)).status,
        400
    );
    assert_eq!(
        server
            .post_bytes(br#"{"not": "array"}"#, false, Some(TOKEN))
            .status,
        400
    );
    let reply = server.request(
        "POST",
        "/ingest",
        &[
            ("Authorization", "Bearer secret"),
            ("Content-Encoding", "gzip"),
        ],
        b"\x1f\x8bgarbage",
    );
    assert_eq!(reply.status, 400);
}

#[test]
fn unknown_paths() {
    let ws = Workspace::new();
    let server = ws.start();
    assert_eq!(server.get("/nope", None).status, 404);
    assert_eq!(server.request("POST", "/nope", &[], b"[]").status, 404);
    assert_eq!(server.get("/nope", Some(TOKEN)).status, 404);
}

#[test]
fn latest_endpoint() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.get("/latest", Some(TOKEN));
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.json(),
        json!({}),
        "empty store has no latest timestamps"
    );

    // Comparison must be in absolute time: 05:00-04:00 (09:00Z) is later
    // than 09:03+01:00 (08:03Z) despite sorting earlier as a string.
    let mut batch = samples();
    batch.as_array_mut().unwrap().push(json!({
        "uuid": "LATE-1", "type": "sleepAnalysis",
        "start": "2026-07-06T01:00:00-04:00", "end": "2026-07-06T05:00:00-04:00",
        "value": "inBed", "source": "WHOOP"
    }));
    server.post(&batch);
    let body = server.latest();
    assert_eq!(body["sleepAnalysis"], "2026-07-06T05:00:00-04:00");
    assert_eq!(body["heartRate"], "2026-07-06T08:00:00+01:00");
    assert!(
        body.get("_deleted").is_none(),
        "tombstones carry no timestamps"
    );

    // 'Z' (zero UTC offset, Lisbon winter) must parse, and 10:00Z beats 05:00-04:00 (09:00Z).
    server.post(&json!([{
        "uuid": "ZULU-1", "type": "sleepAnalysis",
        "start": "2026-07-06T09:00:00Z", "end": "2026-07-06T10:00:00Z",
        "value": "awake", "source": "Watch"
    }]));
    assert_eq!(server.latest()["sleepAnalysis"], "2026-07-06T10:00:00Z");
}

#[test]
fn latest_survives_restart() {
    let ws = Workspace::new();
    let server = ws.start();
    server.post(&samples());
    server.kill();
    let server = ws.start();
    assert_eq!(
        server.latest()["sleepAnalysis"],
        "2026-07-06T02:40:00+01:00"
    );
}

#[test]
fn latest_requires_auth_when_token_set() {
    let ws = Workspace::new();
    let server = ws.start();
    assert_eq!(server.get("/latest", None).status, 401);
    assert_eq!(server.get("/latest", Some(TOKEN)).status, 200);
}

// ------------------------------------------------ what the rewrite adds

#[test]
fn refuses_to_start_without_a_token() {
    for token in [None, Some(""), Some("   ")] {
        let ws = Workspace::new();
        let exit = match ws.try_start(ws.command(&[], token)) {
            Started::Exited(exit) => exit,
            Started::Running(_) => panic!("started without a token: {token:?}"),
        };
        assert_eq!(exit.code, Some(2), "{exit:?}");
        assert!(exit.stderr.contains("PULSO_TOKEN"), "{exit:?}");
        assert!(
            exit.stdout.is_empty(),
            "no banner without a token: {exit:?}"
        );
        assert!(
            !ws.data.exists() && !ws.index.exists(),
            "a refused start must create nothing"
        );
    }
}

#[test]
fn bad_settings_and_unknown_commands_are_usage_errors() {
    let ws = Workspace::new();
    let mut command = ws.command(&[], Some(TOKEN));
    command.env("PULSO_PORT", "http");
    let exit = run(command);
    assert_eq!(exit.code, Some(2), "{exit:?}");
    assert!(exit.stderr.contains("PULSO_PORT"), "{exit:?}");

    let exit = run(ws.command(&["frobnicate"], Some(TOKEN)));
    assert_eq!(exit.code, Some(2), "{exit:?}");
    let exit = run(ws.command(&["reindex", "extra"], None));
    assert_eq!(exit.code, Some(2), "{exit:?}");

    let exit = run(ws.command(&["help"], None));
    assert_eq!(exit.code, Some(0));
    assert!(
        exit.stdout.contains("PULSO_TOKEN") && exit.stdout.contains("reindex"),
        "{exit:?}"
    );
    assert!(!ws.data.exists() && !ws.index.exists());
}

#[test]
fn listens_where_it_is_told_to() {
    let ws = Workspace::new();
    let port = free_port();
    let mut command = ws.command(&[], Some(TOKEN));
    command.env("PULSO_PORT", port.to_string());
    let Started::Running(server) = ws.try_start(command) else {
        panic!("did not start")
    };
    assert_eq!(server.port, port);
    assert!(
        server
            .banner
            .starts_with(&format!("pulso server on 127.0.0.1:{port} — data: ")),
        "{}",
        server.banner
    );
    assert!(server.banner.contains("bearer token required"));
    assert_eq!(server.get("/health", None).status, 200);
}

#[test]
fn a_second_ingest_of_every_file_adds_nothing() {
    let ws = Workspace::new();
    let server = ws.start();
    let kinds = ["heartRate", "stepCount", "sleepAnalysis", "odd type!"];
    let mut all: Vec<Value> = (0..600)
        .map(|n| {
            sample(
                &format!("U{n}"),
                kinds[n % 4],
                &format!("2026-07-06T{:02}:{:02}:00+01:00", n / 60 % 24, n % 60),
            )
        })
        .collect();
    all.push(json!({"deleted": ["T1", "T2", "T3"]}));
    all.push(json!({"deleted": ["T4"]}));
    for chunk in all.chunks(250) {
        let reply = server.post_with(&Value::Array(chunk.to_vec()), true, Some(TOKEN));
        assert_eq!(reply.status, 200);
    }
    let before = ws.snapshot();
    assert_eq!(before.len(), 5);

    let resend = |server: &Server| {
        for (name, bytes) in &before {
            let elements: Vec<Value> = String::from_utf8_lossy(bytes)
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            for chunk in elements.chunks(100) {
                let reply = server
                    .post_with(&Value::Array(chunk.to_vec()), true, Some(TOKEN))
                    .json();
                assert_eq!(
                    reply,
                    json!({"received": chunk.len(), "new": 0, "deleted": 0}),
                    "re-sending {name}"
                );
            }
        }
    };
    resend(&server);
    assert_eq!(before, ws.snapshot(), "no file may change");

    server.kill();
    let server = ws.start();
    resend(&server);
    assert_eq!(
        before,
        ws.snapshot(),
        "no file may change after a restart either"
    );
}

/// Files as the Python server wrote them, with the damage a long-lived data
/// directory collects: blank and unreadable lines, a tombstone file.
fn python_written_files(ws: &Workspace) {
    fs::create_dir_all(&ws.data).unwrap();
    let line = |uuid: &str, kind: &str, end: &str| {
        format!(
            "{{\"uuid\":\"{uuid}\",\"type\":\"{kind}\",\"start\":\"{end}\",\"end\":\"{end}\",\"value\":58,\"receivedAt\":\"2026-07-07T09:00:00+01:00\"}}\n"
        )
    };
    let heart = [
        line("H1", "heartRate", "2026-07-06T08:00:00+01:00"),
        "\n".to_owned(),
        line("H2", "heartRate", "2026-07-06T05:00:00-04:00"),
        "not json\n".to_owned(),
        line("H3", "heartRate", "2026-07-05T23:00:00Z"),
    ]
    .concat();
    fs::write(ws.file("heartRate"), heart).unwrap();
    fs::write(
        ws.file("sleepAnalysis"),
        [
            line("S1", "sleepAnalysis", "2026-07-06T01:00:00+01:00"),
            line("S2", "sleepAnalysis", "2026-07-06T09:03:00+01:00"),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        ws.file("_deleted"),
        "{\"deleted\":[\"X1\",\"X2\"],\"receivedAt\":\"2026-07-07T09:00:00+01:00\"}\n{\"deleted\":[\"X3\"],\"receivedAt\":\"r\"}\n",
    )
    .unwrap();
}

#[test]
fn existing_files_are_served_in_place_after_a_reindex() {
    let ws = Workspace::new();
    python_written_files(&ws);
    let originals = ws.snapshot();

    // Without an index the server refuses to start, and says what to run.
    let exit = match ws.try_start(ws.command(&[], Some(TOKEN))) {
        Started::Exited(exit) => exit,
        Started::Running(_) => panic!("started on existing files without an index"),
    };
    assert_ne!(exit.code, Some(0));
    assert!(exit.stderr.contains("pulso-server reindex"), "{exit:?}");
    assert_eq!(ws.snapshot(), originals);

    let exit = ws.reindex();
    assert_eq!(exit.code, Some(0), "{exit:?}");
    assert!(
        exit.stdout
            .contains("indexed 5 uuids and 3 deleted uuids from 3 files"),
        "{exit:?}"
    );
    assert_eq!(ws.snapshot(), originals, "reindex must not touch the data");

    let server = ws.start();
    // H2 ends at 09:00Z, later than H1's 07:00Z and H3's 23:00Z the day before.
    assert_eq!(
        server.latest(),
        json!({"heartRate": "2026-07-06T05:00:00-04:00", "sleepAnalysis": "2026-07-06T09:03:00+01:00"})
    );
    let mut again = vec![];
    for (name, bytes) in &originals {
        for line in String::from_utf8_lossy(bytes)
            .lines()
            .filter(|l| l.starts_with('{'))
        {
            again.push((name.clone(), serde_json::from_str::<Value>(line).unwrap()));
        }
    }
    let elements: Vec<Value> = again.into_iter().map(|(_, v)| v).collect();
    assert_eq!(elements.len(), 7);
    assert_eq!(server.ingest(&Value::Array(elements)), (7, 0, 0));
    assert_eq!(ws.snapshot(), originals);

    assert_eq!(
        server.ingest(&json!([sample(
            "NEW",
            "heartRate",
            "2026-07-08T08:00:00+01:00"
        )])),
        (1, 1, 0)
    );
    let after = ws.snapshot();
    for ((_, before), (_, now)) in originals.iter().zip(&after) {
        assert!(now.starts_with(before), "what was there stays as it was");
    }
    assert_eq!(server.latest()["heartRate"], "2026-07-08T08:00:00+01:00");
}

#[test]
fn the_index_is_rebuilt_only_by_the_reindex_command() {
    let ws = Workspace::new();
    let server = ws.start();
    assert_eq!(server.ingest(&samples()), (3, 2, 2));
    server.kill();
    let data = ws.snapshot();

    fs::remove_dir_all(&ws.index).unwrap();
    let exit = match ws.try_start(ws.command(&[], Some(TOKEN))) {
        Started::Exited(exit) => exit,
        Started::Running(_) => panic!("started without an index"),
    };
    assert!(exit.stderr.contains("pulso-server reindex"), "{exit:?}");
    assert!(
        !ws.index.join("seen.sorted").exists(),
        "starting must not rebuild the index"
    );
    assert!(!ws.index.join("state.json").exists());

    assert_eq!(ws.reindex().code, Some(0));
    assert_eq!(ws.snapshot(), data);
    let server = ws.start();
    assert_eq!(server.ingest(&samples()), (3, 0, 0));
    assert_eq!(server.latest()["heartRate"], "2026-07-06T08:00:00+01:00");
}

#[test]
fn the_index_defaults_to_a_hidden_directory_inside_the_data_directory() {
    let ws = Workspace::new();
    let mut command = ws.command(&[], Some(TOKEN));
    command.env_remove("PULSO_INDEX");
    let Started::Running(server) = ws.try_start(command) else {
        panic!("did not start")
    };
    server.ingest(&samples());
    assert!(ws.data.join(".index").join("state.json").is_file());
    assert!(!ws.index.exists());
    assert!(
        server
            .banner
            .contains(&format!("index: {}", ws.data.join(".index").display())),
        "{}",
        server.banner
    );
    let mut names: Vec<String> = fs::read_dir(&ws.data)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".index",
            "_deleted.ndjson",
            "heartRate.ndjson",
            "sleepAnalysis.ndjson"
        ]
    );
}

#[test]
fn an_index_in_use_cannot_be_shared() {
    let ws = Workspace::new();
    let first = ws.start();
    let exit = match ws.try_start(ws.command(&[], Some(TOKEN))) {
        Started::Exited(exit) => exit,
        Started::Running(_) => panic!("a second server took over the index"),
    };
    assert_ne!(exit.code, Some(0));
    assert!(exit.stderr.contains("another pulso-server"), "{exit:?}");
    let exit = ws.reindex();
    assert_ne!(exit.code, Some(0));
    assert!(exit.stderr.contains("another pulso-server"), "{exit:?}");
    assert_eq!(first.ingest(&samples()), (3, 2, 2));
}

#[test]
fn a_killed_server_recovers_what_it_had_not_indexed() {
    let ws = Workspace::new();
    let server = ws.start();
    server.ingest(&samples());
    server.kill();

    // A crash after the data was written but before the index was: a complete
    // line, and a line cut short.
    let mut heart = fs::OpenOptions::new()
        .append(true)
        .open(ws.file("heartRate"))
        .unwrap();
    writeln!(
        heart,
        "{}",
        sample("LOST", "heartRate", "2026-07-09T08:00:00+01:00")
    )
    .unwrap();
    let mut sleep = fs::OpenOptions::new()
        .append(true)
        .open(ws.file("sleepAnalysis"))
        .unwrap();
    write!(sleep, "{{\"uuid\":\"TORN\",\"ty").unwrap();
    drop((heart, sleep));

    let server = ws.start();
    assert_eq!(server.latest()["heartRate"], "2026-07-09T08:00:00+01:00");
    assert_eq!(
        server.ingest(&json!([sample(
            "LOST",
            "heartRate",
            "2026-07-09T08:00:00+01:00"
        )])),
        (1, 0, 0)
    );
    assert_eq!(
        server.ingest(&json!([sample(
            "NEXT",
            "sleepAnalysis",
            "2026-07-10T08:00:00+01:00"
        )])),
        (1, 1, 0)
    );

    let text = ws.text("sleepAnalysis");
    assert!(text.ends_with('\n'));
    let unreadable = text
        .lines()
        .filter(|line| serde_json::from_str::<Value>(line).is_err())
        .count();
    assert_eq!(
        unreadable, 1,
        "only the cut line is lost, the next record is whole: {text}"
    );
}

#[test]
fn data_that_changed_behind_the_index_stops_the_start() {
    let ws = Workspace::new();
    let server = ws.start();
    server.ingest(&samples());
    server.kill();

    let len = fs::metadata(ws.file("heartRate")).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(ws.file("heartRate"))
        .unwrap()
        .set_len(len - 5)
        .unwrap();
    let exit = match ws.try_start(ws.command(&[], Some(TOKEN))) {
        Started::Exited(exit) => exit,
        Started::Running(_) => panic!("started on a file shorter than its index"),
    };
    assert!(
        exit.stderr.contains("heartRate.ndjson") && exit.stderr.contains("reindex"),
        "{exit:?}"
    );

    assert_eq!(ws.reindex().code, Some(0));
    let server = ws.start();
    // The cut line no longer holds its sample, so that one is new again; the rest is known.
    assert_eq!(server.ingest(&samples()), (3, 1, 0));
    let text = ws.text("heartRate");
    let unreadable = text
        .lines()
        .filter(|line| serde_json::from_str::<Value>(line).is_err())
        .count();
    assert_eq!(unreadable, 1, "{text}");
    assert_eq!(
        ws.lines_ok("heartRate").last().unwrap()["uuid"],
        "BBBB-2222"
    );
}

#[test]
fn a_body_that_does_not_parse_stores_nothing() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.post_bytes(
        br#"[{"uuid":"a","type":"t"},{"uuid":"b","type":"t"},{"uuid":"c","#,
        false,
        Some(TOKEN),
    );
    assert_eq!(reply.status, 400);
    assert!(!ws.file("t").exists());
    assert_eq!(server.latest(), json!({}));
    assert_eq!(
        server.ingest(&json!([{"uuid": "a", "type": "t"}, {"uuid": "b", "type": "t"}])),
        (2, 2, 0)
    );
}

#[test]
fn a_storage_error_is_reported_and_the_retry_succeeds() {
    let ws = Workspace::new();
    let server = ws.start();
    // A directory where a data file belongs: the second sample cannot be stored.
    fs::create_dir(ws.file("blocked")).unwrap();
    let batch = json!([
        sample("A", "good", "2026-07-06T08:00:00Z"),
        sample("B", "blocked", "2026-07-06T08:00:00Z")
    ]);
    let reply = server.post(&batch);
    assert_eq!(reply.status, 500, "{}", reply.text());
    assert_eq!(reply.json(), json!({"error": "storage error"}));
    let wait = Instant::now();
    while !server.stderr().contains("blocked.ndjson") && wait.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        server.stderr().contains("ingest failed") && server.stderr().contains("blocked.ndjson"),
        "{}",
        server.stderr()
    );
    assert_eq!(server.get("/health", None).status, 200);

    // The retry finds the first sample already stored and adds only the second.
    fs::remove_dir(ws.file("blocked")).unwrap();
    assert_eq!(server.ingest(&batch), (2, 1, 0));
    assert_eq!((ws.lines("good").len(), ws.lines("blocked").len()), (1, 1));
    assert_eq!(server.ingest(&batch), (2, 0, 0));
}

#[test]
fn stored_lines_keep_the_shape_the_python_server_wrote() {
    let ws = Workspace::new();
    let server = ws.start();
    let element = r#"{"uuid":"U-1","type":"heartRate","start":"2026-07-06T08:00:00+01:00","end":"2026-07-06T08:00:00+01:00","value":58.0,"extra":[1,2.50,1e5,-0.0,1e-7,123456789.123456789],"name":"é日本 \"q\" \\ \n \u0007","receivedAt":"old","metadata":{"z":1,"a":{"b":[true,null]}}}"#;
    let reply = server.post_bytes(format!("[{element}]").as_bytes(), false, Some(TOKEN));
    assert_eq!(reply.status, 200);

    // What `json.dumps(obj, separators=(",", ":"), ensure_ascii=False)` makes of the element,
    // with the stamp the server wrote in the place `receivedAt` already had.
    let expected = r#"{"uuid":"U-1","type":"heartRate","start":"2026-07-06T08:00:00+01:00","end":"2026-07-06T08:00:00+01:00","value":58.0,"extra":[1,2.5,100000.0,-0.0,1e-07,123456789.12345679],"name":"é日本 \"q\" \\ \n \u0007","receivedAt":"STAMP","metadata":{"z":1,"a":{"b":[true,null]}}}"#;
    let stored = ws.text("heartRate");
    let (head, rest) = stored.split_once("\"receivedAt\":\"").unwrap();
    let (stamp, tail) = rest.split_once('"').unwrap();
    assert_eq!(
        format!("{head}\"receivedAt\":\"STAMP\"{tail}"),
        format!("{expected}\n")
    );
    let b = stamp.as_bytes();
    assert!(
        stamp.len() == 25 && b[10] == b'T' && (b[19] == b'+' || b[19] == b'-') && b[22] == b':',
        "{stamp}"
    );
}

#[test]
fn received_at_is_the_server_local_time() {
    for (zone, suffix) in [("UTC-3", "+03:00"), ("UTC", "+00:00"), ("EST5", "-05:00")] {
        let ws = Workspace::new();
        let mut command = ws.command(&[], Some(TOKEN));
        command.env("TZ", zone);
        let Started::Running(server) = ws.try_start(command) else {
            panic!("did not start")
        };
        server.ingest(&json!([sample("Z", "t", "2026-07-06T08:00:00Z")]));
        let stamp = ws.lines("t")[0]["receivedAt"].as_str().unwrap().to_owned();
        assert!(stamp.ends_with(suffix), "TZ={zone}: {stamp}");
    }
}

#[test]
fn request_size_limits() {
    let ws = Workspace::new();
    let server = ws.start();
    let head = |extra: &str| {
        format!("POST /ingest HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}\r\n")
    };

    // Above the limit: refused from the header alone, without waiting for a body.
    let started = Instant::now();
    let reply = server
        .send(head("Authorization: Bearer secret\r\nContent-Length: 67108865\r\n").as_bytes());
    assert_eq!(reply.status, 413);
    assert_eq!(reply.json(), json!({"error": "bad content length"}));
    assert!(started.elapsed() < Duration::from_secs(10));

    // No length, or an empty body.
    let reply =
        server.send(head("Authorization: Bearer secret\r\nContent-Length: 0\r\n").as_bytes());
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json(), json!({"error": "bad content length"}));
    let reply = server
        .send(head("Authorization: Bearer secret\r\nTransfer-Encoding: chunked\r\n").as_bytes());
    assert_eq!(reply.status, 400);

    // Authentication comes first, then the length.
    let reply = server.send(head("Content-Length: 67108865\r\n").as_bytes());
    assert_eq!(reply.status, 401);
    let reply =
        server.send(head("Authorization: Bearer wrong\r\nContent-Length: 0\r\n").as_bytes());
    assert_eq!(reply.status, 401);

    // A small body that inflates past the limit is refused, not inflated.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..65 {
        encoder.write_all(&zeros).unwrap();
    }
    let bomb = encoder.finish().unwrap();
    assert!(bomb.len() < 1 << 20);
    let reply = server.request(
        "POST",
        "/ingest",
        &[
            ("Authorization", "Bearer secret"),
            ("Content-Encoding", "gzip"),
        ],
        &bomb,
    );
    assert_eq!(reply.status, 413);
    assert_eq!(reply.json(), json!({"error": "body too large"}));
    assert_eq!(
        server.get("/health", None).status,
        200,
        "the server is still up"
    );
}

#[test]
fn replies_are_json_and_errors_say_what_went_wrong() {
    let ws = Workspace::new();
    let server = ws.start();
    let cases = [
        (server.get("/health", None), 200, json!({"status": "ok"})),
        (
            server.get("/nope", None),
            404,
            json!({"error": "not found"}),
        ),
        (
            server.get("/latest", None),
            401,
            json!({"error": "unauthorized"}),
        ),
        (
            server.post_with(&samples(), false, None),
            401,
            json!({"error": "unauthorized"}),
        ),
        (
            server.post_bytes(b"nope", false, Some(TOKEN)),
            400,
            json!({"error": "bad json"}),
        ),
        (
            server.post_bytes(b"{}", false, Some(TOKEN)),
            400,
            json!({"error": "body must be a json array"}),
        ),
        (
            server.post_bytes(b"[]", true, Some(TOKEN)),
            200,
            json!({"received": 0, "new": 0, "deleted": 0}),
        ),
        (
            server.post_bytes(b"\x1f\x8b", false, Some(TOKEN)),
            400,
            json!({"error": "bad json"}),
        ),
    ];
    for (reply, status, body) in cases {
        assert_eq!(reply.status, status, "{}", reply.text());
        assert_eq!(reply.header("content-type"), Some("application/json"));
        assert_eq!(reply.json(), body);
        assert_eq!(
            reply.header("content-length"),
            Some(reply.body.len().to_string().as_str())
        );
    }
    let reply = server.request(
        "POST",
        "/ingest",
        &[
            ("Authorization", "Bearer secret"),
            ("Content-Encoding", "gzip"),
        ],
        b"\x1f\x8bx",
    );
    assert_eq!(
        (reply.status, reply.json()),
        (400, json!({"error": "bad gzip body"}))
    );
    let reply = server.request(
        "POST",
        "/ingest",
        &[
            ("Authorization", "Bearer secret"),
            ("Content-Encoding", "GZIP"),
        ],
        &gzip(b"[]"),
    );
    assert_eq!(reply.status, 200, "the encoding name is not case sensitive");
}

#[test]
fn paths_are_matched_exactly() {
    let ws = Workspace::new();
    let server = ws.start();
    for target in [
        "/",
        "/health/",
        "/HEALTH",
        "/health?x=1",
        "/latest?x=1",
        "//health",
    ] {
        assert_eq!(server.get(target, Some(TOKEN)).status, 404, "GET {target}");
    }
    assert_eq!(
        server
            .request(
                "POST",
                "/ingest?x=1",
                &[("Authorization", "Bearer secret")],
                b"[]"
            )
            .status,
        404
    );
    assert_eq!(server.get("/ingest", Some(TOKEN)).status, 404);
    assert_eq!(server.request("POST", "/health", &[], b"[]").status, 404);
    assert_eq!(server.request("POST", "/latest", &[], b"[]").status, 404);
    // Methods the protocol does not use are unknown routes too.
    for method in ["PUT", "DELETE", "PATCH", "OPTIONS"] {
        assert_eq!(
            server
                .request(
                    method,
                    "/ingest",
                    &[("Authorization", "Bearer secret")],
                    b""
                )
                .status,
            404,
            "{method}"
        );
    }
}

#[test]
fn a_connection_can_be_reused_for_several_requests() {
    let ws = Workspace::new();
    let server = ws.start();
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut exchange = |request: String, body: &[u8]| {
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            head.push_str(&line);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (
            head.lines().next().unwrap().to_owned(),
            String::from_utf8(body).unwrap(),
        )
    };

    let body = serde_json::to_vec(&samples()).unwrap();
    let post = format!(
        "POST /ingest HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    assert_eq!(
        exchange("GET /health HTTP/1.1\r\nHost: x\r\n\r\n".into(), b"").0,
        "HTTP/1.1 200 OK"
    );
    let (status, reply) = exchange(post.clone(), &body);
    assert_eq!(
        (status.as_str(), reply.as_str()),
        (
            "HTTP/1.1 200 OK",
            r#"{"received": 3, "new": 2, "deleted": 2}"#
        )
    );
    let (status, reply) = exchange(post, &body);
    assert_eq!(
        (status.as_str(), reply.as_str()),
        (
            "HTTP/1.1 200 OK",
            r#"{"received": 3, "new": 0, "deleted": 0}"#
        )
    );
    let (status, reply) = exchange(
        "GET /latest HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret\r\n\r\n".into(),
        b"",
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(reply.contains("sleepAnalysis"), "{reply}");
}

#[test]
fn http_1_0_clients_are_served() {
    let ws = Workspace::new();
    let server = ws.start();
    let reply = server.send(b"GET /health HTTP/1.0\r\n\r\n");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["status"], "ok");
}

#[test]
fn identical_batches_sent_at_once_are_stored_once() {
    let ws = Workspace::new();
    let server = ws.start();
    let elements: Vec<Value> = (0..2000)
        .map(|n| {
            sample(
                &format!("C{n}"),
                if n % 2 == 0 { "a" } else { "b" },
                "2026-07-06T08:00:00Z",
            )
        })
        .collect();
    let body = serde_json::to_vec(&elements).unwrap();

    let new: Vec<u64> = thread::scope(|scope| {
        let threads: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    server.post_bytes(&body, false, Some(TOKEN)).json()["new"]
                        .as_u64()
                        .unwrap()
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(
        new.iter().sum::<u64>(),
        2000,
        "each sample is new to exactly one request: {new:?}"
    );
    assert_eq!(ws.lines("a").len() + ws.lines("b").len(), 2000);
}

#[test]
fn the_token_is_compared_whole() {
    let ws = Workspace::new();
    let server = ws.start();
    for header in [
        "Bearer secre",
        "Bearer secrett",
        "bearer secret",
        "Bearer  secret",
        "secret",
        "Basic c2VjcmV0",
        "Bearer ",
    ] {
        let reply = server.request("GET", "/latest", &[("Authorization", header)], b"");
        assert_eq!(reply.status, 401, "{header}");
    }
    assert_eq!(
        server
            .request("GET", "/latest", &[("authorization", "Bearer secret")], b"")
            .status,
        200
    );
}

#[test]
fn every_request_is_logged_with_its_outcome() {
    let ws = Workspace::new();
    let server = ws.start();
    server.ingest(&samples());
    server.get("/nope", None);
    let wait = Instant::now();
    let log = loop {
        let log = String::from_utf8_lossy(&server.access_log.lock().unwrap()).into_owned();
        if log.lines().count() >= 2 || wait.elapsed() > Duration::from_secs(10) {
            break log;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        log.contains("\"POST /ingest\" 200 received=3 new=2 deleted=2"),
        "{log}"
    );
    assert!(log.contains("\"GET /nope\" 404"), "{log}");
    assert!(!log.contains("secret"), "the token must never be logged");
}

#[test]
fn anonymous_memory_stays_small_while_ingesting() {
    const LIMIT: u64 = 50 * 1024 * 1024;
    const SAMPLES: usize = 100_000;
    const BATCH: usize = 5_000;

    let batch = |from: usize| {
        let elements: Vec<String> = (from..from + BATCH)
            .map(|n| {
                format!(
                    r#"{{"uuid":"{n:08X}-0000-4000-8000-{:012X}","type":"type{}","start":"2026-07-06T08:00:00+01:00","end":"2026-07-06T08:00:{:02}+01:00","value":{n},"unit":"count/min","source":"Apple Watch","sourceBundleId":"com.apple.health.A1B2C3","metadata":{{"timeZone":"Europe/Lisbon"}}}}"#,
                    n * 7919,
                    n % 8,
                    n % 60
                )
            })
            .collect();
        gzip(format!("[{}]", elements.join(",")).as_bytes())
    };
    let post = |server: &Server, body: &[u8]| {
        server.request(
            "POST",
            "/ingest",
            &[
                ("Authorization", "Bearer secret"),
                ("Content-Encoding", "gzip"),
            ],
            body,
        )
    };
    // Peak anonymous memory seen while `work` runs.
    let watch = |pid: u32, work: &mut dyn FnMut()| {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let peak = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sampler = {
            let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
            thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    peak.fetch_max(anonymous_bytes(pid), std::sync::atomic::Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(10));
                }
            })
        };
        work();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        sampler.join().unwrap();
        peak.load(std::sync::atomic::Ordering::Relaxed)
    };

    let ws = Workspace::new();
    let server = ws.start();
    let idle = anonymous_bytes(server.pid());
    let peak_ingest = watch(server.pid(), &mut || {
        for from in (0..SAMPLES).step_by(BATCH) {
            let reply = post(&server, &batch(from));
            assert_eq!(reply.json()["new"], BATCH);
        }
    });
    server.kill();

    // The tail merged into the sorted file along the way; a restart must still know every uuid.
    let server = ws.start();
    let idle_restarted = anonymous_bytes(server.pid());
    let peak_reingest = watch(server.pid(), &mut || {
        for from in (0..SAMPLES).step_by(BATCH) {
            let reply = post(&server, &batch(from));
            assert_eq!(
                reply.json(),
                json!({"received": BATCH, "new": 0, "deleted": 0})
            );
        }
    });
    let after = anonymous_bytes(server.pid());

    println!(
        "anonymous MiB: idle {:.1}, ingesting {:.1}, restarted {:.1}, re-ingesting {:.1}, after {:.1}",
        idle as f64 / 1048576.0,
        peak_ingest as f64 / 1048576.0,
        idle_restarted as f64 / 1048576.0,
        peak_reingest as f64 / 1048576.0,
        after as f64 / 1048576.0
    );
    for (what, bytes) in [
        ("idle", idle),
        ("ingesting", peak_ingest),
        ("idle after a restart", idle_restarted),
        ("re-ingesting", peak_reingest),
        ("after", after),
    ] {
        assert!(bytes < LIMIT, "{what}: {bytes} bytes of anonymous memory");
    }
    let sorted = fs::metadata(ws.index.join("seen.sorted")).unwrap().len();
    assert!(
        sorted >= 16 * 65_536,
        "the index was merged to disk: {sorted}"
    );
}
