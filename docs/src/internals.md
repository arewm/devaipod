# Internals

This page links to the internal rustdoc documentation for devaipod's crates.

## Crates

- [`devaipod`](internals/devaipod/index.html) - Main binary and library

## Building the docs

To build the internals documentation locally:

```bash
cargo doc --workspace --no-deps --document-private-items
```

The documentation will be in `target/doc/`.
