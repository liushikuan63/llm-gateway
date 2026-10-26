# Project-local SQLite WAL-reset backport

This directory copies the pinned `libsqlite3-sys 0.30.1` package, retaining its
licenses, API, build configuration and SQLite 3.46.0 version/source identifiers.
It is SQLite **3.46.0 + walreset-053bd393 backport**, not an upstream 3.51.3 release.

Only `sqlite3/sqlite3.c` differs from the packaged source: recheck live/cached
WAL salts after acquiring read-lock slot 0, before backfilling; always release
the original exclusive read lock. No fault-injection hook is in this source.

Official fix: https://github.com/sqlite/sqlite/commit/fe57e14b49f9189b56da8233ab3415ce5ff6b1ff
(Fossil `053bd3930f827156fd67ea4546a36227cffbb6f8bada3b5d1a7cf5f1867ac624`).
Risk description: https://sqlite.org/wal.html#walresetbug

Original amalgamation SHA256:
`c01235302fe80da901fb70c7622c39147e29d9f29b7f6eb746b23517f320c90d`.
Patched amalgamation SHA256:
`7413c18f5457ffed275c6313436b6ded5a5e0d44a81301a837ec2827789a0b54`.

The unchanged SQLCipher sources are copied for package provenance only; this
project's locked SQLite bundled feature path uses `sqlite3/sqlite3.c`. This
backport does not claim to fix an enabled SQLCipher build. Do not enable that
alternative without its own qualification. No global Cargo registry or system
SQLite was changed. Review the stage's old/fixed owned-C reproducer and B
isolation receipts; ordinary concurrency tests alone are not this race proof.
