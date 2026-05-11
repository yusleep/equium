# Equium Fleet Tool Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a local Equium fleet tool that creates fresh funding/worker wallets, dry-runs and executes SOL distribution, launches multiple CPU miner workers, and records structured logs.

**Architecture:** Add a Rust binary crate under `clients/fleet-tools` with a testable library and a thin CLI. Runtime wallets and logs stay under `.local/equium-fleet/`; source code contains no RPC secrets or key material.

**Tech Stack:** Rust 2021, `clap`, `serde`, `serde_json`, `solana-sdk`, `solana-client`, existing `equium-miner` binary.

---

### Task 1: Workspace And CLI Skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `clients/fleet-tools/Cargo.toml`
- Create: `clients/fleet-tools/src/lib.rs`
- Create: `clients/fleet-tools/src/main.rs`

**Steps:**

1. Write failing tests in `clients/fleet-tools/src/lib.rs` for worker name generation and default fleet directory.
2. Run `cargo test -p equium-fleet-tools worker_names_are_zero_padded default_fleet_dir_is_local`.
3. Add the new crate to the workspace and implement the minimal tested helpers.
4. Run the same tests and confirm they pass.

### Task 2: Wallet Manifest Creation

**Files:**
- Modify: `clients/fleet-tools/src/lib.rs`
- Modify: `clients/fleet-tools/src/main.rs`

**Steps:**

1. Write failing tests for manifest shape with one funding wallet and N worker entries.
2. Implement `wallets create` to generate Solana CLI-compatible keypair JSON files and `manifest.json`.
3. Ensure it refuses to overwrite an existing manifest unless `--force` is provided.
4. Run focused tests, then `cargo test -p equium-fleet-tools`.

### Task 3: Distribution Planning And Status

**Files:**
- Modify: `clients/fleet-tools/src/lib.rs`
- Modify: `clients/fleet-tools/src/main.rs`

**Steps:**

1. Write failing tests for lamport conversion and top-up planning.
2. Implement `wallets status` using `RpcClient::get_balance`.
3. Implement `wallets distribute` dry-run planning.
4. Implement `--live` transfer broadcasting using the generated funding keypair.
5. Run unit tests and compile the binary.

### Task 3b: Distribution Optimization

**Files:**
- Modify: `clients/fleet-tools/src/lib.rs`
- Modify: `clients/fleet-tools/src/main.rs`
- Modify: `clients/fleet-tools/README.md`

**Steps:**

1. Write failing tests for batching planned transfers and checking funding balance against transfer total plus fee reserve.
2. Implement `batch_transfers` and `check_distribution_funding`.
3. Add `wallets distribute --batch-size N`, defaulting to 8.
4. Print planned batches, funding balance, estimated fee reserve, total required balance, and shortfall during dry-run.
5. In live mode, refuse to broadcast if funding is insufficient and send one batched transaction per batch.
6. Run focused tests, then `cargo test -p equium-fleet-tools`.

### Task 4: Multi-Worker Miner Orchestration

**Files:**
- Modify: `clients/fleet-tools/src/lib.rs`
- Modify: `clients/fleet-tools/src/main.rs`

**Steps:**

1. Write failing tests for parsing miner output into structured event JSON.
2. Implement run directory creation and per-worker JSONL logging.
3. Implement `mine fleet` process spawning for all or first N workers.
4. Forward worker output to both terminal and JSONL logs.
5. Run tests and `cargo build -p equium-fleet-tools --release`.

### Task 4b: Supervisor Optimization

**Files:**
- Modify: `clients/fleet-tools/src/lib.rs`
- Modify: `clients/fleet-tools/src/main.rs`
- Modify: `clients/fleet-tools/README.md`

**Steps:**

1. Write failing tests for explicit worker subset selection, restart decisions, and stagger delay calculation.
2. Extend `--workers` to accept selectors such as `2-4,worker-008` while preserving `all` and first-N behavior.
3. Add `--max-restarts`, `--restart-delay-ms`, and `--stagger-ms` to `mine fleet`.
4. Supervise each worker in its own thread, logging supervisor start, exit, restart, and stagger events to that worker JSONL file.
5. Update `agent status` next actions to recommend the supervised mining command.
6. Run focused tests, then `cargo test -p equium-fleet-tools`.

### Task 5: Documentation And Verification

**Files:**
- Modify: `.gitignore`
- Create: `clients/fleet-tools/README.md`

**Steps:**

1. Add `.local/` to `.gitignore`.
2. Document the exact command flow using `EQUIUM_RPC_URL` without hardcoding private RPC values.
3. Run `cargo test -p equium-fleet-tools`.
4. Run `cargo build -p equium-fleet-tools --release`.
5. Run `target/release/equium-fleet --help` and summarize command output.
