//! Equium ($EQM) — on-chain CPU mining program.
//!
//! See `/home/ubuntu/.claude/plans/i-want-to-develop-kind-rossum.md` for the
//! canonical design. Account structs live in this file (Anchor 0.31 expects
//! them next to `#[program]`); handler logic lives in `instructions/`.

// Anchor 0.31's `#[program]` macro expands to a call to
// `AccountInfo::realloc`, which solana-program 2.x marked deprecated
// in favor of `resize()`. The deprecation is purely cosmetic — both
// methods compile to the same on-chain code path. Anchor's next
// release fixes the macro; until then we suppress at the crate
// level so user builds aren't drowned in macro-derived warnings.
#![allow(deprecated)]

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

pub mod errors;
pub mod instructions;
pub mod pow;
pub mod round;
pub mod schedule;
pub mod state;

use crate::errors::EquiumError;
use crate::state::{EquiumConfig, CONFIG_SEED, VAULT_SEED};

declare_id!("ZKGMUfxiRCXFPnqz9zgqAnuqJy15jk7fKbR4o6FuEQM");

/// The only pubkey allowed to call `initialize` and (until renounced)
/// `fund_vault`, `set_target`, and `renounce_admin`. Hardcoding this
/// closes the deploy-race window: an attacker monitoring the mempool
/// can no longer front-run the deployer's `initialize` to claim admin.
pub const EXPECTED_ADMIN: Pubkey = pubkey!("AgbSti5LyTfYHVytBhNP8HHz3Ko7bZfSvnsm9cJAEQM");

#[program]
pub mod equium {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, args: InitializeArgs) -> Result<()> {
        instructions::initialize::handler(ctx, args)
    }

    pub fn fund_vault(ctx: Context<FundVault>) -> Result<()> {
        instructions::fund_vault::handler(ctx)
    }

    pub fn mine(
        ctx: Context<Mine>,
        nonce: [u8; 32],
        soln_indices: Vec<u8>,
    ) -> Result<()> {
        instructions::mine::handler(ctx, nonce, soln_indices)
    }

    pub fn advance_empty_round(ctx: Context<AdvanceEmptyRound>) -> Result<()> {
        instructions::advance_empty_round::handler(ctx)
    }

    pub fn renounce_admin(ctx: Context<RenounceAdmin>) -> Result<()> {
        instructions::renounce_admin::handler(ctx)
    }

    pub fn set_target(ctx: Context<SetTarget>, new_target: [u8; 32]) -> Result<()> {
        instructions::set_target::handler(ctx, new_target)
    }
}

// ---------------------------------------------------------------------------
// Account contexts
// ---------------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitializeArgs {
    /// 256-bit big-endian initial difficulty target.
    pub initial_target: [u8; 32],
    /// Equihash parameter `n` (locked at init).
    pub equihash_n: u32,
    /// Equihash parameter `k`.
    pub equihash_k: u32,
    /// Metaplex metadata URI (icon + descriptive JSON).
    pub metadata_uri: String,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut, address = EXPECTED_ADMIN @ EquiumError::NotAdmin)]
    pub admin: Signer<'info>,

    #[account(
        init,
        payer = admin,
        space = 8 + EquiumConfig::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump,
    )]
    pub config: Account<'info, EquiumConfig>,

    /// Pre-existing mint, created by the deployer before `initialize`. The
    /// program never mints, so mint authority management (including
    /// revocation) is the deployer's job. Accepts classic SPL Token OR
    /// Token-2022 mints — caller selects via `token_program`.
    pub mint: InterfaceAccount<'info, Mint>,

    /// PDA-addressed token account custodying the mineable supply. Created
    /// here as empty; gets the deposit via `fund_vault`. Authority is the
    /// config PDA so only the program can transfer out.
    #[account(
        init,
        payer = admin,
        seeds = [VAULT_SEED],
        bump,
        token::mint = mint,
        token::authority = config,
        token::token_program = token_program,
    )]
    pub mineable_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct FundVault<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.config_bump,
        has_one = admin @ EquiumError::NotAdmin,
        has_one = mint @ EquiumError::WrongMint,
        has_one = mineable_vault @ EquiumError::WrongVault,
    )]
    pub config: Account<'info, EquiumConfig>,

    pub mint: InterfaceAccount<'info, Mint>,

    /// Admin's source token account. SPL transfer enforces that `admin` is
    /// the authority; we just verify the mint matches.
    #[account(
        mut,
        constraint = source.mint == mint.key() @ EquiumError::SourceMintMismatch,
    )]
    pub source: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [VAULT_SEED],
        bump = config.mineable_vault_bump,
    )]
    pub mineable_vault: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct Mine<'info> {
    #[account(mut)]
    pub miner: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.config_bump,
        has_one = mint @ EquiumError::WrongMint,
        has_one = mineable_vault @ EquiumError::WrongVault,
    )]
    pub config: Account<'info, EquiumConfig>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [VAULT_SEED],
        bump = config.mineable_vault_bump,
    )]
    pub mineable_vault: InterfaceAccount<'info, TokenAccount>,

    /// Miner's ATA receives the block reward. Created on first mine if
    /// missing.
    #[account(
        init_if_needed,
        payer = miner,
        associated_token::mint = mint,
        associated_token::authority = miner,
        associated_token::token_program = token_program,
    )]
    pub miner_ata: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,

    /// CHECK: SlotHashes sysvar — read manually in `pow::read_recent_slot_hash`
    /// because `Sysvar::get` doesn't work for SlotHashes (account too large).
    #[account(address = anchor_lang::solana_program::sysvar::slot_hashes::ID)]
    pub slot_hashes: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct AdvanceEmptyRound<'info> {
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.config_bump,
    )]
    pub config: Account<'info, EquiumConfig>,

    /// CHECK: SlotHashes sysvar.
    #[account(address = anchor_lang::solana_program::sysvar::slot_hashes::ID)]
    pub slot_hashes: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct RenounceAdmin<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.config_bump,
        has_one = admin @ EquiumError::NotAdmin,
    )]
    pub config: Account<'info, EquiumConfig>,
}

#[derive(Accounts)]
pub struct SetTarget<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.config_bump,
        has_one = admin @ EquiumError::NotAdmin,
    )]
    pub config: Account<'info, EquiumConfig>,
}

