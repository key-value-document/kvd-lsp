//! End-to-end tests: spawn the server binary and speak LSP over stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn bin_path() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("target");
    p.push("debug");
    p.push("kvd-lsp");
    p
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start() -> Self {
        let mut child = Command::new(bin_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn kvd-lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, body: &Value) {
        let raw = serde_json::to_vec(body).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", raw.len()).unwrap();
        self.stdin.write_all(&raw).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_msg(&mut self, timeout: Duration) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        let mut len: Option<usize> = None;
        loop {
            if Instant::now() > deadline {
                return None;
            }
            let mut line = String::new();
            // BufRead blocks; overall test timeout bounds a hang.
            // EOF (0 bytes) means the child died: stop instead of spinning.
            let n = self.reader.read_line(&mut line).ok()?;
            if n == 0 {
                return None;
            }
            if line.trim().is_empty() {
                if len.is_some() {
                    break;
                }
                continue;
            }
            if let Some(v) = line.split_once(':') {
                if v.0.trim().eq_ignore_ascii_case("content-length") {
                    len = v.1.trim().parse().ok();
                }
            }
        }
        let mut buf = vec![0u8; len?];
        self.reader.read_exact(&mut buf).ok()?;
        serde_json::from_slice(&buf).ok()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        self.read_msg(Duration::from_secs(5))
            .unwrap_or_else(|| panic!("no response to {method}"))
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    fn initialize(&mut self) {
        let resp = self.request("initialize", json!({"processId":null,"capabilities":{}}));
        assert!(resp.get("result").is_some(), "initialize failed: {resp}");
        self.notify("initialized", json!({}));
    }

    fn did_open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"kvd","version":1,"text":text}}),
        );
    }

    /// Next `publishDiagnostics` notification, skipping other messages.
    fn next_diagnostics(&mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for diagnostics"
            );
            let msg = self.read_msg(Duration::from_secs(5)).expect("no message");
            if msg.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics")
            {
                return msg;
            }
        }
    }

    fn shutdown(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":"shutdown"}));
        let resp = self
            .read_msg(Duration::from_secs(5))
            .expect("no shutdown response");
        assert_eq!(
            resp.get("result"),
            Some(&Value::Null),
            "shutdown failed: {resp}"
        );
        self.send(&json!({"jsonrpc":"2.0","method":"exit"}));
        // tower-lsp keeps serving after `exit`; don't wait forever.
        // Give it a beat to flush, then reap by force.
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn diags_for(msg: &Value) -> &[Value] {
    msg.pointer("/params/diagnostics")
        .and_then(|d| d.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

#[test]
fn parse_error_produces_diagnostic() {
    let mut s = Session::start();
    s.initialize();
    s.did_open("file:///tmp/lsp-test-bad.kvd", "port: : bad\n");
    let msg = s.next_diagnostics();
    let diags = diags_for(&msg);
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0]
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
    assert_eq!(
        diags[0].pointer("/source").and_then(|v| v.as_str()),
        Some("kvd")
    );
    s.shutdown();
}

#[test]
fn clean_doc_has_no_diagnostics() {
    let mut s = Session::start();
    s.initialize();
    s.did_open("file:///tmp/lsp-test-good.kvd", "port: 8080\nhost: \"x\"\n");
    let msg = s.next_diagnostics();
    assert!(diags_for(&msg).is_empty(), "unexpected diagnostics: {msg}");
    s.shutdown();
}

#[test]
fn embedded_schema_violation_reported() {
    let mut s = Session::start();
    s.initialize();
    s.did_open(
        "file:///tmp/lsp-test-emb.kvd",
        "port: \"nope\"\n__schema__:\n  port:\n    type: int\n",
    );
    let msg = s.next_diagnostics();
    let diags = diags_for(&msg);
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].pointer("/code").and_then(|v| v.as_str()),
        Some("schema-violation")
    );
    s.shutdown();
}

#[test]
fn sibling_schema_violation_reported() {
    let dir = std::env::temp_dir().join("kvd-lsp-test-sib");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.schema.kvd"), "port:\n  type: int\n").unwrap();
    let uri = format!("file://{}/app.kvd", dir.display());
    let mut s = Session::start();
    s.initialize();
    s.did_open(&uri, "port: \"bad\"\n");
    let msg = s.next_diagnostics();
    assert_eq!(
        diags_for(&msg).len(),
        1,
        "expected sibling violation: {msg}"
    );
    s.shutdown();
}

#[test]
fn hover_completion_formatting_definition() {
    let dir = std::env::temp_dir().join("kvd-lsp-test-feats");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.schema.kvd"), "port:\n  type: int\n").unwrap();
    std::fs::write(dir.join("app.kvd"), "port: 8080\n").unwrap();
    let uri = format!("file://{}/app.kvd", dir.display());
    let schema_uri = format!("file://{}/app.schema.kvd", dir.display());

    let mut s = Session::start();
    s.initialize();
    s.did_open(&uri, "port: 8080\n");
    let _ = s.next_diagnostics();

    let hover = s.request(
        "textDocument/hover",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":1}}),
    );
    let contents = hover
        .pointer("/result/contents")
        .cloned()
        .unwrap_or(Value::Null);
    assert!(
        contents.to_string().contains("port"),
        "hover missing key: {hover}"
    );

    let comp = s.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":2}}),
    );
    let labels: Vec<String> = comp
        .pointer("/result")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|i| {
            i.pointer("/label")
                .and_then(|l| l.as_str())
                .map(str::to_string)
        })
        .collect();
    for want in ["int", "port", "true"] {
        assert!(
            labels.iter().any(|l| l == want),
            "completion missing {want}: {labels:?}"
        );
    }

    let fmt = s.request(
        "textDocument/formatting",
        json!({"textDocument":{"uri":uri},"options":{"tabSize":2,"insertSpaces":true}}),
    );
    assert!(
        fmt.pointer("/result").is_some(),
        "no formatting result: {fmt}"
    );

    let def = s.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":1}}),
    );
    assert_eq!(
        def.pointer("/result/uri").and_then(|u| u.as_str()),
        Some(schema_uri.as_str()),
        "goto data->schema failed: {def}"
    );

    s.did_open(&schema_uri, "port:\n  type: int\n");
    let _ = s.next_diagnostics();
    let back = s.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":schema_uri},"position":{"line":0,"character":1}}),
    );
    assert_eq!(
        back.pointer("/result/uri").and_then(|u| u.as_str()),
        Some(uri.as_str()),
        "goto schema->data failed: {back}"
    );

    s.shutdown();
}
