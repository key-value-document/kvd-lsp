//! kvd-lsp: language server for KVD documents.
//!
//! Diagnostics come from [`kvd_rs::deserialize`]; schema checks from
//! [`kvd_rs::schema`]; formatting round-trips through
//! [`kvd_rs::serialize`]. Schema lookup is by convention: a data file
//! `app.kvd` is checked against the sibling `app.schema.kvd` file when it
//! exists. Schemas are never embedded in data documents.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

/// Builtin type names with one-line help for completion and hover.
const BUILTINS: &[(&str, &str)] = &[
    ("int", "integer literal, e.g. 8080 or 1_000"),
    ("float", "floating-point literal, e.g. 0.75 or 1e3"),
    ("bool", "the bare literals true / false"),
    ("str", "quoted string, or \"\"\" block for multi-line text"),
    (
        "bytes",
        "base64 text; schema-only type with no literal of its own",
    ),
    (
        "list",
        "dash-marker sequence; element type via a one-item list",
    ),
    ("dict", "equals-marker mapping with opaque quoted keys"),
];

const SCALAR_KEYWORDS: &[(&str, &str)] = &[
    ("true", "bool literal"),
    ("false", "bool literal"),
    ("null", "absent value; allowed only under optional: true"),
];

/// Descriptor-block keys for schema files (spec §10). A schema leaf block
/// is a descriptor iff it contains a `type` key.
const DESCRIPTOR_KEYS: &[(&str, &str)] = &[
    (
        "type",
        "required: bare type name (int, float, bool, str, bytes, dict, list)",
    ),
    ("optional", "optional: true allows absence or null"),
    ("description", "ignored documentation string for the field"),
    (
        "deprecated",
        "ignored block: reason/since strings marking the field deprecated",
    ),
    (
        "element",
        "item type for type: list (required), value type for type: dict (optional)",
    ),
    (
        "validation",
        "optional block of constraint keys (min, max, pattern, ...)",
    ),
];

/// Constraint keys for a `validation` block (spec §10).
const VALIDATION_KEYS: &[(&str, &str)] = &[
    ("min", "int/float: value >= min"),
    ("max", "int/float: value <= max"),
    ("exclusive_min", "int/float: value > exclusive_min"),
    ("exclusive_max", "int/float: value < exclusive_max"),
    ("min_len", "str/bytes/list/dict: length >= min_len"),
    ("max_len", "str/bytes/list/dict: length <= max_len"),
    ("pattern", "str: full-match regex"),
];

/// Keys inside a `deprecated` block (spec §10).
const DEPRECATED_KEYS: &[(&str, &str)] = &[
    ("reason", "deprecated: human-readable reason (string)"),
    (
        "since",
        "deprecated: version or date deprecation started (string)",
    ),
];

/// True for schema files (`*.schema.kvd`), which hold type names and
/// descriptor blocks instead of data values.
pub fn is_schema_uri(uri: &Url) -> bool {
    uri.path().ends_with(".schema.kvd")
}

/// True when the cursor sits inside a comment or an unterminated string,
/// where neither keys nor values should be suggested.
fn in_comment_or_string(before: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in before.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if q == '"' && c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else if c == '#' {
            return true;
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        }
    }
    quote.is_some()
}

#[derive(Debug)]
struct Backend {
    client: Client,
    docs: Arc<RwLock<HashMap<Url, String>>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            docs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_text(&self, uri: &Url) -> Option<String> {
        self.docs.read().await.get(uri).cloned()
    }

    async fn diagnose(&self, uri: Url, version: Option<i32>) {
        let text = self.get_text(&uri).await.unwrap_or_default();
        let mut diags = parse_diagnostics(&text);
        if diags.is_empty() {
            diags.extend(verify_diagnostics(&text, &uri).await);
        }
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}

/// Parse failure as a single error diagnostic.
pub fn parse_diagnostics(text: &str) -> Vec<Diagnostic> {
    match kvd_rs::deserialize::from_str(text) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let line = e.line.saturating_sub(1) as u32;
            let start = e.col.saturating_sub(1) as u32;
            let line_len = text
                .lines()
                .nth(line as usize)
                .map_or(0, |l| l.len() as u32);
            let end = (start + 1).min(line_len.max(start));
            vec![Diagnostic {
                range: Range {
                    start: Position {
                        line,
                        character: start.min(line_len),
                    },
                    end: Position {
                        line,
                        character: end,
                    },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String(e.kind.as_str().to_string())),
                source: Some("kvd".to_string()),
                message: format!("{}: {}", e.kind.as_str(), e.message),
                ..Default::default()
            }]
        }
    }
}

