-- CBSA baseline schema. Derived from CBSA COBOL copybooks under
-- cics-banking-sample-application-cbsa/src/base/cobol_copy/.
--
-- Mapping summary:
--   PIC 9(n)        -> integer / bigint sized to fit n digits
--   PIC 9(n)V99     -> numeric(n+2,2)
--   PIC X(n)        -> varchar(n) (NOT NULL with default '' to match COBOL spaces)
--   PIC 9(8) date   -> date (DDMMYYYY in COBOL, decoded in app layer)
--   REDEFINES       -> not modeled; the underlying scalar wins.
--   eyecatcher      -> not persisted; an in-memory invariant only.
--
-- The branch sortcode is fixed by SORTCODE.cpy (PIC 9(6) VALUE 987654) and is
-- carried as a column for forward compatibility.

CREATE TABLE customer (
    sortcode          CHAR(6)        NOT NULL,
    customer_number   BIGINT         NOT NULL,
    name              VARCHAR(60)    NOT NULL DEFAULT '',
    address           VARCHAR(160)   NOT NULL DEFAULT '',
    date_of_birth     DATE           NOT NULL,
    credit_score      SMALLINT       NOT NULL DEFAULT 0,
    cs_review_date    DATE,
    CONSTRAINT customer_pk PRIMARY KEY (sortcode, customer_number),
    CONSTRAINT customer_credit_score_chk CHECK (credit_score BETWEEN 0 AND 999)
);

CREATE TABLE account (
    sortcode             CHAR(6)        NOT NULL,
    account_number       BIGINT         NOT NULL,
    customer_number      BIGINT         NOT NULL,
    account_type         VARCHAR(8)     NOT NULL DEFAULT '',
    interest_rate        NUMERIC(6,2)   NOT NULL DEFAULT 0,
    opened               DATE           NOT NULL,
    overdraft_limit      NUMERIC(12,2)  NOT NULL DEFAULT 0,
    last_stmt_date       DATE,
    next_stmt_date       DATE,
    available_balance    NUMERIC(12,2)  NOT NULL DEFAULT 0,
    actual_balance       NUMERIC(12,2)  NOT NULL DEFAULT 0,
    CONSTRAINT account_pk PRIMARY KEY (sortcode, account_number),
    CONSTRAINT account_customer_fk FOREIGN KEY (sortcode, customer_number)
        REFERENCES customer (sortcode, customer_number)
);

CREATE INDEX account_customer_idx
    ON account (sortcode, customer_number);

-- PROCTRAN is the high-write-volume audit log. We hash-shard the primary key
-- across 16 buckets so sequentially-allocated counters do not create a hot
-- range on insert. CockroachDB-specific syntax: USING HASH WITH (bucket_count
-- = N) is parsed as a regular index by Postgres-compatible tooling.
CREATE TABLE proctran (
    sortcode               CHAR(6)        NOT NULL,
    counter                BIGINT         NOT NULL,
    logical_delete         BOOLEAN        NOT NULL DEFAULT FALSE,
    tran_date              DATE           NOT NULL,
    tran_time              TIME           NOT NULL,
    tran_ref               BIGINT         NOT NULL DEFAULT 0,
    tran_type              CHAR(3)        NOT NULL,
    description            VARCHAR(40)    NOT NULL DEFAULT '',
    amount                 NUMERIC(12,2)  NOT NULL DEFAULT 0,
    CONSTRAINT proctran_pk PRIMARY KEY (sortcode, counter) USING HASH WITH (bucket_count = 16)
);

CREATE INDEX proctran_date_idx
    ON proctran (sortcode, tran_date) USING HASH WITH (bucket_count = 16);

-- CONTROL is a single-row table tracking next-id counters. Modelled with a
-- fixed primary key column to make UPSERTs explicit; in COBOL it was a single
-- VSAM record.
CREATE TABLE control (
    id                  CHAR(6)  NOT NULL PRIMARY KEY DEFAULT 'GLOBAL',
    customer_count      BIGINT   NOT NULL DEFAULT 0,
    customer_last       BIGINT   NOT NULL DEFAULT 0,
    account_count       BIGINT   NOT NULL DEFAULT 0,
    account_last        BIGINT   NOT NULL DEFAULT 0
);

INSERT INTO control (id) VALUES ('GLOBAL') ON CONFLICT DO NOTHING;
