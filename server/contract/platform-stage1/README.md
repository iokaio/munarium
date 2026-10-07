# Proposed Stage 1 foundation candidate

These Apache-2.0 files are unchanged public hub candidates, not accepted/released contracts.
[vendor-lock.json](vendor-lock.json) identifies the exact public hub revision and the
LF-normalized SHA-256 of every source file. Do not edit generated copies manually.

Reproduce from the matching `munarium-platform` revision with
`python scripts/vendor_stage1.py --destination CONSUMER_CANDIDATE_DIRECTORY`.
The exporter refuses to replace different bytes. Candidate export does not publish,
activate, accept or confer authority. Registry's separately pinned signed v2 manifest
contract remains the manifest admission source of truth.