/// Schema diagnostics: the sibling `<name>.schema.kvd` file when it
/// exists. Schemas are never embedded in data documents (spec §2).
async fn verify_diagnostics(text: &str, uri: &Url) -> Vec<Diagnostic> {
    if kvd_rs::deserialize::from_str(text).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(schema_text) = sibling_schema_text(uri).await {
        match kvd_rs::schema::verify_from_str(text, &schema_text) {
            Ok(()) => {}
            Err(kvd_rs::schema::VerifyError::Violations(vs))
            | Err(kvd_rs::schema::VerifyError::SchemaMalformed(vs)) => {
                for v in &vs {
                    out.push(violation_diagnostic(text, v));
                }
            }
            Err(kvd_rs::schema::VerifyError::ParseSchema(e)) => out.push(Diagnostic {
                range: zero_range(),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String("bad-schema".to_string())),
                source: Some("kvd".to_string()),
                message: format!("sibling schema does not parse: {e}"),
                ..Default::default()
            }),
            Err(_) => {}
        }
    }
    out
}

fn violation_diagnostic(text: &str, v: &kvd_rs::schema::Violation) -> Diagnostic {
    Diagnostic {
        range: locate_path(text, &v.path),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("schema-violation".to_string())),
        source: Some("kvd".to_string()),
        message: v.to_string(),
        ..Default::default()
    }
}

fn zero_range() -> Range {
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: 0,
        },
    }
}

/// Map a dotted violation path (`app.port`, `endpoints[0].method`) to the
/// range of the key on the matching source line.
pub fn locate_path(text: &str, path: &str) -> Range {
    let norm: Vec<String> = path
        .split('.')
        .map(|seg| seg.split('[').next().unwrap_or(seg).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if norm.is_empty() {
        return zero_range();
    }
    let lines: Vec<&str> = text.lines().collect();
    for (i, _) in lines.iter().enumerate() {
        if path_at_lines(&lines, i) == norm {
            return key_range(lines[i], i as u32);
        }
    }
    let last = norm.last().cloned().unwrap_or_default();
    for (i, line) in lines.iter().enumerate() {
        if line_key(line).as_deref() == Some(last.as_str()) {
            return key_range(line, i as u32);
        }
    }
    zero_range()
}

/// Dotted key path of the entry on `line_idx`, via the indent stack.
/// List markers (`- `) and dict markers (`= `) count as one extra level
/// and contribute no segment of their own.
pub fn path_at_lines(lines: &[&str], line_idx: usize) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in lines.iter().take(line_idx + 1) {
        push_line(&mut stack, line);
    }
    stack.into_iter().map(|(_, k)| k).collect()
}

/// Path of the node containing the entry on `line_idx`, excluding that
/// entry's own key. Used to complete keys at the current position.
pub fn parent_path_at(lines: &[&str], line_idx: usize) -> Vec<String> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in lines.iter().take(line_idx) {
        push_line(&mut stack, line);
    }
    let current = lines.get(line_idx).copied().unwrap_or("");
    let eff = strip_entry_markers(current).1;
    while stack.last().is_some_and(|(d, _)| *d >= eff) {
        stack.pop();
    }
    stack.into_iter().map(|(_, k)| k).collect()
}

/// Line content without leading `- ` / `= ` markers, plus the effective
/// indent (each marker counts as one extra level).
fn strip_entry_markers(line: &str) -> (&str, usize) {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let mut content = line.trim();
    let mut eff = indent;
    if content.starts_with("- ") || content == "-" {
        content = content.strip_prefix("- ").unwrap_or("");
        eff += 2;
    }
    if content.starts_with("= ") || content == "=" {
        content = content.strip_prefix("= ").unwrap_or("");
        eff += 2;
    }
    (content, eff)
}

