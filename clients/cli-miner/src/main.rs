//! Equium reference CLI miner.
//!
//! Polling-based loop (M3 minimum): fetch config → solve current challenge →
//! submit `mine` ix → wait for confirmation → repeat. Upgrades to WebSocket
//! `accountSubscribe` planned for M4 once browser miner needs the same plumbing.
//!
//! Usage:
//!   equium-miner --rpc-url http://127.0.0.1:8899 \
//!                --keypair ~/.config/solana/id.json \
//!                --max-blocks 100

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anchor_lang::prelude::AccountMeta;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossbeam_channel::{bounded, RecvTimeoutError};
use equihash_core::challenge::{build_input, solution_hash};
use equihash_core::solver::{try_nonce, BaseState};
use equihash_core::target::hash_under_target;
use equium::state::{EquiumConfig, CONFIG_SEED, VAULT_SEED};
use rand::RngCore;
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{read_keypair_file, Keypair, Signer};
use solana_sdk::system_program;
use solana_sdk::sysvar;
use solana_sdk::transaction::Transaction;

#[derive(Parser, Debug)]
#[command(version, about = "Equium reference CPU miner")]
struct Args {
    /// RPC endpoint URL. Defaults to the public mainnet endpoint, which
    /// rate-limits aggressively under sustained load — use a Helius / Triton
    /// key for real mining.
    #[arg(long, default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Path to a keypair JSON for the miner wallet.
    #[arg(long)]
    keypair: PathBuf,

    /// Override the program ID. Defaults to the value compiled into `equium`.
    #[arg(long)]
    program_id: Option<String>,

    /// Stop after N successful blocks (0 = run forever).
    #[arg(long, default_value_t = 0u64)]
    max_blocks: u64,

    /// Compute-unit limit per `mine` tx. Plan: 1.4M.
    #[arg(long, default_value_t = 1_400_000u32)]
    cu_limit: u32,

    /// Cap on nonce attempts per worker thread before refetching state.
    /// Total attempts per round ≈ threads × this value.
    #[arg(long, default_value_t = 4096u64)]
    max_nonces_per_round: u64,

    /// Number of solver threads. Defaults to all physical cores. Each thread
    /// independently grinds nonces; first to find a below-target solution
    /// wins the round.
    #[arg(long, short = 't', default_value_t = 0)]
    threads: usize,
}

// ANSI styling shortcuts. Colors are picked to look good against either a
// warm cream (recording) or default dark terminal. The palette is intentionally
// simple — magenta/rose for brand, gold for highlights, sage for wins.
const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m";
const C_BOLD: &str = "\x1b[1m";
const C_ROSE: &str = "\x1b[35m"; // brand
const C_ROSE_B: &str = "\x1b[1;35m";
const C_GOLD: &str = "\x1b[33m";
const C_GOLD_B: &str = "\x1b[1;33m";
const C_SAGE: &str = "\x1b[32m";
const C_SAGE_B: &str = "\x1b[1;32m";
const C_TEAL: &str = "\x1b[36m";
const C_GRAY: &str = "\x1b[90m";

const LOGO: &str = r#"
   ███████╗ ██████╗ ██╗   ██╗██╗██╗   ██╗███╗   ███╗
   ██╔════╝██╔═══██╗██║   ██║██║██║   ██║████╗ ████║
   █████╗  ██║   ██║██║   ██║██║██║   ██║██╔████╔██║
   ██╔══╝  ██║▄▄ ██║██║   ██║██║██║   ██║██║╚██╔╝██║
   ███████╗╚██████╔╝╚██████╔╝██║╚██████╔╝██║ ╚═╝ ██║
   ╚══════╝ ╚══▀▀═╝  ╚═════╝ ╚═╝ ╚═════╝ ╚═╝     ╚═╝"#;

const RULE: &str = "   ────────────────────────────────────────────────────";

