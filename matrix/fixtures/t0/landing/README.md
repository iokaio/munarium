# The landing-export fixture

An immutable CSV export of an `opportunities` table beside the manifest the
`landing` adapter reads — eight rows, three regions, two closed stages. It is
the **mode-A blob scenario's** input: this directory is uploaded to a blob
container under `landing/crm/` with an operator's own identity, and a
`store: az` DataSource is registered that Matrix reads through its **managed
identity** (`Storage Blob Data Reader` on the container). Until 2026-08-30 the
adapter could read only a filesystem, so that path had nothing to prove it.

`manifest.json` carries the file's sha256 and row count. The bytes are LF and
generated rather than hand-typed; edit the CSV and the adapter refuses it as
changed under its manifest, which is the point.
