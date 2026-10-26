use kvd_lsp::{
    collect_keys, descriptor_keys_for, descriptor_type_at, find_path_line, is_schema_uri,
    key_before_colon, locate_path, lookup_path, next_segment, parent_path_at, parse_diagnostics,
    path_at_lines, validation_keys_for, word_at,
};
use tower_lsp::lsp_types::{DiagnosticSeverity, Position, Url};

#[test]
fn parse_error_gives_one_diagnostic() {
    let diags = parse_diagnostics("port: : bad\n");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diags[0].range.start.line, 0);
    assert_eq!(diags[0].source.as_deref(), Some("kvd"));
}

#[test]
fn clean_doc_has_no_parse_diagnostics() {
    assert!(parse_diagnostics("port: 8080\nhost: \"x\"\n").is_empty());
}

#[test]
fn path_at_lines_tracks_indent() {
    let lines = ["app:", "  port: 8080", "  host: \"x\""];
    assert_eq!(path_at_lines(&lines, 0), vec!["app"]);
    assert_eq!(path_at_lines(&lines, 1), vec!["app", "port"]);
    assert_eq!(path_at_lines(&lines, 2), vec!["app", "host"]);
}

#[test]
fn path_at_lines_skips_list_markers() {
    let lines = ["tags:", "- \"a\"", "- \"b\""];
    assert_eq!(path_at_lines(&lines, 0), vec!["tags"]);
    assert_eq!(path_at_lines(&lines, 1), vec!["tags"]);
}

#[test]
fn parent_path_at_excludes_current_key() {
    let lines = ["server:", "  host: \"x\"", "  port: 8080"];
    assert_eq!(parent_path_at(&lines, 2), vec!["server"]);
    assert_eq!(parent_path_at(&lines, 1), vec!["server"]);
}

#[test]
fn parent_path_at_handles_list_items() {
    let lines = ["endpoints:", "  - path: \"/health\"", "    method: \"GET\""];
    assert_eq!(parent_path_at(&lines, 2), vec!["endpoints"]);
    assert_eq!(parent_path_at(&lines, 1), vec!["endpoints"]);
}

#[test]
fn next_segment_matches_current_path() {
    let current = vec!["server".to_string()];
    assert_eq!(
        next_segment("server.host", &current).as_deref(),
        Some("host")
    );
    assert_eq!(next_segment("server", &current), None);
    assert_eq!(next_segment("other.host", &current), None);
}

#[test]
fn word_at_finds_key() {
    let w = word_at(
        "port: 8080\n",
        Position {
            line: 0,
            character: 1,
        },
    );
    assert_eq!(w.as_deref(), Some("port"));
}

#[test]
fn word_at_returns_none_on_punctuation() {
    assert!(
        word_at(
            "port: 8080\n",
            Position {
                line: 0,
                character: 4
            }
        )
        .is_none()
    );
}

#[test]
fn locate_path_finds_key_range() {
    let text = "port: 8080\nhost: \"x\"\n";
    let r = locate_path(text, "host");
    assert_eq!(
        r.start,
        Position {
            line: 1,
            character: 0
        }
    );
    assert_eq!(
        r.end,
        Position {
            line: 1,
            character: 4
        }
    );
}

#[test]
fn collect_keys_lists_all_keys() {
    let doc = kvd_rs::deserialize::from_str("port: 8080\nhost: \"x\"\n").unwrap();
    let mut keys = Vec::new();
    collect_keys(&doc, "", &mut keys);
    assert!(keys.contains(&"port".to_string()));
    assert!(keys.contains(&"host".to_string()));
}

#[test]
fn lookup_path_descends_maps() {
    let doc = kvd_rs::deserialize::from_str("app:\n  port: 8080\n").unwrap();
    let hit = lookup_path(&doc, &["app".to_string(), "port".to_string()]);
    assert!(hit.is_some());
    assert!(lookup_path(&doc, &["nope".to_string()]).is_none());
}

