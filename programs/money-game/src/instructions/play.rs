//! Everything in this file runs inside the Private Ephemeral Rollup (TEE).

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::{
    CreateEphemeralPermissionCpi, UpdateEphemeralPermissionCpi,
};
use ephemeral_rollups_sdk::access_control::structs::{
    EphemeralMembersArgs, Member, AUTHORITY_FLAG, TX_LOGS_FLAG,
};
use ephemeral_rollups_sdk::anchor::commit;
use ephemeral_rollups_sdk::consts::{EPHEMERAL_VAULT_ID, MAGIC_PROGRAM_ID, PERMISSION_PROGRAM_ID};
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder};

use session_keys::{session_auth_or, Session, SessionError, SessionTokenV2};

use crate::constants::*;
use crate::errors::MoneyError;
use crate::logic::Moves;
use crate::state::*;

fn now() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp)
}

/// The five hands, read in seat order.
#[derive(Accounts)]
pub struct Hands<'info> {
    #[account(mut, seeds = [HAND_SEED, &hand0.game_id.to_le_bytes(), &[0]], bump = hand0.bump)]
    pub hand0: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &hand1.game_id.to_le_bytes(), &[1]], bump = hand1.bump)]
    pub hand1: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &hand2.game_id.to_le_bytes(), &[2]], bump = hand2.bump)]
    pub hand2: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &hand3.game_id.to_le_bytes(), &[3]], bump = hand3.bump)]
    pub hand3: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &hand4.game_id.to_le_bytes(), &[4]], bump = hand4.bump)]
    pub hand4: Box<Account<'info, Hand>>,
}

impl<'info> Hands<'info> {
    /// Sealed moves for the game's current turn. A move only counts if the
    /// seat's current wallet made it this turn.
    pub fn moves(&self, game: &Game) -> Result<Moves> {
        let hands = [&self.hand0, &self.hand1, &self.hand2, &self.hand3, &self.hand4];
        let stamp = game.stamp();
        let mut out: Moves = [None; SEATS];
        for (i, h) in hands.iter().enumerate() {
            require!(h.game_id == game.game_id, MoneyError::BadSeat);
            if h.turn_stamp == stamp && game.seats[i].acted == stamp {
                out[i] = Some(h.value);
            }
        }
        Ok(out)
    }

    /// Gives each seat's hand its Money Makers supply, readable only by that seat.
    pub fn show_supplies(&mut self, rec: &Record) {
        let hands = [
            &mut self.hand0,
            &mut self.hand1,
            &mut self.hand2,
            &mut self.hand3,
            &mut self.hand4,
        ];
        for (i, h) in hands.into_iter().enumerate() {
            if h.supply != rec.mm_supply[i] {
                h.supply = rec.mm_supply[i];
            }
        }
    }

    /// Gives each seat that moved in `turn` its own result, readable only by
    /// that seat: what it won and whether it won the round. Nothing until the
    /// round has resolved (a parked raffle waits for its draw).
    pub fn show_results(&mut self, rec: &Record, turn: u8) {
        let r = &rec.rounds[turn as usize];
        if r.stamp != turn + 1 {
            return;
        }
        let hands = [
            &mut self.hand0,
            &mut self.hand1,
            &mut self.hand2,
            &mut self.hand3,
            &mut self.hand4,
        ];
        for (i, h) in hands.into_iter().enumerate() {
            let moved = r.mask & (1 << i) != 0;
            h.result_stamp = r.stamp;
            h.result = if moved { r.delta[i] } else { 0 };
            h.result_won = moved && r.winner as usize == i;
        }
    }
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(game_id: u64)]
pub struct InitGamePermission<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the game PDA; pays its own permission rent and signs via seeds.
    #[account(mut, seeds = [GAME_SEED, &game_id.to_le_bytes()], bump)]
    pub game: UncheckedAccount<'info>,
    /// CHECK: checked by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,
    /// CHECK: permission program.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,
    /// CHECK: magic vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,
    /// CHECK: magic program.
    #[account(address = MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
}

