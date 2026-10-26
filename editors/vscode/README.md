# KVD for VS Code

Language support for the [KVD](https://github.com/key-value-document/kvd-lsp)
config/data format, powered by the `kvd-lsp` language server.

## Features

- **Diagnostics** — parse errors with line/column ranges, plus schema
  violations when a sibling `<name>.schema.kvd` file exists.
- **Completion** — builtin type names, scalar keywords, document keys, and
  schema descriptor/validation keys.
- **Hover** — one-line help for builtins and value shapes for document keys.
- **Formatting** — whole-document rewrite via `kvd-rs`.
- **Go to definition** — jump between data keys and their schema keys.

## Requirements

This extension is a client for the `kvd-lsp` binary, which is installed
separately:

```sh
cargo install kvd-lsp
```

`cargo install` puts the binary in `$CARGO_HOME/bin` (`~/.cargo/bin` by
default, `%USERPROFILE%\.cargo\bin` on Windows). The extension searches
that directory automatically, so no configuration is needed.

To use a binary elsewhere, set `kvd.serverPath`:

```json
{
    "kvd.serverPath": "/absolute/path/to/kvd-lsp"
}
```

When `kvd.serverPath` is empty, the extension checks `$CARGO_HOME/bin`
(or `~/.cargo/bin`), then `target/debug/kvd-lsp` in a source checkout.

## Extension settings

| Setting                  | Default                    | Description                                                     |
| ------------------------ | -------------------------- | --------------------------------------------------------------- |
| `kvd.serverPath`         | `""`                       | Explicit path for the current host. Overrides everything else.   |
| `kvd.serverPathLinux`    | `~/.cargo/bin/kvd-lsp`     | Path used on Linux when `kvd.serverPath` is empty.               |
| `kvd.serverPathDarwin`   | `~/.cargo/bin/kvd-lsp`     | Path used on macOS when `kvd.serverPath` is empty.               |
| `kvd.serverPathWindows`  | `~/.cargo/bin/kvd-lsp.exe` | Path used on Windows when `kvd.serverPath` is empty.             |

`~`, `$VAR`, `${VAR}`, `${userHome}`, `${env:VAR}`, and `%VAR%` are
expanded. Resolution order is `kvd.serverPath`, then the current OS's
setting, then auto-detection (`$CARGO_HOME/bin`, `~/.cargo/bin`, or a
source checkout). If an OS default does not exist, auto-detection still
runs. Because each OS reads its own key, one synced `settings.json` can
carry a path per host:

```json
{
    "kvd.serverPathLinux": "/home/me/.cargo/bin/kvd-lsp",
    "kvd.serverPathDarwin": "/Users/me/.cargo/bin/kvd-lsp",
    "kvd.serverPathWindows": "C:\\Users\\me\\.cargo\\bin\\kvd-lsp.exe"
}
```

## Commands

| Command                 | Description           |
| ----------------------- | --------------------- |
| `kvd.restartServer`     | Restart the language server. |

## License

MIT
