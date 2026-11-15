//! Money — a five-seat trust game for token holders, played in under an hour.
//!
//! Layers:
//! * **Base layer**: config, holder registry, treasury (pump.fun creator
//!   rewards, in USDC), per-game USDC escrow, VRF requests, claims.
//! * **Private Ephemeral Rollup (TEE)**: the live game. `Game` is public,
//!   each seat's `Hand` is private to that seat's wallet, and the `Record`
//!   of every round's moves and payouts is private to everyone until the
//!   game is committed at the end. Only medals and each mini-game's totals
//!   are made public as the game goes.
//! * **Public Ephemeral Rollup**: the `Ledger` of balances, medals and
//!   transfers, so money changes hands in transactions anyone can inspect.
//! * **VRF**: seat draws, replacement draws, raffle winners.
//! * **Session keys**: sealed moves. A wallet-signed session token
//!   lets an ephemeral key act for that wallet until it expires. Claims stay
//!   with the wallet.
#![allow(ambiguous_glob_reexports)]
#![allow(unexpected_cfgs)]

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::ephemeral;

pub mod constants;
pub mod errors;
pub mod instructions;
pub mod logic;
pub mod state;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("H6ZM85L5ktQGxR3dTbLckxSX3ac4P4FWnKfFGb2gAx3j");

#[ephemeral]
#[program]
pub mod money_game {
    use super::*;

    // ---- admin / base layer ------------------------------------------------
    pub fn init_config(ctx: Context<InitConfig>, params: ConfigParams) -> Result<()> {
        instructions::admin::init_config(ctx, params)
    }
    pub fn update_config(ctx: Context<UpdateConfig>, params: ConfigParams, paused: bool) -> Result<()> {
        instructions::admin::update_config(ctx, params, paused)
    }
    pub fn abandon_game(ctx: Context<AbandonGame>, game_id: u64) -> Result<()> {
        instructions::admin::abandon_game(ctx, game_id)
    }
    pub fn init_registry(ctx: Context<InitRegistry>) -> Result<()> {
        instructions::admin::init_registry(ctx)
    }
    pub fn fund_treasury(ctx: Context<FundTreasury>, amount: u64) -> Result<()> {
        instructions::admin::fund_treasury(ctx, amount)
    }

    // ---- holder registry (base) -------------------------------------------
    pub fn enter_registry(ctx: Context<EnterRegistry>) -> Result<()> {
        instructions::registry::enter_registry(ctx)
    }
    pub fn leave_registry(ctx: Context<LeaveRegistry>) -> Result<()> {
        instructions::registry::leave_registry(ctx)
    }
    pub fn prune_holder(ctx: Context<PruneHolder>) -> Result<()> {
        instructions::registry::prune_holder(ctx)
    }

    // ---- table lifecycle (base) -------------------------------------------
    pub fn start_game(ctx: Context<StartGame>) -> Result<()> {
        instructions::table::start_game(ctx)
    }
    pub fn delegate_pda(ctx: Context<DelegatePda>, target: DelegateTarget) -> Result<()> {
        instructions::table::delegate_pda(ctx, target)
    }
    pub fn finalize(ctx: Context<Finalize>) -> Result<()> {
        instructions::table::finalize(ctx)
    }
    pub fn reclaim_hands(ctx: Context<ReclaimHands>) -> Result<()> {
        instructions::table::reclaim_hands(ctx)
    }
    pub fn claim(ctx: Context<Claim>, seat: u8) -> Result<()> {
        instructions::table::claim(ctx, seat)
    }
    pub fn sweep(ctx: Context<Sweep>) -> Result<()> {
        instructions::table::sweep(ctx)
    }

    // ---- VRF ---------------------------------------------------------------
    pub fn request_draw(ctx: Context<RequestDraw>) -> Result<()> {
        instructions::draw::request_draw(ctx)
    }
    pub fn on_draw(ctx: Context<OnDraw>, randomness: [u8; 32], nonce: u64) -> Result<()> {
        instructions::draw::on_draw(ctx, randomness, nonce)
    }

    // ---- Private ER (TEE) ---------------------------------------------------
    pub fn init_game_permission(ctx: Context<InitGamePermission>, game_id: u64) -> Result<()> {
        instructions::play::init_game_permission(ctx, game_id)
    }
    pub fn init_record_permission(ctx: Context<InitRecordPermission>, game_id: u64) -> Result<()> {
        instructions::play::init_record_permission(ctx, game_id)
    }
    pub fn init_hand_permission(ctx: Context<InitHandPermission>, game_id: u64, seat: u8) -> Result<()> {
        instructions::play::init_hand_permission(ctx, game_id, seat)
    }
    pub fn sync_hand(ctx: Context<SyncHand>, seat: u8) -> Result<()> {
        instructions::play::sync_hand(ctx, seat)
    }
    pub fn apply_draw(ctx: Context<ApplyDraw>) -> Result<()> {
        instructions::play::apply_draw(ctx)
    }
    pub fn submit_move(ctx: Context<SubmitMove>, seat: u8, value: u32) -> Result<()> {
        instructions::play::submit_move(ctx, seat, value)
    }
    pub fn unseal_move(ctx: Context<SubmitMove>, seat: u8) -> Result<()> {
        instructions::play::unseal_move(ctx, seat)
    }
    pub fn propose_offer(ctx: Context<SeatAction>, seat: u8, offer: [u32; SEATS]) -> Result<()> {
        instructions::play::propose_offer(ctx, seat, offer)
    }
    pub fn take_guaranteed(ctx: Context<SeatAction>, seat: u8) -> Result<()> {
        instructions::play::take_guaranteed(ctx, seat)
    }
    pub fn tick(ctx: Context<Tick>) -> Result<()> {
        instructions::play::tick(ctx)
    }
    pub fn sync_balances(ctx: Context<SyncBalances>, balances: [u32; SEATS]) -> Result<()> {
        instructions::play::sync_balances(ctx, balances)
    }
    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        instructions::play::settle(ctx)
    }

    // ---- public ER (ledger) ---------------------------------------------------
    pub fn relay(ctx: Context<CrankLedger>, seq: u32, entry: LogEntry, wallet: Pubkey) -> Result<()> {
        instructions::ledger::relay(ctx, seq, entry, wallet)
    }
    pub fn set_window(ctx: Context<CrankLedger>, stamp: u8) -> Result<()> {
        instructions::ledger::set_window(ctx, stamp)
    }
    pub fn transfer_points(ctx: Context<LedgerTransfer>, seat: u8, to: u8, amount: u32) -> Result<()> {
        instructions::ledger::transfer_points(ctx, seat, to, amount)
    }
    pub fn settle_ledger(ctx: Context<SettleLedger>) -> Result<()> {
        instructions::ledger::settle_ledger(ctx)
    }
}
