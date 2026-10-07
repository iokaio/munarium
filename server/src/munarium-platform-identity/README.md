# Platform principal verification

This package compiles Warden's public identity-only implementation from an exact
content-addressed export, without its SQLite grant store or credential broker.
It uses this workspace's pinned dependencies. The exported Rust files remain
unchanged and retain their Apache-2.0 license and notice. The source lock records
every file hash; it is not a release or a human acceptance record.

Update the owner-maintained implementation in `iokaio/munarium-warden`, run its
principal tests, then run `python scripts/export_identity.py <this-directory>/upstream`
from that checkout. Do not hand-edit `upstream/`. Server's identity boundary tests
and the source integrity check must pass after an update.
