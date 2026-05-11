use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletEntry {
    pub name: String,
    pub pubkey: String,
    pub keypair_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetManifest {
    pub version: u32,
    pub funding: WalletEntry,
    pub workers: Vec<WalletEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerBalance {
    pub worker_name: String,
    pub pubkey: String,
    pub lamports: u64,
}

impl WorkerBalance {
    pub fn new(worker_name: impl Into<String>, pubkey: impl Into<String>, lamports: u64) -> Self {
        Self {
            worker_name: worker_name.into(),
            pubkey: pubkey.into(),
            lamports,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTransfer {
    pub worker_name: String,
    pub to_pubkey: String,
    pub lamports: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionPlan {
    pub transfers: Vec<PlannedTransfer>,
    pub total_lamports: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributionFundingCheck {
    pub transaction_count: usize,
    pub fee_lamports: u64,
    pub required_lamports: u64,
    pub shortfall_lamports: u64,
    pub has_sufficient_funding: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinerLogEvent {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorLogEvent {
    pub ts_unix_ms: u128,
    pub worker: String,
    pub stream: String,
    pub kind: String,
}

impl MonitorLogEvent {
    pub fn new(
        ts_unix_ms: u128,
        worker: impl Into<String>,
        stream: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            ts_unix_ms,
            worker: worker.into(),
            stream: stream.into(),
            kind: kind.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorSummary {
    pub expected_workers: usize,
    pub seen_workers: usize,
    pub running_workers: usize,
    pub exited_workers: usize,
    pub failed_workers: usize,
    pub rounds: usize,
    pub mined: usize,
    pub errors: usize,
    pub restarts: usize,
    pub last_event_unix_ms: Option<u128>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorConfig {
    pub max_restarts: u32,
    pub restart_delay_ms: u64,
    pub stagger_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusInput {
    pub manifest_exists: bool,
    pub rpc_configured: bool,
    pub miner_bin_exists: bool,
    pub worker_count: Option<usize>,
    pub funding_lamports: Option<u64>,
    pub planned_transfer_count: Option<usize>,
    pub planned_total_lamports: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchStatus {
    ConfigMissing,
    MiningClosed { block_height: u64 },
    MiningOpen { block_height: u64 },
}

impl LaunchStatus {
    pub fn should_start_mining(&self) -> bool {
        matches!(self, LaunchStatus::MiningOpen { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            LaunchStatus::ConfigMissing => "config missing".to_string(),
            LaunchStatus::MiningClosed { block_height } => {
                format!("mining closed at block #{block_height}")
            }
            LaunchStatus::MiningOpen { block_height } => {
                format!("mining open at block #{block_height}")
            }
        }
    }
}

pub fn worker_name(index: usize) -> String {
    format!("worker-{index:03}")
}

pub fn default_fleet_dir() -> PathBuf {
    PathBuf::from(".local/equium-fleet")
}

pub fn build_manifest(worker_count: usize) -> FleetManifest {
    let workers = (1..=worker_count)
        .map(|index| {
            let name = worker_name(index);
            WalletEntry {
                keypair_path: format!("workers/{name}.json"),
                name,
                pubkey: String::new(),
            }
        })
        .collect();

    FleetManifest {
        version: 1,
        funding: WalletEntry {
            name: "funding".to_string(),
            pubkey: String::new(),
            keypair_path: "funding.json".to_string(),
        },
        workers,
    }
}

pub fn build_manifest_with_pubkeys(
    funding_pubkey: String,
    worker_pubkeys: Vec<String>,
) -> Result<FleetManifest, String> {
    let mut manifest = build_manifest(worker_pubkeys.len());
    manifest.funding.pubkey = funding_pubkey;
    for (worker, pubkey) in manifest.workers.iter_mut().zip(worker_pubkeys) {
        worker.pubkey = pubkey;
    }
    Ok(manifest)
}

pub fn plan_distribution(workers: &[WorkerBalance], target_lamports: u64) -> DistributionPlan {
    let mut transfers = Vec::new();
    let mut total_lamports = 0u64;

    for worker in workers {
        if worker.lamports >= target_lamports {
            continue;
        }
        let lamports = target_lamports - worker.lamports;
        total_lamports = total_lamports.saturating_add(lamports);
        transfers.push(PlannedTransfer {
            worker_name: worker.worker_name.clone(),
            to_pubkey: worker.pubkey.clone(),
            lamports,
        });
    }

    DistributionPlan {
        transfers,
        total_lamports,
    }
}

pub fn batch_transfers(
    transfers: &[PlannedTransfer],
    batch_size: usize,
) -> Result<Vec<Vec<PlannedTransfer>>, String> {
    if batch_size == 0 {
        return Err("batch size must be greater than zero".to_string());
    }

    Ok(transfers
        .chunks(batch_size)
        .map(|chunk| chunk.to_vec())
        .collect())
}

pub fn check_distribution_funding(
    funding_lamports: u64,
    transfer_lamports: u64,
    transfer_count: usize,
    batch_size: usize,
    fee_lamports_per_transaction: u64,
) -> Result<DistributionFundingCheck, String> {
    if batch_size == 0 {
        return Err("batch size must be greater than zero".to_string());
    }

    let transaction_count = if transfer_count == 0 {
        0
    } else {
        transfer_count.div_ceil(batch_size)
    };
    let fee_lamports = (transaction_count as u64)
        .checked_mul(fee_lamports_per_transaction)
        .ok_or_else(|| "fee reserve overflows u64 lamports".to_string())?;
    let required_lamports = transfer_lamports
        .checked_add(fee_lamports)
        .ok_or_else(|| "distribution requirement overflows u64 lamports".to_string())?;
    let shortfall_lamports = required_lamports.saturating_sub(funding_lamports);

    Ok(DistributionFundingCheck {
        transaction_count,
        fee_lamports,
        required_lamports,
        shortfall_lamports,
        has_sufficient_funding: shortfall_lamports == 0,
    })
}

pub fn sol_to_lamports(input: &str) -> Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err("SOL amount must be a non-negative decimal".to_string());
    }

    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() > 2 {
        return Err("SOL amount has too many decimal points".to_string());
    }

    let whole = if parts[0].is_empty() {
        0
    } else {
        parts[0]
            .parse::<u64>()
            .map_err(|_| "invalid SOL whole amount".to_string())?
    };

    let frac = if parts.len() == 2 {
        let raw = parts[1];
        if raw.len() > 9 {
            return Err("SOL amount has more than 9 decimal places".to_string());
        }
        if !raw.chars().all(|c| c.is_ascii_digit()) {
            return Err("invalid SOL fractional amount".to_string());
        }
        let mut padded = raw.to_string();
        while padded.len() < 9 {
            padded.push('0');
        }
        if padded.is_empty() {
            0
        } else {
            padded
                .parse::<u64>()
                .map_err(|_| "invalid SOL fractional amount".to_string())?
        }
    } else {
        0
    };

    whole
        .checked_mul(LAMPORTS_PER_SOL)
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(|| "SOL amount overflows u64 lamports".to_string())
}

pub fn lamports_to_sol(lamports: u64) -> String {
    let whole = lamports / LAMPORTS_PER_SOL;
    let frac = lamports % LAMPORTS_PER_SOL;
    if frac == 0 {
        whole.to_string()
    } else {
        format!("{}.{:09}", whole, frac)
            .trim_end_matches('0')
            .to_string()
    }
}

pub fn classify_miner_line(line: &str) -> MinerLogEvent {
    let trimmed = line.trim();
    let kind = if trimmed.contains("MINED!") {
        "mined"
    } else if trimmed.starts_with("round #") {
        "round"
    } else if trimmed.contains("failed") || trimmed.contains("error") {
        "error"
    } else {
        "info"
    };

    MinerLogEvent {
        kind: kind.to_string(),
        message: trimmed.to_string(),
    }
}

pub fn summarize_monitor_events(
    events: &[MonitorLogEvent],
    expected_workers: usize,
) -> MonitorSummary {
    let mut seen_workers = HashSet::new();
    let mut worker_last_kind = HashMap::new();
    let mut rounds = 0usize;
    let mut mined = 0usize;
    let mut errors = 0usize;
    let mut restarts = 0usize;
    let mut last_event_unix_ms = None;

    for event in events {
        seen_workers.insert(event.worker.clone());
        worker_last_kind.insert(event.worker.clone(), event.kind.clone());
        last_event_unix_ms = Some(last_event_unix_ms.map_or(event.ts_unix_ms, |last| {
            if event.ts_unix_ms > last {
                event.ts_unix_ms
            } else {
                last
            }
        }));

        match event.kind.as_str() {
            "round" => rounds += 1,
            "mined" => mined += 1,
            "error" => errors += 1,
            "restart" => restarts += 1,
            _ => {}
        }
    }

    let seen_workers_count = seen_workers.len();
    let exited_workers_count = worker_last_kind
        .values()
        .filter(|kind| kind.as_str() == "exit")
        .count();
    let failed_workers_count = worker_last_kind
        .values()
        .filter(|kind| kind.as_str() == "error")
        .count();
    MonitorSummary {
        expected_workers,
        seen_workers: seen_workers_count,
        running_workers: seen_workers_count
            .saturating_sub(exited_workers_count)
            .saturating_sub(failed_workers_count),
        exited_workers: exited_workers_count,
        failed_workers: failed_workers_count,
        rounds,
        mined,
        errors,
        restarts,
        last_event_unix_ms,
    }
}

pub fn select_workers(workers: &[WalletEntry], selector: &str) -> Result<Vec<WalletEntry>, String> {
    let trimmed = selector.trim();
    if trimmed == "all" {
        return Ok(workers.to_vec());
    }

    if trimmed.contains(',') || trimmed.contains('-') || trimmed.starts_with("worker-") {
        return select_worker_set(workers, trimmed);
    }

    let count = trimmed
        .parse::<usize>()
        .map_err(|_| "workers must be 'all' or a positive integer".to_string())?;
    if count == 0 {
        return Err("worker count must be greater than zero".to_string());
    }
    if count > workers.len() {
        return Err(format!(
            "requested {count} workers, but manifest only has {}",
            workers.len()
        ));
    }
    Ok(workers.iter().take(count).cloned().collect())
}

fn select_worker_set(workers: &[WalletEntry], selector: &str) -> Result<Vec<WalletEntry>, String> {
    let mut selected = Vec::new();
    let mut seen = HashSet::new();

    for token in selector.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err("worker selector contains an empty token".to_string());
        }

        if let Some((start, end)) = token.split_once('-') {
            if start.starts_with("worker") || end.starts_with("worker") {
                push_worker_by_name(workers, token, &mut selected, &mut seen)?;
                continue;
            }
            let start = parse_worker_index(start)?;
            let end = parse_worker_index(end)?;
            if start > end {
                return Err(format!(
                    "invalid worker range {token}: start is greater than end"
                ));
            }
            for index in start..=end {
                let name = worker_name(index);
                push_worker_by_name(workers, &name, &mut selected, &mut seen)?;
            }
        } else if token.starts_with("worker-") {
            push_worker_by_name(workers, token, &mut selected, &mut seen)?;
        } else {
            let index = parse_worker_index(token)?;
            let name = worker_name(index);
            push_worker_by_name(workers, &name, &mut selected, &mut seen)?;
        }
    }

    Ok(selected)
}

fn parse_worker_index(input: &str) -> Result<usize, String> {
    let index = input
        .parse::<usize>()
        .map_err(|_| format!("invalid worker selector token {input}"))?;
    if index == 0 {
        return Err("worker indexes are 1-based".to_string());
    }
    Ok(index)
}

fn push_worker_by_name(
    workers: &[WalletEntry],
    name: &str,
    selected: &mut Vec<WalletEntry>,
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    let worker = workers
        .iter()
        .find(|worker| worker.name == name)
        .ok_or_else(|| format!("worker {name} is not in the manifest"))?;

    if seen.insert(worker.name.clone()) {
        selected.push(worker.clone());
    }
    Ok(())
}

pub fn should_restart_worker(
    exit_success: bool,
    restarts_so_far: u32,
    config: &SupervisorConfig,
) -> bool {
    !exit_success && restarts_so_far < config.max_restarts
}

pub fn worker_start_delay_ms(worker_position: usize, config: &SupervisorConfig) -> u64 {
    (worker_position as u64).saturating_mul(config.stagger_ms)
}

pub fn agent_next_actions(input: &AgentStatusInput) -> Vec<String> {
    let mut actions = Vec::new();
    if !input.manifest_exists {
        actions.push("run: equium-fleet wallets create --workers 8".to_string());
        return actions;
    }
    if !input.rpc_configured {
        actions.push("set EQUIUM_RPC_URL or pass --rpc-url".to_string());
    }
    if !input.miner_bin_exists {
        actions.push("run: cargo build -p equium-cli-miner --release".to_string());
    }
    if input.funding_lamports == Some(0) {
        actions.push("fund the printed funding wallet with SOL".to_string());
    }
    if input.planned_transfer_count.unwrap_or(0) > 0 {
        actions.push("dry-run distribution, then rerun with --live when correct".to_string());
    }
    if input.worker_count.unwrap_or(0) > 0
        && input.rpc_configured
        && input.miner_bin_exists
        && input.planned_transfer_count == Some(0)
    {
        actions.push(
            "run: equium-fleet mine fleet --workers all --max-restarts 2 --stagger-ms 250"
                .to_string(),
        );
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_names_are_zero_padded() {
        assert_eq!(worker_name(1), "worker-001");
        assert_eq!(worker_name(12), "worker-012");
        assert_eq!(worker_name(123), "worker-123");
    }

    #[test]
    fn default_fleet_dir_is_local() {
        assert_eq!(default_fleet_dir().to_string_lossy(), ".local/equium-fleet");
    }

    #[test]
    fn manifest_uses_expected_relative_paths() {
        let manifest = build_manifest(3);

        assert_eq!(manifest.funding.keypair_path, "funding.json");
        assert_eq!(manifest.workers.len(), 3);
        assert_eq!(manifest.workers[0].name, "worker-001");
        assert_eq!(manifest.workers[0].keypair_path, "workers/worker-001.json");
        assert_eq!(manifest.workers[2].name, "worker-003");
        assert_eq!(manifest.workers[2].keypair_path, "workers/worker-003.json");
    }

    #[test]
    fn manifest_pubkeys_are_attached_in_order() {
        let manifest = build_manifest_with_pubkeys(
            "funding-pubkey".to_string(),
            vec!["worker-a".to_string(), "worker-b".to_string()],
        )
        .unwrap();

        assert_eq!(manifest.funding.pubkey, "funding-pubkey");
        assert_eq!(manifest.workers[0].pubkey, "worker-a");
        assert_eq!(manifest.workers[1].pubkey, "worker-b");
    }

    #[test]
    fn distribution_plan_tops_up_workers_to_target() {
        let workers = vec![
            WorkerBalance::new("worker-001", "111", 5_000_000),
            WorkerBalance::new("worker-002", "222", 40_000_000),
            WorkerBalance::new("worker-003", "333", 30_000_000),
        ];

        let plan = plan_distribution(&workers, 30_000_000);

        assert_eq!(plan.total_lamports, 25_000_000);
        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].worker_name, "worker-001");
        assert_eq!(plan.transfers[0].to_pubkey, "111");
        assert_eq!(plan.transfers[0].lamports, 25_000_000);
    }

    #[test]
    fn distribution_batches_and_funding_check_include_fee_reserve() {
        let transfers = vec![
            PlannedTransfer {
                worker_name: "worker-001".to_string(),
                to_pubkey: "111".to_string(),
                lamports: 10_000_000,
            },
            PlannedTransfer {
                worker_name: "worker-002".to_string(),
                to_pubkey: "222".to_string(),
                lamports: 20_000_000,
            },
            PlannedTransfer {
                worker_name: "worker-003".to_string(),
                to_pubkey: "333".to_string(),
                lamports: 30_000_000,
            },
        ];

        let batches = batch_transfers(&transfers, 2).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[1][0].worker_name, "worker-003");

        let check =
            check_distribution_funding(60_005_000, 60_000_000, transfers.len(), 2, 5_000).unwrap();
        assert_eq!(check.transaction_count, 2);
        assert_eq!(check.fee_lamports, 10_000);
        assert_eq!(check.required_lamports, 60_010_000);
        assert_eq!(check.shortfall_lamports, 5_000);
        assert!(!check.has_sufficient_funding);
    }

    #[test]
    fn sol_to_lamports_handles_decimal_amounts() {
        assert_eq!(sol_to_lamports("0.03").unwrap(), 30_000_000);
        assert_eq!(sol_to_lamports("1").unwrap(), 1_000_000_000);
        assert_eq!(sol_to_lamports("0.000000001").unwrap(), 1);
        assert!(sol_to_lamports("0.0000000001").is_err());
    }

    #[test]
    fn miner_lines_are_classified_for_jsonl_logs() {
        let mined = classify_miner_line("     MINED!   +25 EQM     try #3   601ms   1.6 H/s");
        assert_eq!(mined.kind, "mined");

        let round = classify_miner_line("   round #42   reward 25 EQM   target 0x10ffff");
        assert_eq!(round.kind, "round");
        assert!(round.message.contains("round #42"));

        let other = classify_miner_line("   miner     AgbS-AEQM");
        assert_eq!(other.kind, "info");
    }

    #[test]
    fn monitor_summary_counts_workers_and_event_kinds() {
        let events = vec![
            MonitorLogEvent::new(100, "worker-001", "supervisor", "start"),
            MonitorLogEvent::new(110, "worker-002", "supervisor", "start"),
            MonitorLogEvent::new(150, "worker-001", "stdout", "round"),
            MonitorLogEvent::new(200, "worker-001", "stdout", "mined"),
            MonitorLogEvent::new(230, "worker-002", "stderr", "error"),
            MonitorLogEvent::new(240, "worker-002", "supervisor", "restart"),
            MonitorLogEvent::new(300, "worker-002", "supervisor", "exit"),
        ];

        let summary = summarize_monitor_events(&events, 2);

        assert_eq!(summary.expected_workers, 2);
        assert_eq!(summary.seen_workers, 2);
        assert_eq!(summary.running_workers, 1);
        assert_eq!(summary.exited_workers, 1);
        assert_eq!(summary.failed_workers, 0);
        assert_eq!(summary.rounds, 1);
        assert_eq!(summary.mined, 1);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.restarts, 1);
        assert_eq!(summary.last_event_unix_ms, Some(300));
    }

    #[test]
    fn monitor_summary_marks_workers_failed_when_error_is_final_state() {
        let events = vec![
            MonitorLogEvent::new(100, "worker-001", "supervisor", "start"),
            MonitorLogEvent::new(110, "worker-001", "supervisor", "error"),
            MonitorLogEvent::new(120, "worker-001", "supervisor", "restart"),
            MonitorLogEvent::new(130, "worker-001", "supervisor", "start"),
            MonitorLogEvent::new(140, "worker-001", "supervisor", "error"),
        ];

        let summary = summarize_monitor_events(&events, 1);

        assert_eq!(summary.running_workers, 0);
        assert_eq!(summary.exited_workers, 0);
        assert_eq!(summary.failed_workers, 1);
        assert_eq!(summary.errors, 2);
        assert_eq!(summary.restarts, 1);
    }

    #[test]
    fn launch_status_starts_only_when_mining_is_open() {
        assert!(!LaunchStatus::ConfigMissing.should_start_mining());
        assert!(!LaunchStatus::MiningClosed { block_height: 0 }.should_start_mining());
        assert!(LaunchStatus::MiningOpen { block_height: 7 }.should_start_mining());

        assert_eq!(
            LaunchStatus::MiningClosed { block_height: 3 }.summary(),
            "mining closed at block #3"
        );
        assert_eq!(
            LaunchStatus::MiningOpen { block_height: 4 }.summary(),
            "mining open at block #4"
        );
    }

    #[test]
    fn worker_selection_accepts_all_or_count() {
        let workers = build_manifest(4).workers;

        assert_eq!(select_workers(&workers, "all").unwrap().len(), 4);
        let first_two = select_workers(&workers, "2").unwrap();
        assert_eq!(first_two.len(), 2);
        assert_eq!(first_two[1].name, "worker-002");
        assert!(select_workers(&workers, "0").is_err());
        assert!(select_workers(&workers, "5").is_err());
    }

    #[test]
    fn worker_selection_accepts_ranges_names_and_dedupes() {
        let workers = build_manifest(5).workers;

        let selected = select_workers(&workers, "2-4,worker-005,worker-003").unwrap();

        let names: Vec<_> = selected.iter().map(|worker| worker.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["worker-002", "worker-003", "worker-004", "worker-005"]
        );
        assert!(select_workers(&workers, "4-2").is_err());
        assert!(select_workers(&workers, "worker-999").is_err());
    }

    #[test]
    fn supervisor_helpers_restart_failed_workers_and_stagger_starts() {
        let config = SupervisorConfig {
            max_restarts: 2,
            restart_delay_ms: 750,
            stagger_ms: 250,
        };

        assert!(should_restart_worker(false, 0, &config));
        assert!(should_restart_worker(false, 1, &config));
        assert!(!should_restart_worker(false, 2, &config));
        assert!(!should_restart_worker(true, 0, &config));
        assert_eq!(worker_start_delay_ms(0, &config), 0);
        assert_eq!(worker_start_delay_ms(3, &config), 750);
    }

    #[test]
    fn agent_next_actions_start_with_wallet_creation_without_manifest() {
        let actions = agent_next_actions(&AgentStatusInput {
            manifest_exists: false,
            rpc_configured: false,
            miner_bin_exists: false,
            worker_count: None,
            funding_lamports: None,
            planned_transfer_count: None,
            planned_total_lamports: None,
        });

        assert_eq!(
            actions,
            vec!["run: equium-fleet wallets create --workers 8"]
        );
    }

    #[test]
    fn agent_next_actions_report_funding_and_distribution() {
        let actions = agent_next_actions(&AgentStatusInput {
            manifest_exists: true,
            rpc_configured: true,
            miner_bin_exists: true,
            worker_count: Some(8),
            funding_lamports: Some(0),
            planned_transfer_count: Some(8),
            planned_total_lamports: Some(240_000_000),
        });

        assert!(actions.contains(&"fund the printed funding wallet with SOL".to_string()));
        assert!(actions
            .contains(&"dry-run distribution, then rerun with --live when correct".to_string()));
    }

    #[test]
    fn agent_next_actions_use_supervised_mining_command_when_ready() {
        let actions = agent_next_actions(&AgentStatusInput {
            manifest_exists: true,
            rpc_configured: true,
            miner_bin_exists: true,
            worker_count: Some(8),
            funding_lamports: Some(1_000_000_000),
            planned_transfer_count: Some(0),
            planned_total_lamports: Some(0),
        });

        assert_eq!(
            actions,
            vec!["run: equium-fleet mine fleet --workers all --max-restarts 2 --stagger-ms 250"]
        );
    }
}
