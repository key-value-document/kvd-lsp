# Changelog

## 0.3.0

- Auto-detect `kvd-lsp` from `$CARGO_HOME/bin` (or `~/.cargo/bin`) so a
  `cargo install kvd-lsp` install works with no configuration.
- Use the platform-specific binary name (`kvd-lsp.exe` on Windows).
- Add `kvd.serverPathLinux`, `kvd.serverPathDarwin`, and
  `kvd.serverPathWindows` so one synced `settings.json` works across hosts.
- Default each OS setting to its `cargo install` location and expand `~`
  and environment variables in configured paths.
- Document `cargo install` as the recommended install path.

## 0.1.0

- Initial release.
- Diagnostics, completion, hover, formatting, and go to definition for KVD
  files via `kvd-lsp`.
