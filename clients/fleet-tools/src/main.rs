use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::str::FromStr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use equium_fleet_tools::{
    agent_next_actions, batch_transfers, build_manifest_with_pubkeys, check_distribution_funding,
    classify_miner_line, default_fleet_dir, lamports_to_sol, plan_distribution, select_workers,
    should_restart_worker, sol_to_lamports, summarize_monitor_events, worker_start_delay_ms,
    AgentStatusInput, DistributionPlan, FleetManifest, MinerLogEvent, MonitorLogEvent,
    PlannedTransfer, SupervisorConfig, WalletEntry, WorkerBalance,
};
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, write_keypair_file, Keypair, Signer};
use solana_sdk::transaction::Transaction;
use solana_system_interface::instruction as system_instruction;

const DEFAULT_DISTRIBUTION_BATCH_SIZE: usize = 8;
const DEFAULT_FEE_LAMPORTS_PER_TRANSACTION: u64 = 5_000;

#[derive(Parser)]
#[command(
    name = "equium-fleet",
    version,
    about = "Local Equium fleet operator tooling"
)]
struct Cli {
    #[arg(long, global = true, value_name = "DIR")]
    fleet_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    Wallets {
        #[command(subcommand)]
        command: WalletCommands,
    },
    Mine {
        #[command(subcommand)]
        command: MineCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    Status {
        #[arg(long)]
        rpc_url: Option<String>,
        #[arg(long, value_name = "SOL", default_value = "0.03")]
        per_worker_sol: String,
        #[arg(long, default_value = "target/release/equium-miner")]
        miner_bin: PathBuf,
    },
}

#[derive(Subcommand)]
enum WalletCommands {
    Create {
        #[arg(long, default_value_t = 8)]
        workers: usize,
        #[arg(long)]
        force: bool,
    },
    Status {
        #[arg(long)]
        rpc_url: Option<String>,
    },
    Distribute {
        #[arg(long)]
        rpc_url: Option<String>,
        #[arg(long, value_name = "SOL")]
        per_worker_sol: String,
        #[arg(long, default_value_t = DEFAULT_DISTRIBUTION_BATCH_SIZE)]
        batch_size: usize,
        #[arg(long)]
        live: bool,
    },
}

#[derive(Subcommand)]
enum MineCommands {
    Fleet {
        #[arg(long)]
        rpc_url: Option<String>,
        #[arg(long, default_value = "all")]
        workers: String,
        #[arg(long, default_value = "target/release/equium-miner")]
        miner_bin: PathBuf,
        #[arg(long)]
        max_blocks: Option<u64>,
        #[arg(long, default_value_t = 0)]
        max_restarts: u32,
        #[arg(long, default_value_t = 1_000)]
        restart_delay_ms: u64,
        #[arg(long, default_value_t = 0)]
        stagger_ms: u64,
        #[arg(long, default_value_t = 30)]
        monitor_interval_secs: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct JsonLogLine {
    ts_unix_ms: u128,
    worker: String,
    stream: String,
    kind: String,
    message: String,
}

#[derive(Serialize)]
struct AgentStatusOutput {
    fleet_dir: String,
    manifest_exists: bool,
    rpc_configured: bool,
    rpc_error: Option<String>,
    miner_bin: String,
    miner_bin_exists: bool,
    target_lamports: Option<u64>,
    target_sol: Option<String>,
    funding: Option<AgentWalletOutput>,
    workers: Vec<AgentWalletOutput>,
    distribution: Option<AgentDistributionOutput>,
    next_actions: Vec<String>,
}

#[derive(Serialize)]
struct AgentWalletOutput {
    name: String,
    pubkey: String,
    keypair_path: String,
    lamports: Option<u64>,
    sol: Option<String>,
}

#[derive(Serialize)]
struct AgentDistributionOutput {
    transfer_count: usize,
    total_lamports: u64,
    total_sol: String,
    transfers: Vec<AgentTransferOutput>,
}

#[derive(Serialize)]
struct AgentTransferOutput {
    worker_name: String,
    to_pubkey: String,
    lamports: u64,
    sol: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let fleet_dir = cli.fleet_dir.unwrap_or_else(default_fleet_dir);

    match cli.command {
        Commands::Agent { command } => match command {
            AgentCommands::Status {
                rpc_url,
                per_worker_sol,
                miner_bin,
            } => agent_status(&fleet_dir, rpc_url, &per_worker_sol, &miner_bin),
        },
        Commands::Wallets { command } => match command {
            WalletCommands::Create { workers, force } => create_wallets(&fleet_dir, workers, force),
            WalletCommands::Status { rpc_url } => wallet_status(&fleet_dir, rpc_url),
            WalletCommands::Distribute {
                rpc_url,
                per_worker_sol,
                batch_size,
                live,
            } => distribute(&fleet_dir, rpc_url, &per_worker_sol, batch_size, live),
        },
        Commands::Mine { command } => match command {
            MineCommands::Fleet {
                rpc_url,
                workers,
                miner_bin,
                max_blocks,
                max_restarts,
                restart_delay_ms,
                stagger_ms,
                monitor_interval_secs,
            } => mine_fleet(
                &fleet_dir,
                rpc_url,
                &workers,
                &miner_bin,
                max_blocks,
                SupervisorConfig {
                    max_restarts,
                    restart_delay_ms,
                    stagger_ms,
                },
                monitor_interval_secs,
            ),
        },
    }
}

fn agent_status(
    root: &Path,
    rpc_url: Option<String>,
    per_worker_sol: &str,
    miner_bin: &Path,
) -> Result<()> {
    let manifest_path = root.join("manifest.json");
    let manifest_exists = manifest_path.exists();
    let rpc_url = rpc_url.or_else(|| std::env::var("EQUIUM_RPC_URL").ok());
    let rpc_configured = rpc_url.is_some();
    let target_lamports = sol_to_lamports(per_worker_sol).ok();
    let target_sol = target_lamports.map(lamports_to_sol);
    let miner_bin_exists = miner_bin.exists();

    let mut rpc_error = None;
    let mut funding = None;
    let mut workers = Vec::new();
    let mut distribution = None;

    if manifest_exists {
        let manifest = read_manifest(root)?;
        if let Some(url) = rpc_url {
            let rpc = RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());
            let funding_lamports = match balance_for(&rpc, &manifest.funding.pubkey) {
                Ok(lamports) => Some(lamports),
                Err(e) => {
                    rpc_error = Some(e.to_string());
                    None
                }
            };
            funding = Some(agent_wallet_output(
                &manifest.funding,
                "funding",
                funding_lamports,
            ));

            let mut worker_balances_for_plan = Vec::new();
            for worker in &manifest.workers {
                let lamports = if rpc_error.is_none() {
                    match balance_for(&rpc, &worker.pubkey) {
                        Ok(lamports) => Some(lamports),
                        Err(e) => {
                            rpc_error = Some(e.to_string());
                            None
                        }
                    }
                } else {
                    None
                };
                if let Some(lamports) = lamports {
                    worker_balances_for_plan.push(WorkerBalance::new(
                        worker.name.clone(),
                        worker.pubkey.clone(),
                        lamports,
                    ));
                }
                workers.push(agent_wallet_output(worker, &worker.name, lamports));
            }

            if let Some(target_lamports) = target_lamports {
                if worker_balances_for_plan.len() == manifest.workers.len() {
                    let plan = plan_distribution(&worker_balances_for_plan, target_lamports);
                    distribution = Some(agent_distribution_output(&plan));
                }
            }
        } else {
            funding = Some(agent_wallet_output(&manifest.funding, "funding", None));
            workers = manifest
                .workers
                .iter()
                .map(|worker| agent_wallet_output(worker, &worker.name, None))
                .collect();
        }
    }

    let next_actions = agent_next_actions(&AgentStatusInput {
        manifest_exists,
        rpc_configured,
        miner_bin_exists,
        worker_count: if workers.is_empty() {
            None
        } else {
            Some(workers.len())
        },
        funding_lamports: funding.as_ref().and_then(|wallet| wallet.lamports),
        planned_transfer_count: distribution.as_ref().map(|d| d.transfer_count),
        planned_total_lamports: distribution.as_ref().map(|d| d.total_lamports),
    });

    let output = AgentStatusOutput {
        fleet_dir: root.display().to_string(),
        manifest_exists,
        rpc_configured,
        rpc_error,
        miner_bin: miner_bin.display().to_string(),
        miner_bin_exists,
        target_lamports,
        target_sol,
        funding,
        workers,
        distribution,
        next_actions,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn agent_wallet_output(
    wallet: &WalletEntry,
    default_name: &str,
    lamports: Option<u64>,
) -> AgentWalletOutput {
    AgentWalletOutput {
        name: if wallet.name.is_empty() {
            default_name.to_string()
        } else {
            wallet.name.clone()
        },
        pubkey: wallet.pubkey.clone(),
        keypair_path: wallet.keypair_path.clone(),
        lamports,
        sol: lamports.map(lamports_to_sol),
    }
}

fn agent_distribution_output(plan: &DistributionPlan) -> AgentDistributionOutput {
    AgentDistributionOutput {
        transfer_count: plan.transfers.len(),
        total_lamports: plan.total_lamports,
        total_sol: lamports_to_sol(plan.total_lamports),
        transfers: plan.transfers.iter().map(agent_transfer_output).collect(),
    }
}

fn agent_transfer_output(transfer: &PlannedTransfer) -> AgentTransferOutput {
    AgentTransferOutput {
        worker_name: transfer.worker_name.clone(),
        to_pubkey: transfer.to_pubkey.clone(),
        lamports: transfer.lamports,
        sol: lamports_to_sol(transfer.lamports),
    }
}

fn create_wallets(root: &Path, workers: usize, force: bool) -> Result<()> {
    if workers == 0 {
        bail!("--workers must be greater than zero");
    }
    let manifest_path = root.join("manifest.json");
    if manifest_path.exists() && !force {
        bail!(
            "manifest already exists at {}; pass --force to overwrite",
            manifest_path.display()
        );
    }

    fs::create_dir_all(root.join("workers"))
        .with_context(|| format!("create {}", root.join("workers").display()))?;

    let funding = Keypair::new();
    write_keypair(&funding, &root.join("funding.json"))?;

    let mut worker_pubkeys = Vec::with_capacity(workers);
    for index in 1..=workers {
        let name = equium_fleet_tools::worker_name(index);
        let worker = Keypair::new();
        worker_pubkeys.push(worker.pubkey().to_string());
        write_keypair(&worker, &root.join("workers").join(format!("{name}.json")))?;
    }

    let manifest = build_manifest_with_pubkeys(funding.pubkey().to_string(), worker_pubkeys)
        .map_err(|e| anyhow!(e))?;
    write_manifest(root, &manifest)?;

    println!("created fleet at {}", root.display());
    println!("funding wallet: {}", manifest.funding.pubkey);
    for worker in &manifest.workers {
        println!("{}: {}", worker.name, worker.pubkey);
    }
    Ok(())
}

fn wallet_status(root: &Path, rpc_url: Option<String>) -> Result<()> {
    let rpc = rpc_client(rpc_url)?;
    let manifest = read_manifest(root)?;

    let funding_balance = balance_for(&rpc, &manifest.funding.pubkey)
        .with_context(|| format!("fetch funding balance {}", manifest.funding.pubkey))?;
    println!("{:<12} {:<44} {:>14}", "name", "pubkey", "SOL");
    println!(
        "{:<12} {:<44} {:>14}",
        "funding",
        manifest.funding.pubkey,
        lamports_to_sol(funding_balance)
    );
    for worker in worker_balances(&rpc, &manifest)? {
        println!(
            "{:<12} {:<44} {:>14}",
            worker.worker_name,
            worker.pubkey,
            lamports_to_sol(worker.lamports)
        );
    }
    Ok(())
}

fn distribute(
    root: &Path,
    rpc_url: Option<String>,
    per_worker_sol: &str,
    batch_size: usize,
    live: bool,
) -> Result<()> {
    let target_lamports = sol_to_lamports(per_worker_sol).map_err(|e| anyhow!(e))?;
    if target_lamports == 0 {
        bail!("--per-worker-sol must be greater than zero");
    }
    if batch_size == 0 {
        bail!("--batch-size must be greater than zero");
    }

    let rpc = rpc_client(rpc_url)?;
    let manifest = read_manifest(root)?;
    let funding_balance = balance_for(&rpc, &manifest.funding.pubkey)
        .with_context(|| format!("fetch funding balance {}", manifest.funding.pubkey))?;
    let workers = worker_balances(&rpc, &manifest)?;
    let plan = plan_distribution(&workers, target_lamports);
    let funding_check = check_distribution_funding(
        funding_balance,
        plan.total_lamports,
        plan.transfers.len(),
        batch_size,
        DEFAULT_FEE_LAMPORTS_PER_TRANSACTION,
    )
    .map_err(|e| anyhow!(e))?;
    let batches = batch_transfers(&plan.transfers, batch_size).map_err(|e| anyhow!(e))?;

    println!(
        "target per worker: {} SOL",
        lamports_to_sol(target_lamports)
    );
    println!(
        "planned transfers: {} workers, {} SOL total",
        plan.transfers.len(),
        lamports_to_sol(plan.total_lamports)
    );
    println!(
        "planned batches: {} transactions, batch size {}",
        funding_check.transaction_count, batch_size
    );
    println!("funding balance: {} SOL", lamports_to_sol(funding_balance));
    println!(
        "estimated fee reserve: {} SOL",
        lamports_to_sol(funding_check.fee_lamports)
    );
    println!(
        "required including fee reserve: {} SOL",
        lamports_to_sol(funding_check.required_lamports)
    );
    if funding_check.shortfall_lamports > 0 {
        println!(
            "funding shortfall: {} SOL",
            lamports_to_sol(funding_check.shortfall_lamports)
        );
    }
    for transfer in &plan.transfers {
        println!(
            "{} -> {} SOL ({})",
            transfer.worker_name,
            lamports_to_sol(transfer.lamports),
            transfer.to_pubkey
        );
    }

    if !live {
        println!("dry-run only; re-run with --live to broadcast");
        return Ok(());
    }

    if plan.transfers.is_empty() {
        println!("nothing to distribute");
        return Ok(());
    }

    if !funding_check.has_sufficient_funding {
        bail!(
            "funding wallet is short by {} SOL",
            lamports_to_sol(funding_check.shortfall_lamports)
        );
    }

    let funding = read_keypair(root.join(&manifest.funding.keypair_path))?;
    for (index, batch) in batches.iter().enumerate() {
        let mut instructions = Vec::with_capacity(batch.len());
        for transfer in batch {
            let to = Pubkey::from_str(&transfer.to_pubkey)
                .with_context(|| format!("parse worker pubkey {}", transfer.to_pubkey))?;
            instructions.push(system_instruction::transfer(
                &funding.pubkey(),
                &to,
                transfer.lamports,
            ));
        }
        let recent = rpc.get_latest_blockhash()?;
        let tx = Transaction::new_signed_with_payer(
            &instructions,
            Some(&funding.pubkey()),
            &[&funding],
            recent,
        );
        let sig = rpc.send_and_confirm_transaction(&tx)?;
        println!(
            "sent batch {}/{} ({} workers): {}",
            index + 1,
            batches.len(),
            batch.len(),
            sig
        );
    }

    Ok(())
}

fn mine_fleet(
    root: &Path,
    rpc_url: Option<String>,
    workers_selector: &str,
    miner_bin: &Path,
    max_blocks: Option<u64>,
    supervisor: SupervisorConfig,
    monitor_interval_secs: u64,
) -> Result<()> {
    let rpc_url = rpc_url_value(rpc_url)?;
    let manifest = read_manifest(root)?;
    let workers = select_workers(&manifest.workers, workers_selector).map_err(|e| anyhow!(e))?;
    if workers.is_empty() {
        bail!("no workers selected");
    }
    if !miner_bin.exists() {
        bail!("miner binary not found at {}", miner_bin.display());
    }

    let run_dir = root.join("runs").join(format!("run-{}", now_unix_secs()));
    fs::create_dir_all(&run_dir).with_context(|| format!("create {}", run_dir.display()))?;
    println!("run logs: {}", run_dir.display());

    let worker_count = workers.len();
    let monitor = start_monitor(&run_dir, worker_count, monitor_interval_secs);

    let mut failed = false;
    let mut handles = Vec::new();
    for (position, worker) in workers.into_iter().enumerate() {
        let root = root.to_path_buf();
        let run_dir = run_dir.clone();
        let rpc_url = rpc_url.clone();
        let miner_bin = miner_bin.to_path_buf();
        handles.push(thread::spawn(move || {
            supervise_worker(
                &root, &run_dir, worker, position, rpc_url, miner_bin, max_blocks, supervisor,
            )
        }));
    }

    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                failed = true;
                eprintln!("{e:#}");
            }
            Err(_) => {
                failed = true;
                eprintln!("worker supervisor panicked");
            }
        }
    }
    let _ = print_monitor_summary(&run_dir, worker_count);
    stop_monitor(monitor);
    if failed {
        bail!("one or more miners exited unsuccessfully");
    }
    Ok(())
}

struct MonitorHandle {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

fn start_monitor(
    run_dir: &Path,
    expected_workers: usize,
    interval_secs: u64,
) -> Option<MonitorHandle> {
    if interval_secs == 0 {
        return None;
    }

    let run_dir = run_dir.to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            if let Err(e) = print_monitor_summary(&run_dir, expected_workers) {
                eprintln!("[monitor] read {}: {e:#}", run_dir.display());
            }
            sleep_monitor_interval(interval_secs, &thread_stop);
        }
    });

    Some(MonitorHandle { stop, handle })
}

