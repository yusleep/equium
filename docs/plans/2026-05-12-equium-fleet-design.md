# Equium Fleet Tool Design

## Goal

Build a local-only operator tool for Equium mining preparation: generate a new funding wallet plus worker wallets, inspect balances through a configured Solana RPC, distribute SOL from the funding wallet to workers, run multiple CPU miner workers, and write structured run logs.

## Safety Boundary

The tool never uses an existing long-term wallet by default. It creates a fresh funding keypair and fresh worker keypairs under `.local/equium-fleet/`. The user manually transfers SOL to the funding wallet, then explicitly runs a distribution command. Distribution defaults to dry-run and only broadcasts transactions with `--live`.

File permission hardening is intentionally out of scope for the first version per user request.

## Architecture

Add a Rust workspace binary crate at `clients/fleet-tools` named `equium-fleet`. Rust is used because the repo already builds Solana Rust dependencies, while the root Node dependencies are not installed locally.

The crate has:

- a small CLI layer using `clap`
- a library layer for testable manifest, distribution, path, and log parsing logic
- Solana RPC calls only in command handlers
- child-process orchestration for the existing `target/release/equium-miner`

Runtime state lives in:

```text
.local/equium-fleet/
  manifest.json
  funding.json
  workers/
    worker-001.json
  runs/
    YYYYMMDD-HHMMSS/
      worker-001.jsonl
```

## Commands

`wallets create --workers N`

Creates a funding wallet and N worker wallets unless a manifest already exists. Prints addresses only.

`wallets status --rpc-url URL`

Reads funding and worker balances and prints a compact table.

`wallets distribute --rpc-url URL --per-worker-sol SOL [--live]`

Plans transfers from funding wallet so every worker reaches the requested target balance. Without `--live`, prints the plan and does not sign or broadcast. With `--live`, signs from `funding.json` and sends Solana system transfers.

Optimized behavior:

- `--batch-size N` packs multiple worker transfers into each transaction, defaulting to 8.
- Dry-runs fetch the funding balance and print planned transaction count, estimated fee reserve, total required balance, and any shortfall.
- Live mode refuses to broadcast if the funding wallet does not cover planned transfers plus the estimated fee reserve.

`mine fleet --rpc-url URL --workers all|N --miner-bin PATH`

Starts one `equium-miner` process per selected worker keypair. Each worker uses the same RPC endpoint and writes stdout-derived JSONL events plus raw lines to its run log.

Optimized behavior:

- `--workers` also accepts explicit subsets such as `2-4,worker-008`.
- `--max-restarts N` restarts failed worker processes before declaring the fleet failed.
- `--restart-delay-ms N` controls backoff between restarts.
- `--stagger-ms N` spaces worker startup to reduce the initial RPC spike.
- JSONL logs include supervisor events for stagger, start, exit, and restart.

## Testing

Tests cover pure logic first:

- worker names are stable and zero-padded
- manifests contain the expected funding/worker paths
- distribution planning tops workers up to the target and skips funded workers
- dry-run distribution returns a plan without creating signatures
- miner stdout lines are converted into structured JSONL events

Networked RPC behavior remains manually verified because it depends on live Solana mainnet and private funds.
