-- SKRYBITDEV-586: Persistent record of blocks the indexer skipped due to
-- parse/compress/standardize/download failures. See ordinals V19 for rationale.

CREATE TABLE failed_blocks (
    block_height        NUMERIC PRIMARY KEY,
    block_hash          TEXT,
    error_kind          TEXT NOT NULL,
    error_message       TEXT NOT NULL,
    retry_count         INT NOT NULL DEFAULT 0,
    last_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at         TIMESTAMPTZ
);

CREATE INDEX failed_blocks_unresolved_idx
    ON failed_blocks (block_height)
    WHERE resolved_at IS NULL;
