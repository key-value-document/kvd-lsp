# kvd-lsp

Language server for the KVD config/data format, over stdio.

Built on `tower-lsp 0.20` and `kvd-rs`. Diagnostics come from
`kvd_rs::deserialize`; schema checks from `kvd_rs::schema`; formatting
round-trips through `kvd_rs::serialize`.

## Build and run

```sh
cargo build
./target/debug/kvd-lsp
```

The server speaks LSP over stdin/stdout. Configure your editor to launch
`kvd-lsp` for files ending in `.kvd`.

## Features

- Diagnostics: parse errors with line/col ranges, plus schema violations
  from the embedded `__schema__` block and the sibling `<name>.schema.kvd`
  file when present.
- Completion: builtin type names (`int`, `float`, `bool`, `str`, `list`,
  `map`), scalar keywords (`true`, `false`, `null`), and document keys
  (data, embedded schema, sibling schema).
- Hover: one-line help for builtins; scalar shape/text or map/list size
  for document keys.
- Formatting: whole-document rewrite via `kvd_rs::serialize::to_string`.
- Go to definition: data file key jumps to the sibling schema key, and
  schema file key jumps back to the data key.

## Schema lookup

By convention only, no configuration:

- `app.kvd` is checked against its embedded `__schema__` block and, when
  it exists on disk, the sibling `app.schema.kvd` file.
- A `*.schema.kvd` file is never checked against a further sibling; its
  go-to-definition target is the sibling data file.
