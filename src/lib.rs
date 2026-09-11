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
        "required: bare type name (int, float, bool, str, dict, list)",
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
    ("min_len", "str/list/dict: length >= min_len"),
    ("max_len", "str/list/dict: length <= max_len"),
    ("pattern", "str: full-match regex"),
];

/// True for schema files (`*.schema.kvd`), which hold type names and
/// descriptor blocks instead of data values.
pub fn is_schema_uri(uri: &Url) -> bool {
    uri.path().ends_with(".schema.kvd")
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
        let Some(head) = key_before_colon(content) else {
            continue;
        };
        let Some(keys) = split_key_head(&head) else {
            continue;
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
    stack.into_iter().map(|(_, k)| k).collect()
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
    let mut c = content;
    if c.starts_with("- ") || c == "-" {
        c = c.strip_prefix("- ").unwrap_or("");
    }
    if c.starts_with("= ") || c == "=" {
        c = c.strip_prefix("= ").unwrap_or("");
    }
    Some(c)
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

        let mut items = Vec::new();
        let last_word = before.rsplit([':', ' ']).next().unwrap_or("");
        if last_word == "type"
            || last_word == "element"
            || before.contains("type:")
            || before.contains("element:")
        {
            for (name, doc) in BUILTINS {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::TYPE_PARAMETER),
                    detail: Some(doc.to_string()),
                    ..Default::default()
                });
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

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
        // Inside a schema file, descriptor and validation keys come first.
        // A `validation` block holds constraint keys; anywhere else in a
        // descriptor, the block keys apply.
        if is_schema_uri(uri) {
            let lines: Vec<&str> = text.lines().collect();
            let idx = (pos.line as usize).min(lines.len().saturating_sub(1));
            let path = path_at_lines(&lines, idx);
            let in_validation = path.last().is_some_and(|s| s == "validation")
                || path.iter().rev().nth(1).is_some_and(|s| s == "validation");
            let table = if in_validation {
                VALIDATION_KEYS
            } else {
                DESCRIPTOR_KEYS
            };
            for (name, doc) in table {
                push(name, CompletionItemKind::FIELD, doc);
            }
        }
        for (name, doc) in BUILTINS {
            push(name, CompletionItemKind::TYPE_PARAMETER, doc);
        }
        for (kw, doc) in SCALAR_KEYWORDS {
            push(kw, CompletionItemKind::KEYWORD, doc);
        }
        if let Ok(doc) = kvd_rs::deserialize::from_str(&text) {
            let mut keys = Vec::new();
            collect_keys(&doc, "", &mut keys);
            for k in &keys {
                if let Some(leaf) = k.split('.').next_back() {
                    push(leaf, CompletionItemKind::FIELD, k);
                }
            }
        }
        if let Some(schema_text) = sibling_schema_text(uri).await {
            if let Ok(schema) = kvd_rs::deserialize::from_str(&schema_text) {
                let mut skeys = Vec::new();
                collect_keys(&schema, "", &mut skeys);
                for k in &skeys {
                    if let Some(leaf) = k.split('.').next_back() {
                        push(leaf, CompletionItemKind::FIELD, k);
                    }
                }
            }
        }
        // In a schema file, mirror the sibling data keys so node names
        // complete the same way they do from the data side.
        if is_schema_uri(uri) {
            if let Some(data_uri) = sibling_data_uri(uri).await {
                let data_text = self.get_text(&data_uri).await.or_else(|| {
                    data_uri
                        .to_file_path()
                        .ok()
                        .and_then(|p| std::fs::read_to_string(p).ok())
                });
                if let Some(data_text) = data_text {
                    if let Ok(data) = kvd_rs::deserialize::from_str(&data_text) {
                        let mut dkeys = Vec::new();
                        collect_keys(&data, "", &mut dkeys);
                        for k in &dkeys {
                            if let Some(leaf) = k.split('.').next_back() {
                                push(leaf, CompletionItemKind::FIELD, k);
                            }
                        }
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
        // In schema files, explain descriptor and validation keys.
        if is_schema_uri(uri) {
            if let Some((_, doc)) = DESCRIPTOR_KEYS
                .iter()
                .chain(VALIDATION_KEYS.iter())
                .find(|(n, _)| *n == word)
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
        let lines: Vec<&str> = text.lines().collect();
        let path = path_at_lines(
            &lines,
            (pos.line as usize).min(lines.len().saturating_sub(1)),
        );
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
