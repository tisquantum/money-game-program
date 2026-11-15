//! Everything in this file runs on the public Ephemeral Rollup, where each
//! game's `Ledger` lives. Money and medals change hands here in ordinary
//! transactions anyone can look up.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::commit;
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder};

use session_keys::{session_auth_or, Session, SessionError, SessionTokenV2};

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

#[derive(Accounts)]
pub struct CrankLedger<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [LEDGER_SEED, &ledger.game_id.to_le_bytes()], bump = ledger.bump)]
    pub ledger: Box<Account<'info, Ledger>>,
}

/// Crank-only: applies the game's public log entry `seq` (seat changes, medals,
/// mini-game totals, The Deal's split, the end of the game). `wallet` is the
/// seat's new wallet for a seating entry.
pub fn relay(ctx: Context<CrankLedger>, seq: u32, entry: LogEntry, wallet: Pubkey) -> Result<()> {
    require_keys_eq!(ctx.accounts.payer.key(), ctx.accounts.config.crank, MoneyError::NotCrank);
    ctx.accounts.ledger.relay(seq, &entry, wallet)?;
    Ok(())
}

/// Crank-only: opens the transfer window for turn `stamp - 1`, or closes it (0).
pub fn set_window(ctx: Context<CrankLedger>, stamp: u8) -> Result<()> {
    require_keys_eq!(ctx.accounts.payer.key(), ctx.accounts.config.crank, MoneyError::NotCrank);
    ctx.accounts.ledger.window = stamp;
    Ok(())
}

#[derive(Accounts, Session)]
pub struct LedgerTransfer<'info> {
    pub player: Signer<'info>,
    #[account(mut, seeds = [LEDGER_SEED, &ledger.game_id.to_le_bytes()], bump = ledger.bump)]
    pub ledger: Box<Account<'info, Ledger>>,
    /// CHECK: the wallet that holds the seat. A session's authority has to be this key.
    pub holder: UncheckedAccount<'info>,
    /// Present when `player` is a session key authorized by `holder`.
    #[session(signer = player, authority = holder.key())]
    pub session_token: Option<Account<'info, SessionTokenV2>>,
}

/// Open transfer of points to another seat (bribes, promises, trust), while
/// a transfer window is open.
#[session_auth_or(
    ctx.accounts.holder.key() == ctx.accounts.player.key(),
    SessionError::InvalidToken
)]
pub fn transfer_points(ctx: Context<LedgerTransfer>, seat: u8, to: u8, amount: u32) -> Result<()> {
    let holder = ctx.accounts.holder.key();
    ctx.accounts
        .ledger
        .transfer(seat as usize, to as usize, &holder, amount)
}

#[commit]
#[derive(Accounts)]
pub struct SettleLedger<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [LEDGER_SEED, &ledger.game_id.to_le_bytes()], bump = ledger.bump)]
    pub ledger: Box<Account<'info, Ledger>>,
}

/// Permissionless once the game is over: commits the final balances and
/// medals back to the base layer, where `finalize` pays out from them.
pub fn settle_ledger(ctx: Context<SettleLedger>) -> Result<()> {
    require!(ctx.accounts.ledger.finished, MoneyError::NotFinished);
    let a = &ctx.accounts;
    MagicIntentBundleBuilder::new(
        a.payer.to_account_info(),
        a.magic_context.to_account_info(),
        a.magic_program.to_account_info(),
    )
    .commit_and_undelegate(&[a.ledger.to_account_info()])
    .build_and_invoke()?;
    Ok(())
}