/// The table itself is public: spectators can watch every resolved turn.
pub fn init_game_permission(ctx: Context<InitGamePermission>, game_id: u64) -> Result<()> {
    if ctx.accounts.permission.lamports() > 0 {
        return Ok(());
    }
    let game = ctx.accounts.game.to_account_info();
    CreateEphemeralPermissionCpi {
        payer: game.clone(),
        permissioned_account: game,
        permission: ctx.accounts.permission.to_account_info(),
        vault: ctx.accounts.ephemeral_vault.to_account_info(),
        magic_program: ctx.accounts.magic_program.to_account_info(),
        permission_program: ctx.accounts.permission_program.to_account_info(),
        args: EphemeralMembersArgs {
            is_private: false,
            members: vec![],
        },
    }
    .invoke_signed(&[&[GAME_SEED, &game_id.to_le_bytes(), &[ctx.bumps.game]]])?;
    Ok(())
}

#[derive(Accounts)]
#[instruction(game_id: u64)]
pub struct InitRecordPermission<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the record PDA; pays its own permission rent and signs via seeds.
    #[account(mut, seeds = [RECORD_SEED, &game_id.to_le_bytes()], bump)]
    pub record: UncheckedAccount<'info>,
    /// CHECK: checked by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,
    /// CHECK: permission program.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,
    /// CHECK: magic vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,
    /// CHECK: magic program.
    #[account(address = MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
}

/// The record is sealed to everyone, the keeper included, until the game is
/// committed back to the base layer.
pub fn init_record_permission(ctx: Context<InitRecordPermission>, game_id: u64) -> Result<()> {
    if ctx.accounts.permission.lamports() > 0 {
        return Ok(());
    }
    let record = ctx.accounts.record.to_account_info();
    CreateEphemeralPermissionCpi {
        payer: record.clone(),
        permissioned_account: record,
        permission: ctx.accounts.permission.to_account_info(),
        vault: ctx.accounts.ephemeral_vault.to_account_info(),
        magic_program: ctx.accounts.magic_program.to_account_info(),
        permission_program: ctx.accounts.permission_program.to_account_info(),
        args: EphemeralMembersArgs {
            is_private: true,
            members: vec![],
        },
    }
    .invoke_signed(&[&[RECORD_SEED, &game_id.to_le_bytes(), &[ctx.bumps.record]]])?;
    Ok(())
}

#[derive(Accounts)]
#[instruction(game_id: u64, seat: u8)]
pub struct InitHandPermission<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the hand PDA; pays its own permission rent and signs via seeds.
    #[account(mut, seeds = [HAND_SEED, &game_id.to_le_bytes(), &[seat]], bump)]
    pub hand: UncheckedAccount<'info>,
    /// CHECK: checked by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,
    /// CHECK: permission program.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,
    /// CHECK: magic vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,
    /// CHECK: magic program.
    #[account(address = MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
}

/// Hands start sealed to everyone; `sync_hand` opens each to its wallet.
pub fn init_hand_permission(ctx: Context<InitHandPermission>, game_id: u64, seat: u8) -> Result<()> {
    if ctx.accounts.permission.lamports() > 0 {
        return Ok(());
    }
    let hand = ctx.accounts.hand.to_account_info();
    CreateEphemeralPermissionCpi {
        payer: hand.clone(),
        permissioned_account: hand,
        permission: ctx.accounts.permission.to_account_info(),
        vault: ctx.accounts.ephemeral_vault.to_account_info(),
        magic_program: ctx.accounts.magic_program.to_account_info(),
        permission_program: ctx.accounts.permission_program.to_account_info(),
        args: EphemeralMembersArgs {
            is_private: true,
            members: vec![],
        },
    }
    .invoke_signed(&[&[HAND_SEED, &game_id.to_le_bytes(), &[seat], &[ctx.bumps.hand]]])?;
    Ok(())
}

