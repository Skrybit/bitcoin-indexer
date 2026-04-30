# ord → analytics-db mapping

Manually-curated mapping between ord's HTTP API responses and the
bitcoin-indexer postgres tables they populate. **Filled in iteratively**
as we run `ord-probe.sh` against representative block heights and
compare to `schema-summary.md`.

Each row answers: *which ord call(s) yield this column, and what
transform is needed?*

## Status

| ord endpoint | covered? | notes |
| --- | --- | --- |
| `GET /r/blockheight` | yes | trivial — bare integer |
| `GET /r/blockhash/{h}` | yes | bare hex string |
| `GET /r/inscriptions/block/{h}/{page}` | partial | returns inscription IDs only — must hit `/r/inscription/{id}` per-id for fields |
| `GET /r/inscription/{id}` | TBD | fields TBD until we run probe |
| `GET /r/sat/{n}` | TBD | sat tracking shape |
| `GET /r/runes/{page}` | TBD | runes paginated list |
| `GET /r/rune/{rune}` | TBD | per-rune detail |
| balance changes | UNKNOWN | unclear if exposed via HTTP — may require ord CLI |
| BRC-20 ops | UNKNOWN | ord doesn't natively track BRC-20 — may need separate path |

## Per-table mapping

Tables enumerated from `tables.txt` (run `discover-schema.sh` first).

### ordinals.chain_tip

| column | type | source | notes |
| --- | --- | --- | --- |
| id | bool PK | constant `t` | single row |
| block_height | int | `GET /r/blockheight` | direct |
| block_hash | text | `GET /r/blockhash/{height}` | strip quotes |

### ordinals.inscriptions

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD — populate after `ord-probe.sh` run |

### ordinals.satoshis

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### ordinals.locations

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### ordinals.current_locations

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### ordinals.inscription_transfers

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### ordinals.counts_by_*

These are **derived aggregates** — not populated directly. After
backfilling base tables, run a one-shot recompute query to fill them.

### runes.runes

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### runes.balance_changes

| column | type | source | notes |
| --- | --- | --- | --- |
| ... | | | TBD |

### ordinals-brc20.tokens / .operations / .balances

ord-server may not natively expose BRC-20 protocol data via HTTP.
Investigate:

- ord CLI on the host: `ord wallet inscriptions`, custom queries?
- Skrybit fork's bitcoin-indexer already has BRC-20 tracking — maybe
  this should run *after* ordinals backfill so the live indexer fills
  the BRC-20 tables on its own as it crawls forward from the
  backfilled chain_tip
- Or: parse inscription content for BRC-20 deploy/mint/transfer JSON
  during backfill (replicates what bitcoin-indexer's brc20 component
  already does)

Decision pending — flag in next session.

## Open questions

1. Does ord HTTP expose **all** historical inscription metadata, or
   only current state? Need to verify by probing a known reorg point.
2. ord 0.27 changed sat tracking — does the response shape match
   what V1__satoshis.sql expects, or is a translation needed?
3. Pagination semantics on `/r/inscriptions/block/{h}/{page}` — fixed
   page size? deterministic? does it stably enumerate?
4. How are "unbound" inscriptions (V17__unbound_inscription_sequence)
   represented in ord? Different endpoint?
