# ord-backfill

Translates state from a fully-synced ord-server into the bitcoin-indexer
postgres schema (`ordinals_*`, `ordinals-brc20_*`, `runes_*`). Used to
seed analytics-db quickly instead of waiting for the live indexer to
catch up from genesis.

## Phases

This component is built in deliberate phases to minimise build cycles
and keep iteration fast while the schema mapping is still being figured
out.

### Phase 1 — Schema discovery (shell, in this repo)

Pure POSIX shell + curl + jq. No build step, no language toolchain.
The goal is to lock down the **mapping** between:

- ord HTTP API responses → analytics-db rows
- ord state we want vs what the API actually exposes (gaps → fall back to `ord` CLI on the host)

Output of phase 1:

- `schema-verification/schema-summary.md` — every table + column in the
  three analytics databases (ordinals, ordinals-brc20, runes)
- `schema-verification/samples/` — captured JSON responses from a real
  ord instance, one per endpoint of interest, for a small set of
  representative block heights
- `schema-verification/mapping.md` — manually-curated mapping doc
  describing `ord_endpoint → table.column` for every column we need
  to populate

### Phase 2 — Rust translator (compiled crate, this directory)

Once mapping is locked, the actual translator lives at
`components/ord-backfill/src/`. It will be:

- A workspace member of bitcoin-indexer (reuses `pg::*`, `config`,
  `ord` types from sibling crates)
- Built via `cargo build -p ord-backfill`
- A single binary that reads from ord (HTTP) and writes to postgres
  via `COPY ... FROM STDIN` into staging tables, then `INSERT ... ON
  CONFLICT DO UPDATE` to canonical tables
- Idempotent + resumable per-height

### Phase 3 — Operations

Either:

- One-shot run from a developer machine pointed at canary
  analytics-db, then production
- NixOS module deploys it as a systemd oneshot on the ord host or on
  analytics-db

Decided in phase 2 once we know runtime characteristics.

## Why shell first

Schema discovery is the hard part — it's where we figure out:

- Which ord endpoints carry which fields
- Where ord's data shape diverges from our schema
- What needs ord-CLI fallback (e.g. ranges, or anything not exposed
  via HTTP)

Doing this in cargo means a compile-edit-run loop measured in minutes.
Doing it in shell is sub-second. We get the mapping nailed down first,
then write the Rust once, knowing what we're building.

## Local invocation via deployments-pve

The deployments-pve repo's `nix/run/` exposes thin wrappers so you can
run the shell scripts from anywhere in the dev environment:

```sh
nix run .#ord-discover-schema       # parse migrations, write schema-summary.md
nix run .#ord-probe -- 825000        # curl ord, dump samples for height 825000
```

Both scripts resolve their location via the deployments-pve git root.
