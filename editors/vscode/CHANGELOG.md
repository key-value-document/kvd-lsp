# Changelog

## 0.3.2

- Highlight keys that follow a `- ` list marker, plus dotted keys and dict
  entries.
- Highlight comments, numbers, booleans, single-quoted strings, and
  multiline strings.

## 0.3.1

- Documentation fixes: link to the KVD spec, document syntax highlighting
  and version requirements, and describe the OS-aware path resolution
  order.

## 0.3.0

- Auto-detect `kvd-lsp` from `$CARGO_HOME/bin` (or `~/.cargo/bin`) so a
  `cargo install kvd-lsp` install works with no configuration.
- Use the platform-specific binary name (`kvd-lsp.exe` on Windows).
- Add `kvd.serverPathLinux`, `kvd.serverPathDarwin`, and
  `kvd.serverPathWindows` so one synced `settings.json` works across hosts.
- Default each OS setting to its `cargo install` location and expand `~`
  and environment variables in configured paths.
- Document `cargo install` as the recommended install path.
- Pairs with `kvd-lsp` 0.3.0, which adds type-aware schema completion and
  `deprecated` block help.

## 0.1.0

- Initial release.
- Diagnostics, completion, hover, formatting, and go to definition for KVD
  files via `kvd-lsp`.
