use anchor_lang::prelude::*;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount};
use ephemeral_rollups_sdk::access_control::structs::EphemeralPermission;
use ephemeral_rollups_sdk::anchor::delegate;
use ephemeral_rollups_sdk::cpi::DelegateConfig;

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

fn system_transfer<'info>(
    system_program: &Program<'info, System>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    lamports: u64,
    signer: &[&[&[u8]]],
) -> Result<()> {
    if lamports == 0 {
        return Ok(());
    }
    transfer(
        CpiContext::new_with_signer(system_program.key(), Transfer { from, to }, signer),
        lamports,
    )
}

/// Moves pool tokens out of an account the treasury PDA controls.
fn treasury_pay<'info>(
    token_program: &Program<'info, Token>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    treasury: AccountInfo<'info>,
    treasury_bump: u8,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let seeds: &[&[u8]] = &[TREASURY_SEED, &[treasury_bump]];
    token::transfer(
        CpiContext::new_with_signer(
            token_program.key(),
            token::Transfer { from, to, authority: treasury },
            &[seeds],
        ),
        amount,
    )
}

fn permission_rent(members: usize) -> u64 {
    ephemeral_rollups_sdk::ephemeral_accounts::rent(EphemeralPermission::size_of(members) as u32)
}

// ---------------------------------------------------------------------------
// start_game: opens the next table on the base layer and escrows its pool.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct StartGame<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: treasury PDA, authority over the treasury's pool and every vault.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(address = config.pool_mint)]
    pub pool_mint: Box<Account<'info, Mint>>,
    #[account(mut, associated_token::mint = pool_mint, associated_token::authority = treasury)]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    /// The house's pool-token account; receives the house cut.
    #[account(mut, token::mint = pool_mint, token::authority = config.house)]
    pub house_pool: Box<Account<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        space = 8 + Game::INIT_SPACE,
        seeds = [GAME_SEED, &(config.game_count + 1).to_le_bytes()],
        bump
    )]
    pub game: Box<Account<'info, Game>>,
    #[account(init, payer = payer, space = 8 + Hand::INIT_SPACE,
        seeds = [HAND_SEED, &(config.game_count + 1).to_le_bytes(), &[0]], bump)]
    pub hand0: Box<Account<'info, Hand>>,
    #[account(init, payer = payer, space = 8 + Hand::INIT_SPACE,
        seeds = [HAND_SEED, &(config.game_count + 1).to_le_bytes(), &[1]], bump)]
    pub hand1: Box<Account<'info, Hand>>,
    #[account(init, payer = payer, space = 8 + Hand::INIT_SPACE,
        seeds = [HAND_SEED, &(config.game_count + 1).to_le_bytes(), &[2]], bump)]
    pub hand2: Box<Account<'info, Hand>>,
    #[account(init, payer = payer, space = 8 + Hand::INIT_SPACE,
        seeds = [HAND_SEED, &(config.game_count + 1).to_le_bytes(), &[3]], bump)]
    pub hand3: Box<Account<'info, Hand>>,
    #[account(init, payer = payer, space = 8 + Hand::INIT_SPACE,
        seeds = [HAND_SEED, &(config.game_count + 1).to_le_bytes(), &[4]], bump)]
    pub hand4: Box<Account<'info, Hand>>,
    #[account(init, payer = payer, space = 8 + Record::INIT_SPACE,
        seeds = [RECORD_SEED, &(config.game_count + 1).to_le_bytes()], bump)]
    pub record: Box<Account<'info, Record>>,
    #[account(init, payer = payer, space = 8 + Ledger::INIT_SPACE,
        seeds = [LEDGER_SEED, &(config.game_count + 1).to_le_bytes()], bump)]
    pub ledger: Box<Account<'info, Ledger>>,
    /// This table's escrow, in the pool token, controlled by the treasury PDA.
    #[account(
        init,
        payer = payer,
        seeds = [VAULT_SEED, &(config.game_count + 1).to_le_bytes()],
        bump,
        token::mint = pool_mint,
        token::authority = treasury,
    )]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(seeds = [DRAW_SEED], bump = draw.bump)]
    pub draw: Box<Account<'info, Draw>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn start_game(ctx: Context<StartGame>) -> Result<()> {
    let config = &mut ctx.accounts.config;
    require!(!config.paused, MoneyError::Paused);
    require!(config.active_game == 0, MoneyError::TableAlreadyRunning);

    let available = ctx.accounts.treasury_pool.amount;
    let pool = (available as u128 * config.pool_bps as u128 / 10_000) as u64;
    let house_cut = (pool as u128 * config.house_bps as u128 / 10_000) as u64;
    let escrow = pool - house_cut;
    require!(escrow >= config.min_pool && escrow > 0, MoneyError::PoolTooSmall);
    if config.game_count == 0 {
        // Passed as a remaining account: StartGame is already at Solana's stack limit.
        let registry = ctx.remaining_accounts.first().ok_or(MoneyError::RegistryMismatch)?;
        require_keys_eq!(registry.key(), config.registry, MoneyError::RegistryMismatch);
        let data = registry.try_borrow_data()?;
        require!(data.len() >= 12, MoneyError::NotEnoughPlayers);
        let registered = u32::from_le_bytes(data[8..12].try_into().unwrap());
        require!(registered >= FIRST_TABLE_REGISTRANTS, MoneyError::NotEnoughPlayers);
    }

    let game_id = config.game_count + 1;
    let tp = &ctx.accounts.token_program;
    let treasury = ctx.accounts.treasury.to_account_info();
    treasury_pay(
        tp,
        ctx.accounts.treasury_pool.to_account_info(),
        ctx.accounts.vault.to_account_info(),
        treasury.clone(),
        config.treasury_bump,
        escrow,
    )?;
    treasury_pay(
        tp,
        ctx.accounts.treasury_pool.to_account_info(),
        ctx.accounts.house_pool.to_account_info(),
        treasury,
        config.treasury_bump,
        house_cut,
    )?;
    let sp = &ctx.accounts.system_program;

    // Pre-fund ephemeral permission rent; those lamports travel with the
    // PDAs into the PER, where each PDA pays for its own permission.
    system_transfer(
        sp,
        ctx.accounts.payer.to_account_info(),
        ctx.accounts.game.to_account_info(),
        permission_rent(0),
        &[],
    )?;
    let hands = [
        &mut ctx.accounts.hand0,
        &mut ctx.accounts.hand1,
        &mut ctx.accounts.hand2,
        &mut ctx.accounts.hand3,
        &mut ctx.accounts.hand4,
    ];
    let hand_bumps = [
        ctx.bumps.hand0,
        ctx.bumps.hand1,
        ctx.bumps.hand2,
        ctx.bumps.hand3,
        ctx.bumps.hand4,
    ];
    for (i, hand) in hands.into_iter().enumerate() {
        system_transfer(
            sp,
            ctx.accounts.payer.to_account_info(),
            hand.to_account_info(),
            permission_rent(1),
            &[],
        )?;
        hand.game_id = game_id;
        hand.seat = i as u8;
        hand.bump = hand_bumps[i];
        hand.turn_stamp = 0;
        hand.value = 0;
        hand.member = Pubkey::default();
        hand.touched_ts = 0;
        hand.touched_stamp = 0;
        hand.supply = 0;
        hand.result_stamp = 0;
        hand.result = 0;
        hand.result_won = false;
    }
    system_transfer(
        sp,
        ctx.accounts.payer.to_account_info(),
        ctx.accounts.record.to_account_info(),
        permission_rent(0),
        &[],
    )?;
    let record = &mut ctx.accounts.record;
    record.game_id = game_id;
    record.bump = ctx.bumps.record;
    let ledger = &mut ctx.accounts.ledger;
    ledger.game_id = game_id;
    ledger.bump = ctx.bumps.ledger;

    let now = Clock::get()?.unix_timestamp;
    let g = &mut ctx.accounts.game;
    g.game_id = game_id;
    g.bump = ctx.bumps.game;
    g.vault_bump = ctx.bumps.vault;
    g.status = STATUS_SEATING;
    g.turn = 0;
    g.pending = PENDING_SEATS;
    g.created_ts = now;
    g.escrow = escrow;
    g.draw_consumed = ctx.accounts.draw.fulfilled_nonce;
    g.dealer = NO_SEAT;
    g.reveal.winner = NO_SEAT;

    config.game_count = game_id;
    config.active_game = game_id;
    msg!("table {} open, escrow {} pool-token units", game_id, escrow);
    Ok(())
}

