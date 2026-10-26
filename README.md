# kvd-lsp

Language server for [KVD](https://github.com/key-value-document/kvd-spec),
an opinionated human-readable config/data format, over stdio.

Built on `tower-lsp 0.20` and `kvd-rs`. Diagnostics come from
`kvd_rs::deserialize`; schema checks from `kvd_rs::schema`; formatting
round-trips through `kvd_rs::serialize`.

## Install

```sh
cargo install kvd-lsp
```

This puts `kvd-lsp` in `$CARGO_HOME/bin` (`~/.cargo/bin` by default,
`%USERPROFILE%\.cargo\bin` on Windows). Requires Rust 1.85 or newer. The
[VS Code client](https://github.com/key-value-document/kvd-lsp/tree/main/editors/vscode)
searches that directory automatically.

## Build and run

```sh
cargo build
./target/debug/kvd-lsp
```

The server speaks LSP over stdin/stdout. Configure your editor to launch
`kvd-lsp` for files ending in `.kvd`.

## Features

- Diagnostics: parse errors with line/col ranges, plus schema violations
  from the sibling `<name>.schema.kvd` file when present.
- Completion: builtin type names (`int`, `float`, `bool`, `str`, `bytes`,
  `list`, `dict`), scalar keywords (`true`, `false`, `null`), and document
  keys. In `*.schema.kvd` files, descriptor keys (`type`, `optional`,
  `description`, `deprecated`, `element`, `validation`), constraint keys
  inside `validation` blocks, and sibling data keys are offered; `element`
  is only offered for `list`/`dict` descriptors, and constraints are
  filtered by the descriptor type (`min`/`max`/`exclusive_min`/
  `exclusive_max` for numbers, `min_len`/`max_len`/`pattern` for strings,
  `min_len`/`max_len` for `bytes` and containers, none for `bool`). Inside a
  `deprecated` block, `reason` and `since` complete. After
  `type:`/`element:` the builtin types complete. Key completion offers only
  the next path segment at the cursor's nesting level, and value positions
  never offer keys.
- Hover: one-line help for builtins; scalar shape/text or dict/list size
  for document keys. In schema files, descriptor, validation, and
  `deprecated` block keys get spec help.
- Formatting: whole-document rewrite via `kvd_rs::serialize::to_string`.
- Go to definition: data file key jumps to the sibling schema key, and
  schema file key jumps back to the data key.

## Schema lookup

By convention only, no configuration:

- `app.kvd` is checked against the sibling `app.schema.kvd` file when it
  exists on disk.
- A `*.schema.kvd` file is never checked against a further sibling; its
  go-to-definition target is the sibling data file.

## License

MIT