/// Push the entry on `line` onto `stack`; ignore non-key lines.
fn push_line(stack: &mut Vec<(usize, String)>, line: &str) {
    let (content, eff) = strip_entry_markers(line);
    let Some(head) = key_before_colon(content) else {
        return;
    };
    let Some(keys) = split_key_head(&head) else {
        return;
    };
    while stack.last().is_some_and(|(d, _)| *d >= eff) {
        stack.pop();
    }
    // Dict keys are always quoted single segments, so split_key_head
    // yields the opaque key unsplit; dotted node paths expand here.
    for key in keys {
        stack.push((eff, key));
    }
}

/// Raw key head before the first `:` outside quotes (`a.b.c`, `"a.b"`).
/// Dotted heads are accepted segment-wise since dots are path separators,
/// not key characters (spec §2). There is no reserved namespace.
pub fn key_before_colon(content: &str) -> Option<String> {
    let head = head_before_colon(content)?;
    split_key_head(head)?; // validate only; keep raw spelling for ranges
    Some(head.to_string())
}

/// Text before the first `:` that is not inside quotes.
fn head_before_colon(content: &str) -> Option<&str> {
    let mut in_quote: Option<char> = None;
    for (i, c) in content.char_indices() {
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
        } else if c == '"' || c == '\'' {
            in_quote = Some(c);
        } else if c == ':' {
            let head = content[..i].trim();
            return if head.is_empty() { None } else { Some(head) };
        }
    }
    None
}

/// Split a key head on `.` separators outside quotes, unquoting quoted
/// segments. Each bare segment must be a valid key.
fn split_key_head(head: &str) -> Option<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut in_quote: Option<char> = None;
    let mut quoted = false;
    for c in head.chars() {
        if let Some(q) = in_quote {
            cur.push(c);
            if c == q {
                in_quote = None;
            }
        } else if c == '"' || c == '\'' {
            if cur.is_empty() {
                quoted = true;
            }
            in_quote = Some(c);
            cur.push(c);
        } else if c == '.' {
            if cur.is_empty() {
                return None;
            }
            segs.push(finish_segment(&cur, quoted)?);
            cur.clear();
            quoted = false;
        } else {
            cur.push(c);
        }
    }
    if in_quote.is_some() || cur.is_empty() {
        return None;
    }
    segs.push(finish_segment(&cur, quoted)?);
    Some(segs)
}

/// Unquote a quoted segment, or validate a bare one.
fn finish_segment(raw: &str, quoted: bool) -> Option<String> {
    if quoted {
        let b = raw.as_bytes();
        if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
            Some(raw[1..raw.len() - 1].to_string())
        } else {
            None
        }
    } else if kvd_rs::grammar::is_key(raw) {
        Some(raw.to_string())
    } else {
        None
    }
}

/// Bare key on a line, ignoring any list or dict marker.
fn line_key(line: &str) -> Option<String> {
    strip_markers(line.trim()).and_then(key_before_colon)
}

/// Line content with any leading `- ` / `= ` markers removed.
fn strip_markers(content: &str) -> Option<&str> {
    Some(strip_entry_markers(content).0)
}

/// Range covering the key text on a source line.
fn key_range(line: &str, line_no: u32) -> Range {
    let trimmed = line.trim_start();
    let mut start = (line.len() - trimmed.len()) as u32;
    let mut rest = trimmed;
    for marker in ["- ", "= "] {
        if rest.starts_with(marker) {
            rest = &rest[2..];
            start += 2;
        }
    }
    let len = line_key(line).map_or(0, |k| k.len() as u32);
    Range {
        start: Position {
            line: line_no,
            character: start,
        },
        end: Position {
            line: line_no,
            character: start + len,
        },
    }
}