// ---------------------------------------------------------------------------
// Delegation. Game, hands and record go to the TEE validator (Private ER); the
// ledger goes to the public ER. Crank-only so nobody can grief a table by
// delegating it somewhere else first.
// ---------------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum DelegateTarget {
    Game { game_id: u64 },
    Hand { game_id: u64, seat: u8 },
    Record { game_id: u64 },
    Ledger { game_id: u64 },
}

impl DelegateTarget {
    fn seeds(&self) -> Vec<Vec<u8>> {
        match self {
            Self::Game { game_id } => vec![GAME_SEED.to_vec(), game_id.to_le_bytes().to_vec()],
            Self::Hand { game_id, seat } => vec![
                HAND_SEED.to_vec(),
                game_id.to_le_bytes().to_vec(),
                vec![*seat],
            ],
            Self::Record { game_id } => vec![RECORD_SEED.to_vec(), game_id.to_le_bytes().to_vec()],
            Self::Ledger { game_id } => vec![LEDGER_SEED.to_vec(), game_id.to_le_bytes().to_vec()],
        }
    }
}

#[delegate]
#[derive(Accounts)]
pub struct DelegatePda<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// CHECK: PDA being delegated; seeds are checked by the delegation CPI.
    #[account(mut, del)]
    pub pda: UncheckedAccount<'info>,
    /// CHECK: must match the validator configured for this account kind.
    pub validator: UncheckedAccount<'info>,
}