fn stop_monitor(monitor: Option<MonitorHandle>) {
    if let Some(monitor) = monitor {
        monitor.stop.store(true, Ordering::Relaxed);
        let _ = monitor.handle.join();
    }
}

fn print_monitor_summary(run_dir: &Path, expected_workers: usize) -> Result<()> {
    let events = read_monitor_events(run_dir)?;
    let summary = summarize_monitor_events(&events, expected_workers);
    println!(
        "[monitor] workers={}/{} seen={} exited={} failed={} rounds={} mined={} errors={} restarts={} last_event={} logs={}",
        summary.running_workers,
        summary.expected_workers,
        summary.seen_workers,
        summary.exited_workers,
        summary.failed_workers,
        summary.rounds,
        summary.mined,
        summary.errors,
        summary.restarts,
        format_last_event_age(summary.last_event_unix_ms),
        run_dir.display(),
    );
    Ok(())
}

fn sleep_monitor_interval(interval_secs: u64, stop: &AtomicBool) {
    let mut slept = 0u64;
    while slept < interval_secs && !stop.load(Ordering::Relaxed) {
        let remaining = interval_secs - slept;
        let step = remaining.min(1);
        thread::sleep(Duration::from_secs(step));
        slept += step;
    }
}

fn read_monitor_events(run_dir: &Path) -> Result<Vec<MonitorLogEvent>> {
    let mut events = Vec::new();
    if !run_dir.exists() {
        return Ok(events);
    }

    for entry in fs::read_dir(run_dir).with_context(|| format!("read {}", run_dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(log) = serde_json::from_str::<JsonLogLine>(line) else {
                continue;
            };
            events.push(MonitorLogEvent::new(
                log.ts_unix_ms,
                log.worker,
                log.stream,
                log.kind,
            ));
        }
    }

    Ok(events)
}

