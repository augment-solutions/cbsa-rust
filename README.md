# cbsa-rust

A Rust port of the CICS Banking Sample Application (CBSA), migrated
program-by-program from the original COBOL source at
[`augment-solutions/cics-banking-sample-application-cbsa`](https://github.com/augment-solutions/cics-banking-sample-application-cbsa).
The Java sibling port lives at
[`augment-solutions/cbsa-java`](https://github.com/augment-solutions/cbsa-java).

## Stack

| Concern        | Crate / runtime                     |
|----------------|-------------------------------------|
| Async runtime  | tokio 1                             |
| HTTP server    | axum 0.7 + tower-http               |
| Database       | sqlx 0.8 (PostgreSQL wire protocol) |
| Datastore      | CockroachDB v24.3                   |
| Migrations     | `sqlx::migrate!` (`./migrations`)   |
| Decimal money  | rust_decimal                        |
| Dates / times  | chrono                              |
| Validation     | validator                           |
| Config         | figment (TOML + `CBSA_*` env)       |
| Errors         | thiserror + RFC 7807 ProblemDetail  |
| Logging        | tracing + tracing-subscriber (json) |
| Tests          | testcontainers + testcontainers-modules (CockroachDB) |

## Layout

```
src/
  config.rs       AppConfig (figment-loaded, sortcode-validated)
  db.rs           sqlx pool + migrate + serialization-error helper
  domain.rs       cross-program value types (PROC-TRAN-TYPE, etc.)
  error.rs        CbsaError + ProblemDetail (RFC 7807)
  repository.rs   per-program persistence modules (added by program PRs)
  service.rs      per-program business logic   (added by program PRs)
  web.rs          axum router + AppState; /health
  lib.rs main.rs  library + bin entrypoints
migrations/
  V0__baseline.sql                CockroachDB schema (CUSTOMER, ACCOUNT,
                                  PROCTRAN, CONTROL with hash-sharded PKs)
  V1__proctran_counter_default.sql sequence-backed default for PROCTRAN.counter
docs/
  translation-rules.md  authoritative COBOL → Rust rulebook
.github/workflows/ci.yml  fmt + clippy + test on push/PR
application.toml          default config (override via CBSA_* env)
```

## Running locally

```
# 1. CockroachDB (single-node, in-memory)
cockroach start-single-node --insecure \
  --store=type=mem,size=4GiB --listen-addr=localhost:26257 --background
cockroach sql --insecure --host=localhost:26257 \
  -e "CREATE DATABASE IF NOT EXISTS cbsa"

# 2. Build & run
cargo run --release
```

The server binds to `0.0.0.0:8080` by default; visit
`http://localhost:8080/health` to verify.

## Tests

```
cargo test
```

Integration tests use `testcontainers-rs` to spin up CockroachDB v24.3 on
demand. Docker (or a compatible runtime) must be available.

## Migration provenance

Each Rust source file's owning COBOL program is recorded in the per-program
PR description. The pinned issue **CBSA Rust Migration Ledger** in this
repo tracks the program checklist (INQCUST, INQACC, … XFRFUN, CRDTAGY1..5).
Out-of-scope COBOL artefacts — `BNK1*`, `BNKMENU` (BMS terminal handlers),
`BANKDATA` (seed loader, replaced by `migrations/`), `ABNDPROC` (replaced by
`CbsaError` + axum `IntoResponse`) — are deliberately not ported. See
[`docs/translation-rules.md`](./docs/translation-rules.md) §15.

## License

Apache-2.0 — same as the upstream COBSA source.
