# CBSA COBOL → Rust Translation Rules

This rulebook is authoritative for the CBSA Rust migration. The Translator
follows it when producing Rust; the Reviewer enforces it when reviewing PRs.
Source repo (read-only): `augment-solutions/cics-banking-sample-application-cbsa`.
Target repo: `augment-solutions/cbsa-rust`. Stack: Rust stable (≥ 1.85), axum
0.7, sqlx 0.8, CockroachDB v24.3 (PostgreSQL wire protocol).

## 1. Project layout

```
src/
  config.rs            AppConfig (figment, sortcode-validated)
  db.rs                sqlx pool, migrator, is_serialization_failure
  domain.rs            cross-program value types (ProcTranType, etc.)
  error.rs             CbsaError + ProblemDetail + IntoResponse
  repository.rs        re-exports per-program submodules
    repository/<program>.rs   one file per aggregate / program
  service.rs           re-exports per-program submodules
    service/<program>.rs      one file per COBOL program
  web.rs               axum Router, AppState, /health
    web/<program>.rs          one file per COBOL program (controller + DTOs)
  lib.rs main.rs
migrations/
  NNNN_*.sql           sqlx-managed schema migrations (4-digit prefix
                       per the `sqlx::migrate!` macro convention; e.g.
                       0001_baseline.sql, 0002_proctran_counter_default.sql)
```

One COBOL program → one `service::<program>` module + one
`web::<program>` module exposing a REST endpoint. The COBOL commarea
(`DFHCOMMAREA`) maps to a request DTO; the returned commarea maps to a
response DTO. See §6 for the REST contract.

## 2. PIC clause → Rust/SQL mapping

| COBOL PIC                  | SQL column     | Rust type                            |
|----------------------------|----------------|--------------------------------------|
| `PIC 9(n)` n ≤ 4           | SMALLINT       | `i16`                                |
| `PIC 9(n)` 5 ≤ n ≤ 9       | INTEGER        | `i32`                                |
| `PIC 9(n)` 10 ≤ n ≤ 18     | BIGINT         | `i64`                                |
| `PIC S9(n)V99` 5 ≤ n+2 ≤ 18| NUMERIC(n+2,2) | `rust_decimal::Decimal`              |
| `PIC X(n)`                 | VARCHAR(n)     | `String`                             |
| `PIC 9(8)` date (DDMMYYYY) | DATE           | `chrono::NaiveDate`                  |
| `PIC 9(6)` time (HHMMSS)   | TIME           | `chrono::NaiveTime`                  |
| `PIC X` flag (`Y`/`N`)     | BOOLEAN        | `bool`                               |
| `PIC 9(6)` sortcode        | CHAR(6)        | `String` (zero-padded, validated)    |

Money columns (balances, amounts, overdraft limits, interest rates) are
**always** `rust_decimal::Decimal`; never `f32`/`f64`. Use
`Decimal::round_dp_with_strategy(2, MidpointNearestEven)` whenever
intermediates are exposed.

## 3. REDEFINES

REDEFINES are a memory-overlay trick. Treat the underlying scalar (e.g.
`CUSTOMER-DATE-OF-BIRTH PIC 9(8)`) as the canonical persisted form and expose
the decomposed view (`-DD`, `-MM`, `-YYYY`) only when the API contract
demands it. Use `chrono::NaiveDate::from_ymd_opt(yyyy, mm, dd)` and
`date.day()` etc. Never persist both forms.

## 4. CICS idioms

| CICS construct                            | Rust replacement                          |
|-------------------------------------------|-------------------------------------------|
| `EXEC CICS READ FILE(...)` / `WRITE`      | `sqlx::query!` / `query_as!`              |
| `EXEC CICS LINK PROGRAM(...)`             | direct async call to another `service::*` |
| `EXEC CICS RETURN COMMAREA(...)`          | handler `-> impl IntoResponse`            |
| `EXEC CICS HANDLE ABEND`                  | typed `CbsaError::Abend(...)` variant     |
| `EXEC CICS ABEND ABCODE('xxxx')`          | `Err(CbsaError::abend("xxxx", ...))`      |
| `EXEC CICS ENQ` / `DEQ` (NCS counters)    | `SELECT ... FOR UPDATE` row-lock or       |
|                                           | `INSERT ... ON CONFLICT` UPSERT           |
| `EXEC CICS ASKTIME` / `FORMATTIME`        | `chrono::Local::now().date_naive()` etc.  |
| `DFHRESP(NORMAL)` checks                  | `?` on `Result`; do not retry on success  |
| `DFHRESP(SYSIDERR)` 100× retry loop       | drop. CockroachDB has its own retries.    |
| `DFHRESP(NOTFND)`                          | `Option::None` from `fetch_optional` →    |
|                                           | populate response with fail code          |