pub fn delegate_pda(ctx: Context<DelegatePda>, target: DelegateTarget) -> Result<()> {
    let config = &ctx.accounts.config;
    require_keys_eq!(ctx.accounts.payer.key(), config.crank, MoneyError::NotCrank);
    let expected = match target {
        DelegateTarget::Ledger { .. } => config.er_validator,
        _ => config.tee_validator,
    };
    require_keys_eq!(ctx.accounts.validator.key(), expected, MoneyError::WrongValidator);

    let seeds = target.seeds();
    let seed_refs: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    ctx.accounts.delegate_pda(
        &ctx.accounts.payer,
        &seed_refs,
        DelegateConfig {
            validator: Some(expected),
            ..Default::default()
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// reclaim_hands: once a game is settled its hands and record are useless;
// their rent goes back to the crank that paid for them. The record is public by
// then, and anyone who wants the round history has had it since the commit.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct ReclaimHands<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    /// CHECK: receives the hands' rent; it paid for them.
    #[account(mut, address = config.crank)]
    pub crank: UncheckedAccount<'info>,
    #[account(mut, close = crank, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[0]], bump = hand0.bump)]
    pub hand0: Box<Account<'info, Hand>>,
    #[account(mut, close = crank, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[1]], bump = hand1.bump)]
    pub hand1: Box<Account<'info, Hand>>,
    #[account(mut, close = crank, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[2]], bump = hand2.bump)]
    pub hand2: Box<Account<'info, Hand>>,
    #[account(mut, close = crank, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[3]], bump = hand3.bump)]
    pub hand3: Box<Account<'info, Hand>>,
    #[account(mut, close = crank, seeds = [HAND_SEED, &game.game_id.to_le_bytes(), &[4]], bump = hand4.bump)]
    pub hand4: Box<Account<'info, Hand>>,
}

pub fn reclaim_hands(ctx: Context<ReclaimHands>) -> Result<()> {
    require!(ctx.accounts.game.status == STATUS_SETTLED, MoneyError::NotSettled);
    Ok(())
}

// ---------------------------------------------------------------------------
// finalize: once the PER has committed the finished game and the public ER its
// ledger, lock in per-seat payouts from the ledger's balances and send unearned
// pool tokens back to the treasury.
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct Finalize<'info> {
    pub payer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(seeds = [LEDGER_SEED, &game.game_id.to_le_bytes()], bump = ledger.bump)]
    pub ledger: Box<Account<'info, Ledger>>,
    #[account(mut, seeds = [VAULT_SEED, &game.game_id.to_le_bytes()], bump = game.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: treasury PDA, authority over the vault.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = config.pool_mint, associated_token::authority = treasury)]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn finalize(ctx: Context<Finalize>) -> Result<()> {
    let game = &mut ctx.accounts.game;
    require!(game.status == STATUS_FINISHED, MoneyError::NotFinished);
    let ledger = &ctx.accounts.ledger;
    require!(
        ledger.finished && ledger.relayed == game.log_seq,
        MoneyError::LedgerBehind
    );

    let distributable = ctx.accounts.vault.amount;
    let payouts = ledger.compute_payouts(distributable, &game.seats);
    let owed: u64 = payouts.iter().sum();
    let unearned = distributable - owed;

    treasury_pay(
        &ctx.accounts.token_program,
        ctx.accounts.vault.to_account_info(),
        ctx.accounts.treasury_pool.to_account_info(),
        ctx.accounts.treasury.to_account_info(),
        ctx.accounts.config.treasury_bump,
        unearned,
    )?;

    game.payouts = payouts;
    game.status = STATUS_SETTLED;
    game.claimed_mask = 0;
    game.claim_deadline = Clock::get()?.unix_timestamp + CLAIM_WINDOW_SECS;

    let config = &mut ctx.accounts.config;
    if config.active_game == game.game_id {
        config.active_game = 0;
    }
    msg!("table {} settled: {} owed, {} back to treasury", game.game_id, owed, unearned);
    Ok(())
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub claimant: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, seeds = [VAULT_SEED, &game.game_id.to_le_bytes()], bump = game.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: treasury PDA, authority over the vault.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(address = config.pool_mint)]
    pub pool_mint: Box<Account<'info, Mint>>,
    /// Where the winnings go; created for the claimant if they don't hold the token yet.
    #[account(
        init_if_needed,
        payer = claimant,
        associated_token::mint = pool_mint,
        associated_token::authority = claimant,
    )]
    pub claimant_pool: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Earnings only become claimable once the game is over and settled.