fn format_last_event_age(last_event_unix_ms: Option<u128>) -> String {
    match last_event_unix_ms {
        Some(last) => {
            let age_ms = now_unix_ms().saturating_sub(last);
            format!("{}s_ago", age_ms / 1_000)
        }
        None => "none".to_string(),
    }
}

fn supervise_worker(
    root: &Path,
    run_dir: &Path,
    worker: WalletEntry,
    position: usize,
    rpc_url: String,
    miner_bin: PathBuf,
    max_blocks: Option<u64>,
    supervisor: SupervisorConfig,
) -> Result<()> {
    let log_path = run_dir.join(format!("{}.jsonl", worker.name));
    let start_delay_ms = worker_start_delay_ms(position, &supervisor);
    if start_delay_ms > 0 {
        write_supervisor_event(
            &log_path,
            &worker.name,
            "stagger",
            &format!("waiting {start_delay_ms}ms before start"),
        )?;
        thread::sleep(Duration::from_millis(start_delay_ms));
    }

    let mut restarts = 0u32;
    loop {
        let attempt = restarts + 1;
        write_supervisor_event(
            &log_path,
            &worker.name,
            "start",
            &format!("starting miner attempt {attempt}"),
        )?;
        let status = run_worker_once(root, run_dir, &worker, &rpc_url, &miner_bin, max_blocks)?;
        let success = status.success();
        write_supervisor_event(
            &log_path,
            &worker.name,
            if success { "exit" } else { "error" },
            &format!("miner exited with {status}"),
        )?;

        if success {
            return Ok(());
        }
        if should_restart_worker(success, restarts, &supervisor) {
            restarts += 1;
            write_supervisor_event(
                &log_path,
                &worker.name,
                "restart",
                &format!(
                    "restarting after {}ms ({restarts}/{})",
                    supervisor.restart_delay_ms, supervisor.max_restarts
                ),
            )?;
            thread::sleep(Duration::from_millis(supervisor.restart_delay_ms));
            continue;
        }

        bail!(
            "{} exited with {} after {} restarts",
            worker.name,
            status,
            restarts
        );
    }
}