## 5. CockroachDB / sqlx conventions

- All persistence goes through `sqlx::PgPool` (PostgreSQL wire protocol).
  Prefer the compile-time-checked macros (`query!`, `query_as!`) when an
  offline `.sqlx/` cache exists; otherwise fall back to the runtime
  variants (`sqlx::query`, `sqlx::query_as`). **Never** hand-build SQL
  strings via `format!`.
- Wrap multi-statement business operations in
  `let mut tx = pool.begin().await?; ... tx.commit().await?;`. **All**
  queries inside the block must be issued through `&mut *tx` (or
  `&mut tx as &mut PgConnection`) so they participate in the transaction.
  Queries issued through the outer `pool` instead run outside it.
- CockroachDB returns serialization errors (SQLSTATE `40001`) on
  contended transactions. The first program PR that needs it adds a
  `db::with_retry` async helper that re-runs the whole closure on
  `db::is_serialization_failure(&err)`. Never re-throw `40001` to the
  caller.
- Sequence-allocation idioms (NCS `HBNKCUST`, `HBNKACCT`) are replaced by
  an UPSERT against `control`:
  `UPDATE control SET customer_last = customer_last + 1 RETURNING customer_last`.
  This is atomic in CockroachDB.
- For high-write tables (`proctran`), the primary key is hash-sharded
  (`USING HASH WITH (bucket_count = 16)`) — see `migrations/0001_baseline.sql`.

## 6. REST contract

- Each program gets a route under `/api/v1/<program-lowercase>`.
- Method follows intent: `INQ*` → `GET`, `CRE*` → `POST`,
  `UPD*` → `PUT`, `DEL*` → `DELETE`, `XFRFUN`/`DBCRFUN` → `POST`.
- Path params for natural keys, e.g. `GET /api/v1/inqcust/{customer_number}`.
- Request/response DTOs are `serde::Deserialize` / `Serialize` `struct`s
  in `web::<program>`. Field names match the snake_case form of the
  commarea (e.g. `INQCUST-CUSTNO` → `customer_number`).
- Validation via the `validator` crate's `#[derive(Validate)]` and
  `#[validate(...)]` field attributes; constraint violations are
  converted into HTTP 400 by the controller calling `.validate()?` and
  mapping `ValidationErrors` into a `ProblemDetail` body.
- Required fields on request DTOs are expressed exclusively through
  `#[validate(...)]` constraints (`length`, `regex`, `range`). Do **not**
  attempt to enforce required-ness inside `Deserialize` impls or via
  manual `Option::ok_or_else` panics: serde failures bypass the unified
  ProblemDetail mapping and produce inconsistent 4xx bodies.
- A "not found" commarea result (e.g. `INQCUST-INQ-FAIL-CD = '1'`) maps
  to HTTP 404 with a `ProblemDetail` whose `failCode` field is the
  COBOL fail code; it is **not** an `Err(CbsaError::*)` return.
- Hard failures (commarea-style abend) return `Err(CbsaError::Abend(...))`,
  which the `IntoResponse` impl maps to HTTP 500 with `abendCode` set.

## 7. Concurrency / async

- Tokio (multi-threaded scheduler) is the only runtime. Handlers are
  `async fn`; long-running compute is delegated to
  `tokio::task::spawn_blocking`.
- For the async credit-agency programs (CRDTAGY1..5), use
  `tokio::spawn` + `tokio::sync::mpsc` (or `JoinSet` for fan-out/fan-in).
  Do **not** use `std::thread`.
- Never `tokio::time::sleep` to mimic CICS `DELAY FOR SECONDS(...)`
  retries; rely on CockroachDB's own retry semantics (and the
  `db::with_retry` helper for application-level retries).


## 8. Error model