fn main() -> Result<()> {
    // Initialize a quiet env_logger only for crate-internal modules; the
    // miner itself uses println! for fully-controlled formatting.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format(|buf, record| {
            use std::io::Write;
            writeln!(buf, "{}{}{}  {}", C_GRAY, record.level(), C_RESET, record.args())
        })
        .init();

    let args = Args::parse();

    let program_id: Pubkey = match &args.program_id {
        Some(s) => Pubkey::from_str(s).context("invalid --program-id")?,
        None => equium::ID,
    };
    let miner_kp = read_keypair_file(&args.keypair)
        .map_err(|e| anyhow!("read keypair {}: {}", args.keypair.display(), e))?;
    let miner = miner_kp.pubkey();

    let rpc = RpcClient::new_with_commitment(args.rpc_url.clone(), CommitmentConfig::confirmed());

    let (config_pda, _) = Pubkey::find_program_address(&[CONFIG_SEED], &program_id);
    let (vault_pda, _) = Pubkey::find_program_address(&[VAULT_SEED], &program_id);

    let network_label = network_label_from_url(&args.rpc_url);

    print_boot(&miner, &program_id, network_label);

    let mut blocks_mined = 0u64;
    let started_at = Instant::now();

    // The mint determines which token program (classic SPL or Token-2022)
    // to talk to. Read it once at startup; if the mint is ever changed
    // off-chain (it shouldn't be once authority is revoked) the miner can
    // be restarted.
    let token_program_id = {
        let cfg = fetch_config(&rpc, &config_pda)
            .with_context(|| format!("fetch config at {}", config_pda))?;
        let mint_acct = rpc.get_account(&cfg.mint).with_context(|| {
            format!("fetch mint {} for token program detection", cfg.mint)
        })?;
        mint_acct.owner
    };
    let mut current_height: u64 = u64::MAX;
    let mut try_in_round: u32 = 0;
    let mut total_nonces: u64 = 0;
    let mut total_reward_base: u64 = 0;
    // Empty-round watchdog. If the chain doesn't move for ROUND_STALL_SECS
    // seconds, the round has stalled — fire `advance_empty_round`. Cooldown
    // prevents us from spamming tx fees if our call is racing other miners.
    const ROUND_STALL_SECS: u64 = 75;
    const ADVANCE_COOLDOWN_SECS: u64 = 30;
    let mut last_height_change_at = Instant::now();
    let mut last_advance_attempt_at: Option<Instant> = None;

    loop {
        let cfg = fetch_config(&rpc, &config_pda)
            .with_context(|| format!("fetch config at {}", config_pda))?;
        let miner_ata = derive_ata(&miner, &cfg.mint, &token_program_id);

        if cfg.block_height != current_height {
            current_height = cfg.block_height;
            try_in_round = 0;
            last_height_change_at = Instant::now();
            last_advance_attempt_at = None;
            println!();
            println!(
                "   {}round #{}{}   {}prize {} EQM{}   {}target 0x{}…{}",
                C_BOLD, cfg.block_height, C_RESET,
                C_DIM, format_reward(cfg.current_epoch_reward), C_RESET,
                C_DIM, hex::encode(&cfg.current_target[..4]), C_RESET,
            );
            println!("{}{}{}", C_GRAY, RULE, C_RESET);
        }

        // Empty-round watchdog: if the round has stalled, try to advance it
        // ourselves. The on-chain instruction requires ROUND_TIMEOUT_SLOTS
        // (≈60s) of slot-elapsed time; we wait a bit longer for safety, and
        // back off ADVANCE_COOLDOWN_SECS between attempts so racing miners
        // don't all spam fees.
        let stall_for = last_height_change_at.elapsed();
        let cooled_down = last_advance_attempt_at
            .map(|t| t.elapsed() >= Duration::from_secs(ADVANCE_COOLDOWN_SECS))
            .unwrap_or(true);
        if stall_for >= Duration::from_secs(ROUND_STALL_SECS) && cooled_down {
            println!(
                "   {}round stalled {}s — calling advance_empty_round{}",
                C_GRAY,
                stall_for.as_secs(),
                C_RESET
            );
            last_advance_attempt_at = Some(Instant::now());
            match submit_advance_empty_round(&rpc, &miner_kp, &program_id, &config_pda) {
                Ok(sig) => println!(
                    "     {}↳ advanced empty round{}   {}sig {}{}",
                    C_SAGE, C_RESET, C_GRAY, short_sig(&sig), C_RESET
                ),
                Err(e) => {
                    let reason = if e.to_string().contains("RoundStillActive") {
                        "another miner beat us to it"
                    } else {
                        "couldn't advance"
                    };
                    println!("     {}↳ {}{}", C_GRAY, reason, C_RESET);
                }
            }
            // Next config fetch will pick up the new height (if our call
            // landed). Skip the solve this iteration.
            continue;
        }

        let solve_started = Instant::now();
        let input = build_input(
            &cfg.current_challenge,
            &miner.to_bytes(),
            cfg.block_height,
        );
        let thread_count = if args.threads == 0 {
            num_cpus::get().max(1)
        } else {
            args.threads
        };

        // Race N worker threads against the same target. First below-target
        // solution wins the round; others abort via the shared stop flag.
        let solution = match race_for_solution(
            cfg.equihash_n,
            cfg.equihash_k,
            &input,
            &cfg.current_target,
            thread_count,
            args.max_nonces_per_round,
        ) {
            Some((sol, nonces_tried)) => {
                total_nonces = total_nonces.saturating_add(nonces_tried);
                sol
            }
            None => {
                // No below-target nonce in this budget. Bump the counter,
                // refetch config, try again.
                total_nonces = total_nonces.saturating_add(
                    args.max_nonces_per_round * thread_count as u64,
                );
                try_in_round += 1;
                let solve_ms = solve_started.elapsed().as_millis() as u64;
                let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
                let hashrate = total_nonces as f64 / session_secs;
                println!(
                    "     {}· try #{}{}   {}exhausted{}        {}{}ms{}   {}{}{}",
                    C_GRAY, try_in_round, C_RESET,
                    C_DIM, C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD, fmt_hashrate(hashrate), C_RESET,
                );
                continue;
            }
        };
        let solve_ms = solve_started.elapsed().as_millis() as u64;
        try_in_round += 1;

        let session_secs = started_at.elapsed().as_secs_f64().max(0.001);
        let hashrate = total_nonces as f64 / session_secs;

        // Sanity: re-verify off-chain that the winning solution is under target.
        let cand_hash = solution_hash(&solution.soln_indices, &input);
        debug_assert!(hash_under_target(&cand_hash, &cfg.current_target));
        let _ = cand_hash;

        match submit_mine(
            &rpc,
            &miner_kp,
            &program_id,
            &config_pda,
            &cfg,
            &vault_pda,
            &miner_ata,
            &token_program_id,
            &solution.nonce,
            solution.soln_indices.clone(),
            args.cu_limit,
        ) {
            Ok(sig) => {
                blocks_mined += 1;
                total_reward_base = total_reward_base.saturating_add(cfg.current_epoch_reward);
                println!(
                    "     {}✓ MINED!{}   {}+{} EQM{}     {}try #{}{}   {}{}ms{}   {}{}{}",
                    C_SAGE_B, C_RESET,
                    C_BOLD, format_reward(cfg.current_epoch_reward), C_RESET,
                    C_DIM, try_in_round, C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD_B, fmt_hashrate(hashrate), C_RESET,
                );
                println!("       {}sig {}{}", C_GRAY, short_sig(&sig), C_RESET);
                println!();
                println!(
                    "   {}total mined{}  {}{} EQM{}   {}·{}   {}blocks{}  {}{}{}   {}·{}   {}uptime{}  {}{}{}",
                    C_DIM, C_RESET,
                    C_BOLD, format_reward(total_reward_base), C_RESET,
                    C_GRAY, C_RESET,
                    C_DIM, C_RESET, C_BOLD, blocks_mined, C_RESET,
                    C_GRAY, C_RESET,
                    C_DIM, C_RESET, C_BOLD, fmt_uptime(session_secs), C_RESET,
                );
            }
            Err(e) => {
                let reason = classify_submit_err(&e.to_string());
                println!(
                    "     {}· try #{}{}   {}{}{}        {}{}ms{}   {}{}{}",
                    C_GRAY, try_in_round, C_RESET,
                    C_DIM, reason, C_RESET,
                    C_DIM, solve_ms, C_RESET,
                    C_GOLD, fmt_hashrate(hashrate), C_RESET,
                );
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }

        if args.max_blocks > 0 && blocks_mined >= args.max_blocks {
            let elapsed = started_at.elapsed().as_secs_f64();
            println!();
            println!(
                "   {}session complete{}  ·  {} blocks  ·  avg latency {:.1}s  ·  {}",
                C_ROSE_B, C_RESET,
                args.max_blocks,
                elapsed / blocks_mined as f64,
                fmt_hashrate(hashrate),
            );
            return Ok(());
        }
    }
}