#[test]
fn find_path_line_matches_indent_path() {
    let text = "app:\n  port: 8080\n";
    assert_eq!(
        find_path_line(text, &["app".to_string(), "port".to_string()]),
        Some(1)
    );
    assert_eq!(find_path_line(text, &["missing".to_string()]), None);
}

#[test]
fn key_before_colon_rejects_bare_values() {
    assert!(key_before_colon("port").is_none());
    assert_eq!(key_before_colon("port: 8080").as_deref(), Some("port"));
}

#[test]
fn schema_uri_detection() {
    let schema = Url::parse("file:///tmp/app.schema.kvd").unwrap();
    let data = Url::parse("file:///tmp/app.kvd").unwrap();
    assert!(is_schema_uri(&schema));
    assert!(!is_schema_uri(&data));
}

#[test]
fn path_at_lines_tracks_validation_block() {
    let lines = ["port:", "  type: int", "  validation:", "    min: 0"];
    assert_eq!(path_at_lines(&lines, 3), vec!["port", "validation", "min"]);
}

fn key_names(keys: &[(&'static str, &'static str)]) -> Vec<&'static str> {
    keys.iter().map(|(name, _)| *name).collect()
}

#[test]
fn descriptor_keys_are_type_aware() {
    assert!(key_names(&descriptor_keys_for(None)).contains(&"element"));
    assert!(!key_names(&descriptor_keys_for(Some("int"))).contains(&"element"));
    assert!(!key_names(&descriptor_keys_for(Some("str"))).contains(&"element"));
    assert!(key_names(&descriptor_keys_for(Some("list"))).contains(&"element"));
    assert!(key_names(&descriptor_keys_for(Some("dict"))).contains(&"element"));
}

#[test]
fn validation_keys_are_type_aware() {
    assert_eq!(
        key_names(&validation_keys_for(Some("int"))),
        vec!["min", "max", "exclusive_min", "exclusive_max"]
    );
    assert_eq!(
        key_names(&validation_keys_for(Some("float"))),
        vec!["min", "max", "exclusive_min", "exclusive_max"]
    );
    assert_eq!(
        key_names(&validation_keys_for(Some("str"))),
        vec!["min_len", "max_len", "pattern"]
    );
    assert_eq!(
        key_names(&validation_keys_for(Some("bytes"))),
        vec!["min_len", "max_len"]
    );
    assert_eq!(
        key_names(&validation_keys_for(Some("list"))),
        vec!["min_len", "max_len"]
    );
    assert_eq!(
        key_names(&validation_keys_for(Some("dict"))),
        vec!["min_len", "max_len"]
    );
    assert!(validation_keys_for(Some("bool")).is_empty());
    assert_eq!(validation_keys_for(None).len(), 7);
}

#[test]
fn descriptor_type_at_reads_type() {
    let text = "port:\n  type: int\n";
    assert_eq!(
        descriptor_type_at(text, &["port".to_string()]).as_deref(),
        Some("int")
    );
    assert_eq!(descriptor_type_at(text, &[]), None);
}

#[test]
fn descriptor_type_at_reads_nested_descriptor() {
    let text = "endpoints:\n  type: list\n  element:\n    type: str\n";
    assert_eq!(
        descriptor_type_at(text, &["endpoints".to_string(), "element".to_string()]).as_deref(),
        Some("str")
    );
}

#[test]
fn descriptor_type_at_handles_schema_list() {
    let text = "endpoints:\n  - type: dict\n";
    assert_eq!(
        descriptor_type_at(text, &["endpoints".to_string()]).as_deref(),
        Some("dict")
    );
}

#[test]
fn descriptor_type_at_ignores_incomplete_blocks() {
    let text = "blob:\n  type: bytes\n  validation:\n    ";
    assert_eq!(
        descriptor_type_at(text, &["blob".to_string()]).as_deref(),
        Some("bytes")
    );
}

#[test]
fn descriptor_type_at_reads_dotted_descriptor() {
    let text = "port.type: int\nport.validation:\n  min: 0\n";
    assert_eq!(
        descriptor_type_at(text, &["port".to_string()]).as_deref(),
        Some("int")
    );
}