- Define `CbsaError` once in `src/error.rs` with at minimum:
  `Validation(String)`, `Abend(&'static str, String)`, `Database(sqlx::Error)`.
  Each program returns a variant with its program-specific abend code
  (e.g. `"CVR1"` for INQCUST VSAM read failure) only for genuinely
  unrecoverable errors.
- The `axum::response::IntoResponse` impl on `CbsaError` is the single
  point that builds an RFC 7807 `ProblemDetail` body, identical for
  every program:
    - `Validation(_)` → 400 with `title="Validation failed"`
    - `Abend(code, _)` → 500 with `abendCode = code`
    - `Database(_)` (or any other variant) → 500 with `abendCode = "UNEX"`
- "Not found" is **never** an `Err(CbsaError::*)` — see §6: handlers
  translate the commarea fail flag (e.g. `INQCUST-INQ-FAIL-CD = '1'`)
  into a `Response` with status 404 and a `ProblemDetail` body directly.
- Reserved abend codes used by every program:
  - `HWPT` — PROCTRAN audit-trail insert failure (§12)
  - `XRTY` — outer retry-exhaustion around `db::with_retry`
  - `UNEX` — fallthrough for any otherwise-unmapped error

## 9. Testing

- Unit-level: per-service tests using `testcontainers-modules`'
  `CockroachDb` image, started once per integration-test binary via a
  `tokio::sync::OnceCell`. Run `db::MIGRATOR.run(&pool).await` against
  the freshly-started container before any tests issue queries.
- Web-layer: build the axum `Router` with a mock `AppState` and exercise
  it via `tower::ServiceExt::oneshot` + `axum::body::to_bytes` to assert
  status, content-type, and JSON body shape.
- Each program PR adds at least: success path, not-found path,
  validation failure path, and one invariant assertion (e.g. balance
  non-negative after operation).
- Tests must use distinct sortcodes/customer-numbers per case (or a
  `serial_test::serial` guard) when they mutate the shared CockroachDB
  container, so test ordering does not leak state between cases.

## 10. Configuration: typed properties + startup validation (sortcode and friends)

- Every COBOL-shaped fixed-width identifier configured via figment is
  bound through the `AppConfig` struct in `src/config.rs` and validated
  by `AppConfig::validate()`. Malformed values fail process startup
  (binary returns `Err(ConfigError::*)`) instead of surfacing later as
  request-time 500s.
- Keep zero-padded fixed-width identifiers as `String` end-to-end in
  domain, services, handlers, DTOs, and tests. Convert to integer only
  at the sqlx persistence boundary when a column truly requires it, and
  only after a regex/length constraint has guaranteed the value is
  exactly six ASCII digits.
- When reading a fixed-width numeric identifier back from a numeric
  persistence column, always zero-pad it back to its COBOL width
  (`format!("{:06}", n)`) before returning it to the domain or wire
  contract.
- Use `^[0-9]{6}$` (explicit ASCII range) rather than `\d{6}` in
  `validator` `#[validate(regex = ...)]` constraints. The `regex` crate
  in default Unicode mode treats `\d` as `\p{Nd}`, which matches
  non-ASCII digits like the fullwidth `０-９` and would let unexpected
  values pass and then fail downstream string comparisons or sqlx
  lookups.
- Enforce the same six-digit ASCII invariant on the read side too:
  domain records that carry a sortcode (e.g. `AccountDetails`,
  `CustomerDetails`) validate `^[0-9]{6}$` in their constructor (or via
  a `TryFrom<String>` impl) so handlers do not need to re-pad or
  re-validate before serialising. Any non-conforming value read from
  the database fails loudly at the repository → domain boundary.
- TOML accepts both quoted strings and bare integers; bare numerics
  silently lose leading zeros and would then fail `^[0-9]{6}$` startup
  validation. Always quote zero-padded fixed-width identifiers in
  `application.toml` (and any environment-variable defaults), e.g.
  `cbsa.sortcode = "987654"`.

## 11. PR / commit conventions

- One COBOL program per branch and per PR. Branch name:
  `migrate/<program-lowercase>`.
- Commit subject: `feat(<program>): translate <PROGRAM> to Rust`.
- PR body: brief rationale, link to the source `.cbl` file, list of
  follow-ups.