/// Word (key, type name, keyword) under the cursor.
pub fn word_at(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let mut i = (pos.character as usize).min(chars.len());
    if i > 0 && i == chars.len() {
        i -= 1;
    }
    if chars.get(i).is_none_or(|c| !is_word_char(*c)) {
        return None;
    }
    let mut s = i;
    while s > 0 && is_word_char(chars[s - 1]) {
        s -= 1;
    }
    let mut e = i;
    while e < chars.len() && is_word_char(chars[e]) {
        e += 1;
    }
    Some(chars[s..e].iter().collect())
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Sibling `<name>.schema.kvd` for a data file, when it exists on disk.
async fn sibling_schema_text(uri: &Url) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".schema.kvd") {
        return None;
    }
    if !name.ends_with(".kvd") {
        return None;
    }
    let schema_path = path.with_file_name(name.replace(".kvd", ".schema.kvd"));
    tokio::fs::read_to_string(schema_path).await.ok()
}

/// Sibling data file `<name>.kvd` for a schema file.
async fn sibling_data_uri(uri: &Url) -> Option<Url> {
    let path = uri.to_file_path().ok()?;
    let name = path.file_name()?.to_str()?;
    if !name.ends_with(".schema.kvd") {
        return None;
    }
    let data_path = path.with_file_name(name.replace(".schema.kvd", ".kvd"));
    Url::from_file_path(data_path).ok()
}

/// All dotted key paths in a node tree.
pub fn collect_keys(node: &kvd_rs::value::Node, prefix: &str, out: &mut Vec<String>) {
    match node {
        kvd_rs::value::Node::Map(m) | kvd_rs::value::Node::Dict(m) => {
            for (k, v) in m.iter() {
                let full = if prefix.is_empty() {
                    k.to_string()
                } else {
                    format!("{prefix}.{k}")
                };
                out.push(full.clone());
                collect_keys(v, &full, out);
            }
        }
        kvd_rs::value::Node::List(items) => {
            for item in items {
                collect_keys(item, prefix, out);
            }
        }
        kvd_rs::value::Node::Scalar(_) => {}
        _ => {}
    }
}

/// Next path segment after `current`, or `None` when `full` is not under it.
pub fn next_segment(full: &str, current: &[String]) -> Option<String> {
    let segs: Vec<&str> = full.split('.').collect();
    if segs.len() <= current.len() {
        return None;
    }
    if segs[..current.len()]
        .iter()
        .zip(current)
        .all(|(a, b)| *a == b)
    {
        Some(segs[current.len()].to_string())
    } else {
        None
    }
}

/// Navigate a parsed document along a dotted path, descending into the
/// first list item when a list is met (schema element position).
pub fn lookup_path<'a>(
    doc: &'a kvd_rs::value::Node,
    path: &[String],
) -> Option<&'a kvd_rs::value::Node> {
    let mut cur = doc;
    for seg in path {
        match cur {
            kvd_rs::value::Node::Map(m) | kvd_rs::value::Node::Dict(m) => cur = m.get(seg)?,
            kvd_rs::value::Node::List(items) => {
                cur = items.first()?;
                match cur {
                    kvd_rs::value::Node::Map(m) | kvd_rs::value::Node::Dict(m) => {
                        cur = m.get(seg)?
                    }
                    _ => return None,
                }
            }
            kvd_rs::value::Node::Scalar(_) => return None,
            _ => return None,
        }
    }
    Some(cur)
}

/// Descriptor keys valid for a descriptor of `type_name` (spec §10).
/// `element` is only accepted on `list`/`dict` descriptors. When the type
/// is unknown (absent or unparseable), every key is offered.
pub fn descriptor_keys_for(type_name: Option<&str>) -> Vec<(&'static str, &'static str)> {
    DESCRIPTOR_KEYS
        .iter()
        .copied()
        .filter(|(name, _)| {
            *name != "element"
                || type_name.is_none()
                || matches!(type_name, Some("list") | Some("dict"))
        })
        .collect()
}