fn run_worker_once(
    root: &Path,
    run_dir: &Path,
    worker: &WalletEntry,
    rpc_url: &str,
    miner_bin: &Path,
    max_blocks: Option<u64>,
) -> Result<ExitStatus> {
    let keypair_path = root.join(&worker.keypair_path);
    let log_path = run_dir.join(format!("{}.jsonl", worker.name));
    let mut command = Command::new(miner_bin);
    command
        .arg("--rpc-url")
        .arg(rpc_url)
        .arg("--keypair")
        .arg(&keypair_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(max_blocks) = max_blocks {
        command.arg("--max-blocks").arg(max_blocks.to_string());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {} for {}", miner_bin.display(), worker.name))?;
    let stdout = attach_log_reader(&mut child, &worker.name, "stdout", &log_path)?;
    let stderr = attach_log_reader(&mut child, &worker.name, "stderr", &log_path)?;
    let status = child
        .wait()
        .with_context(|| format!("wait for {}", worker.name))?;
    let _ = stdout.join();
    let _ = stderr.join();
    Ok(status)
}

fn attach_log_reader(
    child: &mut Child,
    worker: &str,
    stream: &str,
    log_path: &Path,
) -> Result<JoinHandle<()>> {
    let reader: Box<dyn BufRead + Send> = match stream {
        "stdout" => Box::new(BufReader::new(
            child.stdout.take().context("child stdout already taken")?,
        )),
        "stderr" => Box::new(BufReader::new(
            child.stderr.take().context("child stderr already taken")?,
        )),
        _ => bail!("unsupported stream {stream}"),
    };
    let worker = worker.to_string();
    let stream = stream.to_string();
    let log_path = log_path.to_path_buf();

    Ok(thread::spawn(move || {
        let mut file = match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(file) => file,
            Err(e) => {
                eprintln!("open log {}: {e}", log_path.display());
                return;
            }
        };
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            println!("[{worker}] {line}");
            let MinerLogEvent { kind, message } = classify_miner_line(&line);
            let event = JsonLogLine {
                ts_unix_ms: now_unix_ms(),
                worker: worker.clone(),
                stream: stream.clone(),
                kind,
                message,
            };
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = writeln!(file, "{json}");
            }
        }
    }))
}