#[derive(Accounts)]
#[instruction(seat: u8)]
pub struct SyncHand<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[seat]], bump = hand.bump)]
    pub hand: Box<Account<'info, Hand>>,
    /// CHECK: checked by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,
    /// CHECK: permission program.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,
    /// CHECK: magic vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,
    /// CHECK: magic program.
    #[account(address = MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
}

/// Points a hand's private permission at whoever holds the seat now, so a
/// replacement wallet can read its own sealed moves and the old one can't.
pub fn sync_hand(ctx: Context<SyncHand>, seat: u8) -> Result<()> {
    let game = &ctx.accounts.game;
    let i = game.seat_index(seat)?;
    let s = game.seats[i];
    let target = if s.status == SEAT_ACTIVE { s.wallet } else { Pubkey::default() };
    if ctx.accounts.hand.member == target {
        return Ok(());
    }
    let members = if target == Pubkey::default() {
        vec![]
    } else {
        vec![Member {
            flags: AUTHORITY_FLAG | TX_LOGS_FLAG,
            pubkey: target,
        }]
    };
    let hand_ai = ctx.accounts.hand.to_account_info();
    UpdateEphemeralPermissionCpi {
        payer: hand_ai.clone(),
        permissioned_account: hand_ai.clone(),
        permission: ctx.accounts.permission.to_account_info(),
        vault: ctx.accounts.ephemeral_vault.to_account_info(),
        magic_program: ctx.accounts.magic_program.to_account_info(),
        permission_program: ctx.accounts.permission_program.to_account_info(),
        authority: hand_ai,
        authority_is_signer: false,
        args: EphemeralMembersArgs {
            is_private: true,
            members,
        },
    }
    .invoke_signed(&[&[
        HAND_SEED,
        &game.game_id.to_le_bytes(),
        &[seat],
        &[ctx.accounts.hand.bump],
    ]])?;
    ctx.accounts.hand.member = target;
    Ok(())
}

// ---------------------------------------------------------------------------
// Player actions
// ---------------------------------------------------------------------------

#[derive(Accounts, Session)]
#[instruction(seat: u8)]
pub struct SubmitMove<'info> {
    pub player: Signer<'info>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[seat]], bump = hand.bump)]
    pub hand: Box<Account<'info, Hand>>,
    /// CHECK: the wallet that holds the seat. A session's authority has to be this key.
    pub holder: UncheckedAccount<'info>,
    /// Present when `player` is a session key authorized by `holder`.
    #[session(signer = player, authority = holder.key())]
    pub session_token: Option<Account<'info, SessionTokenV2>>,
}

/// Seals a move for the current turn. Only the seat's wallet can read it
/// back until the turn resolves. In transfer windows this is "ready".
/// `player` is either the seat's wallet or a session key that wallet authorized.
#[session_auth_or(
    ctx.accounts.holder.key() == ctx.accounts.player.key(),
    SessionError::InvalidToken
)]
pub fn submit_move(ctx: Context<SubmitMove>, seat: u8, value: u32) -> Result<()> {
    let t = now()?;
    let game = &mut ctx.accounts.game;
    game.require_playable(t)?;
    let i = game.seat_index(seat)?;
    game.require_holder(i, &ctx.accounts.holder.key())?;
    require!(!game.has_acted(i), MoneyError::AlreadyActed);
    let hand = &mut ctx.accounts.hand;
    game.validate_move(i, value, hand.supply)?;

    let stamp = game.stamp();
    touch(hand, t, stamp)?;
    hand.value = value;
    hand.turn_stamp = stamp;
    game.seats[i].acted = stamp;
    Ok(())
}