/// Constraint keys valid for `type_name` in a `validation` block (spec §10).
/// An unknown type offers every constraint.
pub fn validation_keys_for(type_name: Option<&str>) -> Vec<(&'static str, &'static str)> {
    VALIDATION_KEYS
        .iter()
        .copied()
        .filter(|(name, _)| match type_name {
            Some("int") | Some("float") => {
                matches!(*name, "min" | "max" | "exclusive_min" | "exclusive_max")
            }
            Some("str") => matches!(*name, "min_len" | "max_len" | "pattern"),
            Some("bytes") => matches!(*name, "min_len" | "max_len"),
            Some("list") | Some("dict") => matches!(*name, "min_len" | "max_len"),
            Some("bool") => false,
            _ => true,
        })
        .collect()
}

/// Parse the document for key completion. When the line under the cursor
/// makes the document invalid, drop that line and retry, then fall back to
/// the prefix before it, so a partially typed entry does not hide keys.
fn parse_for_completion(text: &str, line_idx: usize) -> Option<kvd_rs::value::Node> {
    if let Ok(doc) = kvd_rs::deserialize::from_str(text) {
        return Some(doc);
    }
    let lines: Vec<&str> = text.lines().collect();
    let without: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != line_idx)
        .map(|(_, line)| *line)
        .collect();
    if let Ok(doc) = kvd_rs::deserialize::from_str(&without.join("\n")) {
        return Some(doc);
    }
    let prefix = lines.iter().take(line_idx).copied().collect::<Vec<_>>();
    kvd_rs::deserialize::from_str(&prefix.join("\n")).ok()
}

/// Type name declared by the descriptor at `path`, if it is a descriptor.
/// Reads the `type` entry from the source lines, so an incomplete sibling
/// block (for example an empty `validation:`) does not hide it.
pub fn descriptor_type_at(text: &str, path: &[String]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    descriptor_type_at_lines(&lines, path)
}

fn descriptor_type_at_lines(lines: &[&str], path: &[String]) -> Option<String> {
    let mut type_path = path.to_vec();
    type_path.push("type".to_string());
    for (i, line) in lines.iter().enumerate() {
        if path_at_lines(lines, i) != type_path {
            continue;
        }
        let (content, _) = strip_entry_markers(line);
        let Some((_, value)) = content.split_once(':') else {
            continue;
        };
        let value = value.split('#').next().unwrap_or("").trim();
        if !value.is_empty() {
            return Some(value.trim_matches(['"', '\'']).to_string());
        }
    }
    None
}

/// True when the node at `path` is a descriptor: it declares a type, or its
/// existing children are all descriptor keys (or it is empty).
fn is_descriptor_node(lines: &[&str], path: &[String]) -> bool {
    if descriptor_type_at_lines(lines, path).is_some() {
        return true;
    }
    for (i, _) in lines.iter().enumerate() {
        let p = path_at_lines(lines, i);
        if p.len() != path.len() + 1 || !p.starts_with(path) {
            continue;
        }
        let child = &p[path.len()];
        if !DESCRIPTOR_KEYS.iter().any(|(name, _)| name == child) {
            return false;
        }
    }
    true
}

fn node_help(node: &kvd_rs::value::Node) -> String {
    match node {
        kvd_rs::value::Node::Scalar(s) => format!("{}: {}", s.shape, s.text),
        kvd_rs::value::Node::Map(m) => format!("map with {} keys", m.len()),
        kvd_rs::value::Node::Dict(m) => format!("dict with {} keys", m.len()),
        kvd_rs::value::Node::List(l) => format!("list with {} items", l.len()),
        _ => "value".to_string(),
    }
}

/// Find the source line whose indent path equals `path`.
pub fn find_path_line(text: &str, path: &[String]) -> Option<u32> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, _) in lines.iter().enumerate() {
        if path_at_lines(&lines, i) == path {
            return Some(i as u32);
        }
    }
    None
}

