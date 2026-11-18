# Local lopdf 0.47.0 patch

Base: crates.io `lopdf` 0.47.0, MIT, package SHA-256
`c6ad532ff9262dd4bfd6e085400cbb39959d48cb8d08518b01b68f77bdf6feca`.
Source files, upstream README, LICENSE and rustfmt configuration are retained.
The manifest retains upstream runtime dependencies/features; examples, benches
and upstream development dependencies are omitted. Blank-line whitespace in the
upstream README is normalized; fixed-width PDF fixtures retain their spaces. MetaSafe exercises the patch
through `tests/pdf_budget.rs` and its existing document tests.

The pinned upstream API only limits individual streams. A check after loading
cannot protect eager object/xref decoding, and an object filter runs after xref
loading. The local path dependency makes the shared budget available to both
the CLI and Tauri without a separate platform-specific helper process.

Changes relative to the upstream source:

- `load_options.rs`: optional `max_total_decompressed_size`; defaults to `None`
  for existing callers.
- `decode_budget.rs`, `lib.rs`: shared, mutex-protected remaining byte budget.
- `object.rs`: cap each filter by the smaller of its existing limit and remaining
  aggregate budget; charge each successful filter output, including intermediate
  layers. Plain streams, including `/Filter []`, are charged before cloning.
- `object_stream.rs`: reuse the parser with budgeted decoded content.
- `parser_aux.rs`, `parser/mod.rs`: budget xref decoding before parsing, retaining
  the existing normalization of the decoded stream dictionary.
- `reader.rs`: share the budget across eager/encrypted/lazy object-stream reads,
  xref revisions, hybrid xrefs and reconstruction readers.

MetaSafe sets 64 MiB per load and retains 8 MiB per stream. Catalog XMP has its
existing separate 1 MiB bound. The aggregate budget counts successful decoding
outputs, including repeated decoding; it is neither a process memory limit nor
a CPU deadline. Parsed objects, input/output snapshots and decoder overhead use
additional memory. The application uses strict loading and no default lopdf
features. Do not replace this dependency with unpatched crates.io 0.47.0.

Remove this local patch when an upstream version supplies equivalent enforcement
and passes the regression suite. A new upstream version needs a fresh comparison;
do not overwrite this directory as an ordinary dependency update.