fn print_boot(miner: &Pubkey, program: &Pubkey, network: &str) {
    println!("{}{}{}", C_ROSE_B, LOGO, C_RESET);
    println!(
        "   {}cpu-mineable on solana{}                            {}$EQM ⛏{}",
        C_DIM, C_RESET, C_GOLD_B, C_RESET
    );
    println!();
    println!("{}{}{}", C_GRAY, RULE, C_RESET);
    println!("   {}miner{}     {}{}{}", C_DIM, C_RESET, C_TEAL, short_pk(miner), C_RESET);
    println!("   {}program{}   {}{}{}", C_DIM, C_RESET, C_TEAL, short_pk(program), C_RESET);
    println!("   {}network{}   {}{}{}", C_DIM, C_RESET, C_TEAL, network, C_RESET);
    println!("{}{}{}", C_GRAY, RULE, C_RESET);
}

fn network_label_from_url(url: &str) -> &'static str {
    if url.contains("mainnet") {
        "solana mainnet"
    } else if url.contains("devnet") {
        "solana devnet"
    } else if url.contains("testnet") {
        "solana testnet"
    } else if url.contains("127.0.0.1") || url.contains("localhost") {
        "solana localnet"
    } else {
        "solana custom"
    }
}

fn fmt_hashrate(hashes_per_sec: f64) -> String {
    if hashes_per_sec >= 1000.0 {
        format!("{:.1} kH/s", hashes_per_sec / 1000.0)
    } else {
        format!("{:.1} H/s", hashes_per_sec)
    }
}

