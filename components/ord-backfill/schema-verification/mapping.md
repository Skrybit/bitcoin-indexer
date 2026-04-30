# ord → analytics-db mapping

Manually-curated mapping between ord 0.27's HTTP API responses and the
bitcoin-indexer postgres tables they populate. Filled iteratively as
we run `ord-probe.sh` and read [docs.ordinals.com/guides/api.html](https://docs.ordinals.com/guides/api.html).

## ord 0.27 HTTP endpoint surface

ord splits its API into two route families:

- **`/r/*` recursive endpoints** — JSON by default, stable contract.
  Designed for inscription content recursion but covers most metadata
  needs. Smaller surface, doesn't expose block-level enumeration.

- **Human routes (`/inscriptions/...`, `/block/...`, `/output/...`)** —
  HTML by default; return JSON when the request carries
  `Accept: application/json`. Where block enumeration + bulk endpoints
  live.

### Required ord flags for backfill

The `inscriptions.ordinal_number`, `satoshis`, and `current_locations`
tables can't be populated unless ord runs with these flags:

| Flag | Adds | Cost |
| --- | --- | --- |
| `--index-runes` | `runes_*` tables source data | already on |
| `--index-sats` | `inscriptions.sat`, `/r/utxo/.sat_ranges`, `/r/sat/<n>` | ~2-3× sync time, larger redb |
| `--index-addresses` | `/address/<ADDRESS>`, full `inscriptions.address` | small |

Adding any of these to an existing index forces a full resync — ord
won't retrofit sat tracking onto an index built without it.

### Endpoint cheatsheet

| Endpoint | Returns | Use for |
| --- | --- | --- |
| `GET /r/blockheight` | int | Resume cursor — current ord tip |
| `GET /r/blockhash/<h>` | string | Verify block alignment |
| `GET /r/blockinfo/<h>` | block stats (fees, tx count, hashes, percentiles) | Sanity check |
| `GET /inscriptions/block/<h>[/<page>]` | `{ids[], more, page_index}` | **Per-block enumeration** |
| `GET /r/inscription/<id>` | full inscription metadata | Per-id metadata |
| `POST /inscriptions` body: id list | array of inscription objects | **⭐ Batch metadata fetch — turns N HTTP calls into 1** |
| `GET /block/<h>` | block stats + `inscriptions[]` + `runes[]` | One-shot block enumeration |
| `GET /r/utxo/<outpoint>` | `{inscriptions, runes, sat_ranges, value}` | Current locations + sat ranges |
| `GET /r/sat/<n>` | `{ids[], more, page}` | Sat → inscriptions (needs `--index-sats`) |
| `GET /r/children/<id>[/<page>]` | child inscription IDs | `inscription_parents`, `inscription_recursions` |
| `GET /r/parents/<id>` | parent inscription IDs | `inscription_parents` |
| `GET /r/metadata/<id>` | hex CBOR | `inscriptions.metadata` (raw bytes) |
| `GET /content/<id>` | bytes | `inscriptions.content` (BYTEA) |
| `GET /runes` | last 100 runes | `runes.runes` enumeration |
| `GET /rune/<rune>` | rune entry + parent | `runes.runes` per-rune |

Pagination: 100 items per page, `more: true/false`, `page_index: N`.

## Per-table mapping

### ordinals.chain_tip

Trivial — single row tracker.

| column | source |
| --- | --- |
| `id` | constant `t` |
| `block_height` | `GET /r/blockheight` (after backfill: max(processed)) |
| `block_hash` | `GET /r/blockhash/<h>` for that height |

### ordinals.inscriptions

`/r/inscription/<id>` carries every column we need. Probe confirmed
shape on Casey's first inscription (number=0, height=767430).

| column | type | source |
| --- | --- | --- |
| `inscription_id` | TEXT PK | `id` |
| `ordinal_number` | NUMERIC | `sat` (**requires `--index-sats`**) |
| `number` | BIGINT UNIQUE | `number` |
| `classic_number` | BIGINT UNIQUE | also `number` for non-cursed; for cursed: ord assigns negative numbers, those become `classic_number` and the positive one is `number`. Need to disambiguate from charms. |
| `block_height` | NUMERIC | `height` |
| `block_hash` | TEXT | `GET /r/blockhash/<height>` |
| `tx_id` | TEXT | first part of `id` (split on `i`) |
| `tx_index` | BIGINT | not directly exposed — need `/decode/<txid>` or derive from satpoint |
| `address` | TEXT | `address` |
| `mime_type` | TEXT | first segment of `content_type` (before `;`) |
| `content_type` | TEXT | `content_type` |
| `content_length` | BIGINT | `content_length` |
| `content` | BYTEA | `GET /content/<id>` (binary body) |
| `fee` | NUMERIC | `fee` |
| `curse_type` | TEXT | derive from `charms` (cursed types in ord 0.27 vary) |
| `recursive` | BOOLEAN | walk `/content/<id>` for `/content/` references; or check children/parents API |
| `input_index` | BIGINT | not directly exposed — needs raw tx parse |
| `pointer` | NUMERIC | not directly exposed in inscription endpoint; in raw envelope |
| `metadata` | TEXT | `GET /r/metadata/<id>` (hex CBOR — store as-is or decode) |
| `metaprotocol` | TEXT | from `/inscription/<id>` (ord 0.27 includes `metaprotocol`) |
| `parent` | TEXT | `GET /r/parents/<id>` (first ID, or join multiple) |
| `delegate` | TEXT | `delegate` |
| `timestamp` | BIGINT | `timestamp` |

**Gaps**: `tx_index`, `input_index`, `pointer`, `recursive`, `curse_type`
need either a raw tx parse or extra walks. For phase-1 backfill we may
populate them as NULL / 0 / FALSE and let the live indexer correct on
forward progress (it has these in its event stream).

### ordinals.satoshis

Requires `--index-sats`.

| column | source |
| --- | --- |
| `ordinal_number` | sat number |
| `rarity` | derive: parse sat number → rarity per ord's algorithm |
| `coinbase_height` | derive: sat number → block (every 50 BTC = 5,000,000,000 sats subsidy) |

We can compute `rarity` and `coinbase_height` purely from the sat
number — no ord call needed. ord exposes the algorithm in their docs.

### ordinals.locations

History of every location an inscription has been at. Per-block scan
of new inscriptions only gives us the current location. To get
history, we'd need to walk `/r/utxo/<outpoint>` for each output
involved in transfer txs — expensive.

**Decision**: skip `locations` history during backfill. Set
`current_locations` from `/r/utxo` of each inscription's current
output. Live indexer maintains history forward.

### ordinals.current_locations

| column | source |
| --- | --- |
| `ordinal_number` | sat number |
| `output` | inscription's `output` field |
| `offset` | last component of `satpoint` (after second `:`) |

### ordinals.inscription_transfers / .inscription_recursions / .inscription_parents

| Table | Source endpoint |
| --- | --- |
| `inscription_transfers` | derive from locations history — skip in backfill |
| `inscription_recursions` | `/r/children/<id>` paginate, infer recursion |
| `inscription_parents` | `/r/parents/<id>` paginate |

### ordinals.counts_by_*

All derived aggregates. Skip during backfill, recompute via SQL once
base tables are populated.

### runes.runes

| column | source endpoint |
| --- | --- |
| `runes` (table) | `GET /runes` (last 100), `GET /rune/<rune>` per-rune detail |

### runes.balance_changes / .ledger / .supply_changes

These are protocol-event tables — every mint/transfer creates a row.
ord doesn't expose balance-change history via HTTP. Either:
- Replay rune events from raw txs (huge work)
- Skip in backfill, let live indexer compute forward

### ordinals-brc20.tokens / .operations / .balances

ord doesn't track BRC-20 natively. Either:
- Parse inscription content during backfill (replicates what
  bitcoin-indexer's brc20 component already does — duplicated logic)
- Skip in backfill, let live indexer compute forward from chain_tip

**Decision**: skip BRC-20 in backfill. Live indexer has the
`components/ordinals/src/core/meta_protocols/brc20/` code that
processes inscriptions for BRC-20 ops. As long as backfilled
inscriptions are present, the live indexer can replay BRC-20 from
the backfilled chain_tip forward.

## Backfill scope decision

**Phase-1 backfill scope** (minimum viable):
- `ordinals.chain_tip`
- `ordinals.inscriptions` (with NULL for derived/expensive columns)
- `ordinals.current_locations`
- `runes.runes`

**Skipped — let live indexer fill forward from backfilled chain_tip**:
- `ordinals.locations` (history)
- `ordinals.inscription_transfers`
- `ordinals.counts_by_*`
- `runes.balance_changes`, `.ledger`, `.supply_changes`
- All `ordinals-brc20.*` tables

This is the minimum data needed for `bitcoin-indexer`'s resume logic
to skip ahead and start at chain tip.

## Open questions

1. Does the live indexer's resume from chain_tip recompute aggregates
   on its own? Or do we need to seed `counts_by_*` ourselves? Need to
   read `bitcoin-indexer/components/ordinals/src/core/protocol/` for
   the resume path.
2. `inscriptions.classic_number` semantics in ord 0.27 — needs check
   against a cursed inscription sample.
3. Are charm types stable enough to derive `curse_type`? Or skip and
   set NULL.
4. Does `--index-addresses` give us `inscriptions.address` reliably
   even when sats aren't indexed? Likely yes, but verify.