- The Reviewer is the auto-review GitHub App; the merge gate is its
  verdict on the PR's HEAD commit (zero blocking inline comments →
  clean; ≥ 1 → blocking). See the supervisor instructions for the
  polling protocol.

## 12. PROCTRAN audit-trail write failures

Every program in this codebase writes a `proctran` row as part of the
mutating transaction it performs (account/customer create, update,
delete, debit/credit, transfer). A failure to write `proctran` is
**not** a domain-level failure of the operation; it is a system abend
that must be escalated to operations.

- A non-retryable `sqlx::Error` returned from a `proctran` insert MUST
  be wrapped as `CbsaError::abend(PROCTRAN_ABEND_CODE, ...)` where
  `PROCTRAN_ABEND_CODE = "HWPT"` (defined in `src/error.rs`). The
  `IntoResponse` impl then surfaces this as a 500 abend, which is the
  right classification for an audit-trail outage.
- Do not map `proctran` insert failures to a domain "fail code" (e.g.
  CRECUST `"1"`, UPDCUST `"3"`). Doing so makes an audit-write outage
  indistinguishable from a real data-mutation failure and silently
  downgrades a 500 to a 200-with-fail-code.
- Always surface SQLSTATE `40001` (serialization failures) unchanged
  so the surrounding `db::with_retry` wrapper can retry the whole
  transaction. Wrap *only* the non-retryable branch:
  ```rust
  match insert_proctran(&mut *tx, &row).await {
      Ok(()) => {}
      Err(err) if db::is_serialization_failure(&err) => return Err(err.into()),
      Err(err) => {
          return Err(CbsaError::abend(
              error::PROCTRAN_ABEND_CODE,
              format!("<PROGRAM> failed to write the audit trail: {err}"),
          ));
      }
  }
  ```
- Use the constant name `PROCTRAN_ABEND_CODE` (not `ABEND_CODE`,
  `WPCD`, or any program-specific spelling) so the intent of the
  match arm is obvious in code review.
- The wrap may live in either the repository or the service layer,
  whichever owns the `proctran` insert call site, but it must be the
  *innermost* wrap around that single insert, not a coarser block over
  the whole transaction — otherwise it swallows non-PROCTRAN failures
  too.
- `PROCTRAN_ABEND_CODE` is for PROCTRAN insert failures only. Reserve
  `RETRY_EXHAUSTED_ABEND_CODE = "XRTY"` for the outer retry-exhaustion
  wrap around `db::with_retry` so a serialization-retry exhaustion
  does not get reported as `HWPT`. Any other `sqlx::Error` that
  escapes `db::with_retry` should be returned as
  `CbsaError::Database(_)` and surfaces as `UNEX`.

## 13. Translator checklist

Quick scan before opening a program PR. Every item must hold.

- [ ] All public `service::*` functions return `Result<_, CbsaError>`
      and validate their inputs at the top of the function (length,
      regex, range) before issuing any query.
- [ ] Every handler that accepts a JSON body deserialises into a
      `#[derive(Deserialize, Validate)]` struct and calls
      `payload.validate()?` as its first action.
- [ ] All error paths return `ProblemDetail` (via `CbsaError::into_response`)
      — no per-program error struct on the wire.
- [ ] Stochastic behavior takes a `&dyn RngCore` (or a small
      generator trait) and a `chrono::Utc::now`/`Local::now` is hidden
      behind an injected `Clock` trait, with deterministic test
      doubles.
- [ ] Retry-exhaustion → fail code `"R"` → 503; not-found → `"1"` →
      404; empty-table → `"1"` → 404.
- [ ] `control` row is `UPDATE`d, never `DELETE`+inserted in production
      code; tests skip `control` or re-seed the baseline row.
- [ ] PROCTRAN insert failures are wrapped as
      `CbsaError::abend(PROCTRAN_ABEND_CODE, ...)` (§12); SQLSTATE
      40001 is propagated unchanged for `db::with_retry`.
- [ ] Sortcode and other zero-padded fixed-width identifiers are
      quoted in TOML, validated `^[0-9]{6}$`, kept as `String`
      end-to-end (§10).

## 14. Out of scope

`BNK1*`, `BNKMENU` (BMS terminal handlers), `BANKDATA` (seed loader;
replaced by sqlx migrations), and `ABNDPROC` (replaced by `CbsaError`
+ axum `IntoResponse`) are dropped and must not be translated.