/// Takes back the move sealed for this turn. Only while the discussion is
/// still open, and no more often than `SEAL_COOLDOWN_SECS`.
#[session_auth_or(
    ctx.accounts.holder.key() == ctx.accounts.player.key(),
    SessionError::InvalidToken
)]
pub fn unseal_move(ctx: Context<SubmitMove>, seat: u8) -> Result<()> {
    let t = now()?;
    let game = &mut ctx.accounts.game;
    game.require_playable(t)?;
    require!(game.discussion_open(t), MoneyError::DecisionOpen);
    let i = game.seat_index(seat)?;
    game.require_holder(i, &ctx.accounts.holder.key())?;
    require!(game.has_acted(i), MoneyError::NotSealed);

    let stamp = game.stamp();
    let hand = &mut ctx.accounts.hand;
    touch(hand, t, stamp)?;
    hand.turn_stamp = 0;
    hand.value = 0;
    game.seats[i].acted = 0;
    Ok(())
}

/// Records a seal or unseal, rejecting a second one inside the cooldown.
/// A new turn starts the clock over: the last touch only counts for its own turn.
fn touch(hand: &mut Hand, now: i64, stamp: u8) -> Result<()> {
    let same_turn = hand.touched_stamp == stamp;
    require!(
        !same_turn
            || hand.touched_ts == 0
            || now.saturating_sub(hand.touched_ts) >= SEAL_COOLDOWN_SECS,
        MoneyError::TooSoon
    );
    hand.touched_ts = now;
    hand.touched_stamp = stamp;
    Ok(())
}

#[derive(Accounts, Session)]
pub struct SeatAction<'info> {
    pub player: Signer<'info>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    /// CHECK: the wallet that holds the seat. A session's authority has to be this key.
    pub holder: UncheckedAccount<'info>,
    /// Present when `player` is a session key authorized by `holder`.
    #[session(signer = player, authority = holder.key())]
    pub session_token: Option<Account<'info, SessionTokenV2>>,
}

fn require_dealer(game: &Game, seat: u8, signer: &Pubkey) -> Result<usize> {
    require!(game.kind() == K_DEAL_PROPOSE, MoneyError::WrongTurn);
    let i = game.seat_index(seat)?;
    require!(i == game.dealer as usize, MoneyError::NotDealer);
    game.require_holder(i, signer)?;
    Ok(i)
}

/// The dealer splits the 5,000-point lump; whatever is not offered is
/// theirs if the table accepts.
#[session_auth_or(
    ctx.accounts.holder.key() == ctx.accounts.player.key(),
    SessionError::InvalidToken
)]
pub fn propose_offer(ctx: Context<SeatAction>, seat: u8, offer: [u32; SEATS]) -> Result<()> {
    let t = now()?;
    let game = &mut ctx.accounts.game;
    game.require_playable(t)?;
    let i = require_dealer(game, seat, &ctx.accounts.holder.key())?;
    game.seats[i].acted = game.stamp();
    game.propose(offer, t)
}

/// The dealer walks away with the guaranteed 15% and the game ends.
#[session_auth_or(
    ctx.accounts.holder.key() == ctx.accounts.player.key(),
    SessionError::InvalidToken
)]
pub fn take_guaranteed(ctx: Context<SeatAction>, seat: u8) -> Result<()> {
    let t = now()?;
    let game = &mut ctx.accounts.game;
    game.require_playable(t)?;
    require_dealer(game, seat, &ctx.accounts.holder.key())?;
    game.take_guaranteed(t);
    Ok(())
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct Tick<'info> {
    pub payer: Signer<'info>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, seeds = [RECORD_SEED, &game.game_id.to_le_bytes()], bump = record.bump)]
    pub record: Box<Account<'info, Record>>,
    pub hands: Hands<'info>,
}

/// Permissionless. Resolves the current turn once its deadline has passed
/// (or everyone has moved): voids seats that sat out, files the sealed moves
/// and payouts in the record, posts medals (and a finished mini-game's
/// totals), and advances the schedule.
pub fn tick(ctx: Context<Tick>) -> Result<()> {
    let t = now()?;
    let game = &mut ctx.accounts.game;
    require!(game.status == STATUS_LIVE, MoneyError::NotLive);
    require!(game.pending == 0, MoneyError::AwaitingRandomness);
    require!(
        t > game.turn_deadline || game.everyone_moved(),
        MoneyError::TurnNotOver
    );
    let moves = ctx.accounts.hands.moves(game)?;
    let rec = &mut ctx.accounts.record;
    let from = game.turn;
    game.resolve_turn(rec, &moves, t);
    ctx.accounts.hands.show_supplies(rec);
    ctx.accounts.hands.show_results(rec, from);
    Ok(())
}

