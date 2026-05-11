# Equium Fleet Tools

`equium-fleet` is a local operator CLI for preparing and running multiple
Equium CPU miners from one checkout. It does not replace `equium-miner`; it
wraps the existing CLI miner with wallet generation, dry-run funding
distribution, process supervision, and agent-readable status output.

## Safety Model

- Generated keypairs live only under `.local/equium-fleet/`.
- `.local/` is ignored by git and must not be shared.
- RPC URLs are read from `--rpc-url` or `EQUIUM_RPC_URL`; do not commit private RPC URLs.
- `wallets distribute` is dry-run by default. It only signs and broadcasts when `--live` is passed.
- The tool prints public keys and balances, not private keys.
- `mine fleet` starts local `equium-miner` child processes and writes local JSONL logs.

Do not send these files to anyone:

```text
.local/equium-fleet/funding.json
.local/equium-fleet/workers/*.json
.local/equium-fleet/manifest.json
```

## Build And Test

```bash
cargo test -p equium-fleet-tools
cargo build -p equium-cli-miner --release
cargo build -p equium-fleet-tools --release
```

The fleet binary is:

```bash
target/release/equium-fleet
```

The miner binary it supervises is:

```bash
target/release/equium-miner
```

## Configure RPC

Set your own Solana RPC in the shell. Each operator should use their own RPC.

```bash
export EQUIUM_RPC_URL='https://YOUR-SOLANA-RPC'
```

You can also pass `--rpc-url` to individual commands.

## Create Wallets

Create a new funding wallet plus worker wallets:

```bash
target/release/equium-fleet wallets create --workers 32
```

This creates:

```text
.local/equium-fleet/
  funding.json
  manifest.json
  workers/
    worker-001.json
    worker-002.json
```

Send SOL manually to the printed funding wallet. Keep enough SOL for worker
balances and future mining transaction fees.

For example, 32 workers at `0.03 SOL` each need `0.96 SOL` before fees. With
the default `--batch-size 8`, distribution is 4 transactions, so the displayed
dry-run will include an estimated fee reserve as well.

## Check Balances

```bash
target/release/equium-fleet wallets status
```

This prints the funding wallet and worker public balances.

## Distribute SOL

Always dry-run first:

```bash
target/release/equium-fleet wallets distribute --per-worker-sol 0.03 --batch-size 8
```

The dry-run prints:

- selected worker top-ups
- planned transaction batches
- funding wallet balance
- estimated fee reserve
- total required balance
- funding shortfall, if any

Broadcast only when the dry-run is correct:

```bash
target/release/equium-fleet wallets distribute --per-worker-sol 0.03 --batch-size 8 --live
```

`--live` refuses to broadcast if the funding wallet does not cover the planned
worker top-ups plus the estimated fee reserve.

## Run Miners

Run every worker:

```bash
target/release/equium-fleet mine fleet --workers all --max-restarts 2 --stagger-ms 250
```

`mine fleet` starts a local monitor automatically. The monitor reads the
current run's JSONL logs and prints periodic aggregate lines like:

```text
[monitor] workers=14/14 seen=14 exited=0 rounds=120 mined=1 errors=0 restarts=0 last_event=3s_ago logs=.local/equium-fleet/runs/run-...
```

The default monitor interval is 30 seconds. Tune or disable it with:

```bash
target/release/equium-fleet mine fleet --workers all --monitor-interval-secs 10
target/release/equium-fleet mine fleet --workers all --monitor-interval-secs 0
```

Run the first 8 workers:

```bash
target/release/equium-fleet mine fleet --workers 8 --max-restarts 2 --stagger-ms 250
```

Run an explicit subset:

```bash
target/release/equium-fleet mine fleet --workers 2-4,worker-008 --max-restarts 2 --stagger-ms 250
```

Worker selection accepts:

- `all`
- `N` for the first N workers
- `A-B` ranges, such as `2-8`
- explicit names, such as `worker-012`
- comma-separated combinations, such as `2-4,worker-008`

Supervisor options:

- `--max-restarts N` restarts a failed miner process up to N times.
- `--restart-delay-ms N` waits before restarting; default is `1000`.
- `--stagger-ms N` delays each worker start by `N * worker_position` to reduce RPC startup spikes.
- `--monitor-interval-secs N` prints aggregate mining status every N seconds; default is `30`, and `0` disables it.

Logs are written under:

```text
.local/equium-fleet/runs/<run-id>/<worker-name>.jsonl
```

Each worker log contains miner stdout/stderr events plus supervisor events for
stagger, start, exit, and restart.

## Agent-Friendly Status

Use this when another coding agent needs a stable, machine-readable view of the
fleet state and the next safe action:

```bash
target/release/equium-fleet agent status --per-worker-sol 0.03
```

The command prints JSON with:

- `manifest_exists`
- `rpc_configured`
- `miner_bin_exists`
- funding and worker balances when RPC is configured
- dry-run distribution plan
- `next_actions`

When the fleet is ready to mine, `next_actions` recommends a supervised mining
command with restart and stagger defaults.

## Prompt For Another Agent

Send this prompt to a friend's agent when you want an independent source review
before they run the tool:

```text
Please review this Equium fleet CLI source before I run it.

Scope:
- Focus on clients/fleet-tools/src/main.rs
- Focus on clients/fleet-tools/src/lib.rs
- Review clients/fleet-tools/README.md
- Review Cargo.toml and Cargo.lock changes that add the fleet-tools crate
- Review .gitignore and confirm .local/ is ignored

Security checks:
1. Confirm generated keypairs stay under .local/equium-fleet/.
2. Confirm private key files are not printed, uploaded, or committed.
3. Confirm RPC URLs are only passed by --rpc-url or EQUIUM_RPC_URL and are not hardcoded.
4. Confirm wallets distribute is dry-run by default.
5. Confirm transactions are only signed and broadcast when --live is passed.
6. Confirm mine fleet only spawns the local equium-miner child process and writes local JSONL logs.
7. Check for hidden network requests, file uploads, shell injection, or reads of unrelated local files.
8. Confirm .local/ and target/ are not part of the committed source.

Verification commands:
cargo test -p equium-fleet-tools
cargo build -p equium-cli-miner --release
cargo build -p equium-fleet-tools --release
target/release/equium-fleet --help
target/release/equium-fleet wallets distribute --help
target/release/equium-fleet mine fleet --help

Please report:
- security findings
- whether it is safe to run
- what still requires manual confirmation
- the exact commands I should run
```

## Minimal Operating Flow

```bash
export EQUIUM_RPC_URL='https://YOUR-SOLANA-RPC'

cargo test -p equium-fleet-tools
cargo build -p equium-cli-miner --release
cargo build -p equium-fleet-tools --release

target/release/equium-fleet wallets create --workers 32
target/release/equium-fleet wallets status
target/release/equium-fleet wallets distribute --per-worker-sol 0.03 --batch-size 8

# Only after manually confirming the dry-run:
target/release/equium-fleet wallets distribute --per-worker-sol 0.03 --batch-size 8 --live

target/release/equium-fleet mine fleet --workers all --max-restarts 2 --stagger-ms 250
```