fn write_supervisor_event(log_path: &Path, worker: &str, kind: &str, message: &str) -> Result<()> {
    println!("[{worker}] supervisor: {message}");
    let event = JsonLogLine {
        ts_unix_ms: now_unix_ms(),
        worker: worker.to_string(),
        stream: "supervisor".to_string(),
        kind: kind.to_string(),
        message: message.to_string(),
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("open log {}", log_path.display()))?;
    writeln!(file, "{}", serde_json::to_string(&event)?)
        .with_context(|| format!("write log {}", log_path.display()))
}

fn rpc_client(rpc_url: Option<String>) -> Result<RpcClient> {
    Ok(RpcClient::new_with_commitment(
        rpc_url_value(rpc_url)?,
        CommitmentConfig::confirmed(),
    ))
}

fn rpc_url_value(rpc_url: Option<String>) -> Result<String> {
    if let Some(url) = rpc_url {
        return Ok(url);
    }
    std::env::var("EQUIUM_RPC_URL").map_err(|_| anyhow!("provide --rpc-url or set EQUIUM_RPC_URL"))
}

fn read_manifest(root: &Path) -> Result<FleetManifest> {
    let path = root.join("manifest.json");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn write_manifest(root: &Path, manifest: &FleetManifest) -> Result<()> {
    let path = root.join("manifest.json");
    let raw = serde_json::to_string_pretty(manifest)?;
    fs::write(&path, raw).with_context(|| format!("write {}", path.display()))
}

fn write_keypair(keypair: &Keypair, path: &Path) -> Result<()> {
    write_keypair_file(keypair, path)
        .map(|_| ())
        .map_err(|e| anyhow!("write keypair {}: {}", path.display(), e))
}

fn read_keypair(path: impl AsRef<Path>) -> Result<Keypair> {
    let path = path.as_ref();
    read_keypair_file(path).map_err(|e| anyhow!("read keypair {}: {}", path.display(), e))
}

fn balance_for(rpc: &RpcClient, pubkey: &str) -> Result<u64> {
    let pubkey = Pubkey::from_str(pubkey).with_context(|| format!("parse pubkey {pubkey}"))?;
    rpc.get_balance(&pubkey).map_err(Into::into)
}

fn worker_balances(rpc: &RpcClient, manifest: &FleetManifest) -> Result<Vec<WorkerBalance>> {
    manifest
        .workers
        .iter()
        .map(|worker| {
            let lamports = balance_for(rpc, &worker.pubkey)
                .with_context(|| format!("fetch worker balance {}", worker.name))?;
            Ok(WorkerBalance::new(
                worker.name.clone(),
                worker.pubkey.clone(),
                lamports,
            ))
        })
        .collect()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
