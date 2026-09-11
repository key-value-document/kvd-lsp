use kvd_lsp::{
    collect_keys, find_path_line, is_schema_uri, key_before_colon, locate_path, lookup_path,
    parse_diagnostics, path_at_lines, word_at,
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
    assert_eq!(
        path_at_lines(&lines, 3),
        vec!["port", "validation", "min"]
    );
}