#[derive(Accounts)]
pub struct SyncBalances<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
}

/// Crank-only, during a transfer window: copies the `Ledger`'s balances, so
/// The Deal's dealer is picked on what each seat really holds.
pub fn sync_balances(ctx: Context<SyncBalances>, balances: [u32; SEATS]) -> Result<()> {
    require_keys_eq!(ctx.accounts.payer.key(), ctx.accounts.config.crank, MoneyError::NotCrank);
    ctx.accounts.game.sync_balances(balances)
}

// ---------------------------------------------------------------------------
// Randomness (VRF output from the base-layer `Draw` mailbox)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct ApplyDraw<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(seeds = [DRAW_SEED], bump = draw.bump)]
    pub draw: Box<Account<'info, Draw>>,
    #[account(address = config.registry)]
    pub registry: AccountLoader<'info, Registry>,
    #[account(mut, seeds = [RECORD_SEED, &game.game_id.to_le_bytes()], bump = record.bump)]
    pub record: Box<Account<'info, Record>>,
    pub hands: Hands<'info>,
}

/// Permissionless, runs in the PER. Finishes a parked raffle and fills
/// empty / voided seats from the holder registry.
pub fn apply_draw(ctx: Context<ApplyDraw>) -> Result<()> {
    let game = &mut ctx.accounts.game;
    let draw = &ctx.accounts.draw;
    require!(game.pending != 0, MoneyError::NothingPending);
    require!(
        game.status == STATUS_SEATING || game.status == STATUS_LIVE,
        MoneyError::NotLive
    );
    require!(
        draw.fulfilled_nonce > game.draw_consumed && draw.fulfilled_nonce == draw.requested_nonce,
        MoneyError::DrawNotReady
    );
    game.draw_consumed = draw.fulfilled_nonce;

    let moves = ctx.accounts.hands.moves(game)?;
    let registry = ctx.accounts.registry.load()?;
    let t = Clock::get()?.unix_timestamp;
    let rec = &mut ctx.accounts.record;
    let from = game.turn;
    game.apply_randomness(rec, &draw.randomness, &moves, registry.active(), t);
    ctx.accounts.hands.show_results(rec, from);
    Ok(())
}

// ---------------------------------------------------------------------------
// Settlement: commit + undelegate back to the base layer for claims. This is
// when every round's moves and payouts in the record become public.
// ---------------------------------------------------------------------------

#[commit]
#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[0]], bump = hand0.bump)]
    pub hand0: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[1]], bump = hand1.bump)]
    pub hand1: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[2]], bump = hand2.bump)]
    pub hand2: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[3]], bump = hand3.bump)]
    pub hand3: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[4]], bump = hand4.bump)]
    pub hand4: Box<Account<'info, Hand>>,
    #[account(mut, seeds = [RECORD_SEED, &game.game_id.to_le_bytes()], bump = record.bump)]
    pub record: Box<Account<'info, Record>>,
}

pub fn settle(ctx: Context<Settle>) -> Result<()> {
    require!(
        ctx.accounts.game.status == STATUS_FINISHED,
        MoneyError::NotFinished
    );
    let a = &ctx.accounts;
    MagicIntentBundleBuilder::new(
        a.payer.to_account_info(),
        a.magic_context.to_account_info(),
        a.magic_program.to_account_info(),
    )
    .commit_and_undelegate(&[
        a.game.to_account_info(),
        a.hand0.to_account_info(),
        a.hand1.to_account_info(),
        a.hand2.to_account_info(),
        a.hand3.to_account_info(),
        a.hand4.to_account_info(),
        a.record.to_account_info(),
    ])
    .build_and_invoke()?;
    Ok(())
}