fn fmt_uptime(seconds: f64) -> String {
    let total = seconds as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

fn short_pk(pk: &Pubkey) -> String {
    let s = pk.to_string();
    format!("{}…{}", &s[..4], &s[s.len() - 4..])
}

fn short_sig(s: &str) -> String {
    if s.len() <= 12 {
        return s.to_string();
    }
    format!("{}…{}", &s[..6], &s[s.len() - 6..])
}

fn format_reward(base_units: u64) -> String {
    let whole = base_units / 1_000_000;
    let frac = base_units % 1_000_000;
    if frac == 0 {
        format!("{}", whole)
    } else {
        format!("{}.{:06}", whole, frac).trim_end_matches('0').to_string()
    }
}

/// Map a submit error string to a single-word reason.
fn classify_submit_err(s: &str) -> &'static str {
    if s.contains("custom program error: 0x1773") || s.contains("AboveTarget") {
        "above target"
    } else if s.contains("custom program error: 0x1772") || s.contains("InvalidEquihash") {
        "stale challenge"
    } else if s.contains("blockhash not found") || s.contains("BlockhashNotFound") {
        "blockhash expired"
    } else {
        "submit error"
    }
}

/// Derive the ATA for `(owner, mint)` under the given token program. Works
/// for both classic SPL Token and Token-2022 — the address depends on which
/// token program owns the mint.
fn derive_ata(owner: &Pubkey, mint: &Pubkey, token_program_id: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, token_program_id)
}

fn fetch_config(rpc: &RpcClient, config_pda: &Pubkey) -> Result<EquiumConfig> {
    let acct = rpc.get_account(config_pda).map_err(|e| {
        anyhow::anyhow!(
            "Could not fetch program config account {}\n\
             Caused by: {}\n\
             \n\
             This usually means one of:\n\
             1. Wrong --rpc-url: make sure you are pointing at Solana mainnet-beta\n\
             2. Wrong program ID: the miner binary was built against a different deployment\n\
             3. Program not yet deployed: check https://equium.xyz for the current status\n\
             4. RPC node is lagging: try a different RPC endpoint (e.g. https://helius.dev)",
            config_pda, e
        )
    })?;

    if acct.data.is_empty() {
        anyhow::bail!(
            "Program config account {} exists but has no data.\n\
             The Equium program may not be fully initialized yet.\n\
             Check https://equium.xyz or the #mining channel for status.",
            config_pda
        );
    }

    let mut data = acct.data.as_slice();
    let cfg = EquiumConfig::try_deserialize(&mut data).map_err(|e| {
        anyhow::anyhow!(
            "Failed to deserialize program config at {}\n\
             Caused by: {}\n\
             \n\
             This usually means the on-chain program was redeployed with a new\n\
             account layout. Update your miner binary:\n\
             git pull && cargo build --release -p equium-cli-miner",
            config_pda, e
        )
    })?;
    Ok(cfg)
}

struct RaceWinner {
    nonce: [u8; 32],
    soln_indices: Vec<u8>,
}