fn end_position(text: &str) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, l) in text.lines().enumerate() {
        line = i as u32;
        character = l.chars().count() as u32;
    }
    Position { line, character }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ":".to_string(),
                        " ".to_string(),
                        "-".to_string(),
                    ]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.docs
            .write()
            .await
            .insert(params.text_document.uri.clone(), params.text_document.text);
        self.diagnose(params.text_document.uri, None).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.docs
                .write()
                .await
                .insert(params.text_document.uri.clone(), change.text);
            self.diagnose(params.text_document.uri, Some(params.text_document.version))
                .await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.diagnose(params.text_document.uri, None).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.write().await.remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let text = self.get_text(uri).await.unwrap_or_default();
        let line_text = text.lines().nth(pos.line as usize).unwrap_or("");
        let before: String = line_text.chars().take(pos.character as usize).collect();
        let schema = is_schema_uri(uri);

        if in_comment_or_string(&before) {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        }

        let last_key = key_before_colon(strip_entry_markers(&before).0)
            .and_then(|head| split_key_head(&head))
            .and_then(|segs| segs.last().cloned());
        let in_value = last_key.is_some();

        if schema && matches!(last_key.as_deref(), Some("type") | Some("element")) {
            let items = BUILTINS
                .iter()
                .map(|(name, doc)| CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::TYPE_PARAMETER),
                    detail: Some(doc.to_string()),
                    ..Default::default()
                })
                .collect();
            return Ok(Some(CompletionResponse::Array(items)));
        }

        let mut items = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |label: &str, kind, detail: &str| {
            if seen.insert(label.to_string()) {
                items.push(CompletionItem {
                    label: label.to_string(),
                    kind: Some(kind),
                    detail: Some(detail.to_string()),
                    ..Default::default()
                });
            }
        };

        if in_value {
            if schema {
                match last_key.as_deref() {
                    Some("optional") => {
                        push("true", CompletionItemKind::VALUE, "optional field");
                        push("false", CompletionItemKind::VALUE, "required field");
                    }
                    Some(
                        "description" | "reason" | "since" | "pattern" | "validation"
                        | "deprecated" | "min" | "max" | "exclusive_min" | "exclusive_max"
                        | "min_len" | "max_len",
                    ) => {}
                    _ => {
                        for (name, doc) in BUILTINS {
                            push(name, CompletionItemKind::TYPE_PARAMETER, doc);
                        }
                    }
                }
            } else {
                for (kw, doc) in SCALAR_KEYWORDS {
                    push(kw, CompletionItemKind::KEYWORD, doc);
                }
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

        let trimmed = line_text.trim_start();
        let after_list = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        if after_list.starts_with("= ") || after_list == "=" {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        }

        let lines: Vec<&str> = text.lines().collect();
        let parent = parent_path_at(&lines, pos.line as usize);

        if schema && !parent.is_empty() {
            let last = parent.last().map(String::as_str);
            if last == Some("validation") {
                let descriptor_path = &parent[..parent.len() - 1];
                let type_name = descriptor_type_at_lines(&lines, descriptor_path);
                for (name, doc) in validation_keys_for(type_name.as_deref()) {
                    push(name, CompletionItemKind::FIELD, doc);
                }
            } else if last == Some("deprecated") {
                for (name, doc) in DEPRECATED_KEYS {
                    push(name, CompletionItemKind::FIELD, doc);
                }
            } else if is_descriptor_node(&lines, &parent) {
                let type_name = descriptor_type_at_lines(&lines, &parent);
                for (name, doc) in descriptor_keys_for(type_name.as_deref()) {
                    push(name, CompletionItemKind::FIELD, doc);
                }
            }
        }

        let mut push_keys = |keys: &[String]| {
            for key in keys {
                if let Some(seg) = next_segment(key, &parent) {
                    push(&seg, CompletionItemKind::FIELD, key);
                }
            }
        };

        if let Some(doc) = parse_for_completion(&text, pos.line as usize) {
            let mut keys = Vec::new();
            collect_keys(&doc, "", &mut keys);
            push_keys(&keys);
        }
        if let Some(schema_text) = sibling_schema_text(uri).await {
            if let Ok(schema_doc) = kvd_rs::deserialize::from_str(&schema_text) {
                let mut keys = Vec::new();
                collect_keys(&schema_doc, "", &mut keys);
                push_keys(&keys);
            }
        }
        if schema {
            if let Some(data_uri) = sibling_data_uri(uri).await {
                let data_text = self.get_text(&data_uri).await.or_else(|| {
                    data_uri
                        .to_file_path()
                        .ok()
                        .and_then(|p| std::fs::read_to_string(p).ok())
                });
                if let Some(data_text) = data_text {
                    if let Ok(data) = kvd_rs::deserialize::from_str(&data_text) {
                        let mut keys = Vec::new();
                        collect_keys(&data, "", &mut keys);
                        push_keys(&keys);
                    }
                }
            }
        }
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let text = self.get_text(uri).await.unwrap_or_default();
        let Some(word) = word_at(&text, pos) else {
            return Ok(None);
        };
        if let Some((_, doc)) = BUILTINS.iter().find(|(n, _)| *n == word) {
            return Ok(Some(Hover {
                contents: HoverContents::Scalar(MarkedString::String(format!(
                    "type `{word}`: {doc}"
                ))),
                range: None,
            }));
        }
        let lines: Vec<&str> = text.lines().collect();
        let path = path_at_lines(
            &lines,
            (pos.line as usize).min(lines.len().saturating_sub(1)),
        );
        // In schema files, explain descriptor, validation, and deprecated
        // block keys. `reason`/`since` are common data keys, so only treat
        // them as schema keys inside a `deprecated` block.
        if is_schema_uri(uri) {
            let deprecated_doc = if path.iter().any(|s| s == "deprecated") {
                DEPRECATED_KEYS.iter().find(|(n, _)| *n == word)
            } else {
                None
            };
            if let Some((_, doc)) = DESCRIPTOR_KEYS
                .iter()
                .chain(VALIDATION_KEYS.iter())
                .find(|(n, _)| *n == word)
                .or(deprecated_doc)
            {
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(format!(
                        "schema key `{word}`: {doc}"
                    ))),
                    range: None,
                }));
            }
        }
        let Ok(doc) = kvd_rs::deserialize::from_str(&text) else {
            return Ok(None);
        };
        let mut info = lookup_path(&doc, &path).map(node_help);
        if info.is_none() {
            let short = path.last().cloned().unwrap_or_else(|| word.clone());
            info = doc
                .as_map()
                .and_then(|m| m.get(short.as_str()))
                .map(node_help);
        }
        match info {
            Some(i) => {
                let show = path.last().unwrap_or(&word);
                Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(format!("`{show}`: {i}"))),
                    range: None,
                }))
            }
            None => Ok(None),
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let text = self
            .get_text(&params.text_document.uri)
            .await
            .unwrap_or_default();
        let Ok(doc) = kvd_rs::deserialize::from_str(&text) else {
            return Ok(None);
        };
        let Ok(pretty) = kvd_rs::serialize::to_string(&doc) else {
            return Ok(None);
        };
        if pretty == text {
            return Ok(Some(Vec::new()));
        }
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: end_position(&text),
            },
            new_text: pretty,
        }]))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let text = self.get_text(uri).await.unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return Ok(None);
        }
        let path = path_at_lines(&lines, (pos.line as usize).min(lines.len() - 1));
        if path.is_empty() {
            return Ok(None);
        }
        let name = uri.path();
        if name.ends_with(".schema.kvd") {
            if let Some(data_uri) = sibling_data_uri(uri).await {
                let data_text = self
                    .get_text(&data_uri)
                    .await
                    .or_else(|| {
                        data_uri
                            .to_file_path()
                            .ok()
                            .and_then(|p| std::fs::read_to_string(p).ok())
                    })
                    .unwrap_or_default();
                if let Some(line) = find_path_line(&data_text, &path) {
                    let range = key_range(data_text.lines().nth(line as usize).unwrap_or(""), line);
                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: data_uri,
                        range,
                    })));
                }
            }
            return Ok(None);
        }
        if let Some(schema_text) = sibling_schema_text(uri).await {
            if let Some(line) = find_path_line(&schema_text, &path) {
                let range = key_range(schema_text.lines().nth(line as usize).unwrap_or(""), line);
                let schema_uri = Url::parse(&uri.to_string().replace(".kvd", ".schema.kvd"))
                    .unwrap_or_else(|_| uri.clone());
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: schema_uri,
                    range,
                })));
            }
        }
        Ok(None)
    }
}

/// Serve the KVD language server over stdio.
pub async fn run() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