/// The wallet holding the seat at the end gets everything the seat earned.
pub fn claim(ctx: Context<Claim>, seat: u8) -> Result<()> {
    let game = &mut ctx.accounts.game;
    require!(game.status == STATUS_SETTLED, MoneyError::NotSettled);
    let i = game.seat_index(seat)?;
    require_keys_eq!(
        game.seats[i].wallet,
        ctx.accounts.claimant.key(),
        MoneyError::NotSeatHolder
    );
    require!(game.claimed_mask & (1 << i) == 0, MoneyError::AlreadyClaimed);
    game.claimed_mask |= 1 << i;

    treasury_pay(
        &ctx.accounts.token_program,
        ctx.accounts.vault.to_account_info(),
        ctx.accounts.claimant_pool.to_account_info(),
        ctx.accounts.treasury.to_account_info(),
        ctx.accounts.config.treasury_bump,
        game.payouts[i],
    )
}

/// After the claim window (or once everyone claimed), leftovers go back to
/// the treasury and the game account and its vault are closed.
#[derive(Accounts)]
pub struct Sweep<'info> {
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, close = crank, seeds = [GAME_SEED, &game.game_id.to_le_bytes()], bump = game.bump)]
    pub game: Box<Account<'info, Game>>,
    #[account(mut, close = crank, seeds = [LEDGER_SEED, &game.game_id.to_le_bytes()], bump = ledger.bump)]
    pub ledger: Box<Account<'info, Ledger>>,
    #[account(mut, close = crank, seeds = [RECORD_SEED, &game.game_id.to_le_bytes()], bump = record.bump)]
    pub record: Box<Account<'info, Record>>,
    #[account(mut, seeds = [VAULT_SEED, &game.game_id.to_le_bytes()], bump = game.vault_bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: treasury PDA, authority over the vault.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = config.pool_mint, associated_token::authority = treasury)]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    /// CHECK: receives the game, ledger, record and vault rent; it paid for them.
    #[account(mut, address = config.crank)]
    pub crank: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn sweep(ctx: Context<Sweep>) -> Result<()> {
    let game = &ctx.accounts.game;
    require!(game.status == STATUS_SETTLED, MoneyError::NotSettled);
    let owed_mask = game
        .payouts
        .iter()
        .enumerate()
        .filter(|(_, p)| **p > 0)
        .fold(0u8, |m, (i, _)| m | (1 << i));
    let all_claimed = game.claimed_mask & owed_mask == owed_mask;
    require!(
        all_claimed || Clock::get()?.unix_timestamp > game.claim_deadline,
        MoneyError::ClaimWindowOpen
    );
    let bump = ctx.accounts.config.treasury_bump;
    let treasury = ctx.accounts.treasury.to_account_info();
    treasury_pay(
        &ctx.accounts.token_program,
        ctx.accounts.vault.to_account_info(),
        ctx.accounts.treasury_pool.to_account_info(),
        treasury.clone(),
        bump,
        ctx.accounts.vault.amount,
    )?;
    let seeds: &[&[u8]] = &[TREASURY_SEED, &[bump]];
    token::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        token::CloseAccount {
            account: ctx.accounts.vault.to_account_info(),
            destination: ctx.accounts.crank.to_account_info(),
            authority: treasury,
        },
        &[seeds],
    ))
}