/// Spawn `threads` solver workers and race them to find a nonce whose
/// Equihash solution falls under `target`. Each worker grinds up to
/// `max_per_thread` nonces before giving up. Returns the winning solution
/// plus the total number of nonces actually tried (across all threads)
/// so the caller can update its hashrate counter.
fn race_for_solution(
    n: u32,
    k: u32,
    input: &[u8; equihash_core::challenge::I_LEN],
    target: &[u8; 32],
    threads: usize,
    max_per_thread: u64,
) -> Option<(RaceWinner, u64)> {
    let base = std::sync::Arc::new(BaseState::new(n, k, input).ok()?);
    let stop = Arc::new(AtomicBool::new(false));
    let total_nonces = Arc::new(AtomicU64::new(0));
    let (tx, rx) = bounded::<RaceWinner>(1);

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let base = base.clone();
        let input = *input;
        let target = *target;
        let stop = stop.clone();
        let total = total_nonces.clone();
        let tx = tx.clone();
        let max = max_per_thread;
        handles.push(std::thread::spawn(move || {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            let mut tried: u64 = 0;
            while tried < max && !stop.load(Ordering::Relaxed) {
                let mut nonce = [0u8; 32];
                rng.fill(&mut nonce);
                tried += 1;
                if let Some(soln) = try_nonce(&base, &input, &nonce) {
                    let h = solution_hash(&soln, &input);
                    if hash_under_target(&h, &target) {
                        // Best-effort send; if the channel is closed the
                        // race is already over.
                        let _ = tx.send(RaceWinner {
                            nonce,
                            soln_indices: soln,
                        });
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            total.fetch_add(tried, Ordering::Relaxed);
        }));
    }
    drop(tx);

    // Wait for either a winner or all threads to exhaust their budget.
    let winner = rx.recv_timeout(Duration::from_secs(600)).ok();
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    let nonces_tried = total_nonces.load(Ordering::Relaxed);
    winner.map(|w| (w, nonces_tried))
}

#[allow(clippy::too_many_arguments)]
fn submit_mine(
    rpc: &RpcClient,
    miner_kp: &Keypair,
    program_id: &Pubkey,
    config_pda: &Pubkey,
    cfg: &EquiumConfig,
    vault_pda: &Pubkey,
    miner_ata: &Pubkey,
    token_program_id: &Pubkey,
    nonce: &[u8; 32],
    soln_indices: Vec<u8>,
    cu_limit: u32,
) -> Result<String> {
    let miner = miner_kp.pubkey();
    let accounts = equium::accounts::Mine {
        miner,
        config: *config_pda,
        mint: cfg.mint,
        mineable_vault: *vault_pda,
        miner_ata: *miner_ata,
        token_program: *token_program_id,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: system_program::ID,
        slot_hashes: sysvar::slot_hashes::ID,
    }
    .to_account_metas(None);
    // anchor-lang re-exports a `solana-program`-flavored AccountMeta; convert.
    let accounts: Vec<AccountMeta> = accounts
        .into_iter()
        .map(|m| AccountMeta {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    let data = equium::instruction::Mine {
        nonce: *nonce,
        soln_indices,
    }
    .data();
    let ix = Instruction {
        program_id: *program_id,
        accounts,
        data,
    };
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(cu_limit);

    let recent = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix, ix],
        Some(&miner),
        &[miner_kp],
        recent,
    );
    let sig = rpc.send_and_confirm_transaction(&tx)?;
    Ok(sig.to_string())
}

/// Permissionless on-chain instruction that closes a stalled round. Callable
/// after `ROUND_TIMEOUT_SLOTS` slots have elapsed since the round opened with
/// no winning solution. The caller pays the tx fee; the unminted reward stays
/// in the vault permanently.
fn submit_advance_empty_round(
    rpc: &RpcClient,
    caller_kp: &Keypair,
    program_id: &Pubkey,
    config_pda: &Pubkey,
) -> Result<String> {
    let caller = caller_kp.pubkey();
    let accounts = equium::accounts::AdvanceEmptyRound {
        caller,
        config: *config_pda,
        slot_hashes: sysvar::slot_hashes::ID,
    }
    .to_account_metas(None);
    let accounts: Vec<AccountMeta> = accounts
        .into_iter()
        .map(|m| AccountMeta {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    let data = equium::instruction::AdvanceEmptyRound {}.data();
    let ix = Instruction {
        program_id: *program_id,
        accounts,
        data,
    };

    let recent = rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&caller), &[caller_kp], recent);
    let sig = rpc.send_and_confirm_transaction(&tx)?;
    Ok(sig.to_string())
}
