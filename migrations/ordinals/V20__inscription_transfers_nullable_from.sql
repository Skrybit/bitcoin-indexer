-- INFRA-213: Backstop for transfer provenance gaps under ADR-079 §2 retention.
-- `locations` history is pruned past the retention window while
-- `current_locations` is not; `insert_locations` now falls back to
-- `current_locations` to derive from_block_height/from_tx_index. If any
-- unforeseen gap still yields NULL provenance, degrade to a NULL row instead
-- of aborting the whole block insert and crash-looping the indexer.
-- Consumers tolerate NULLs (API recomputes + INNER JOINs; ETL joins drop them).

ALTER TABLE inscription_transfers ALTER COLUMN from_block_height DROP NOT NULL;
ALTER TABLE inscription_transfers ALTER COLUMN from_tx_index DROP NOT NULL;
