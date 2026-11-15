use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount};

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ConfigParams {
    pub crank: Pubkey,
    pub mint: Pubkey,
    pub min_hold: u64,
    pub pool_bps: u16,
    pub house_bps: u16,
    pub house: Pubkey,
    pub min_pool: u64,
    pub tee_validator: Pubkey,
    pub er_validator: Pubkey,
}

impl ConfigParams {
    fn apply(&self, c: &mut Config) -> Result<()> {
        require!(self.pool_bps <= 10_000, MoneyError::BadBps);
        require!(self.house_bps <= 10_000, MoneyError::BadBps);
        c.crank = self.crank;
        c.mint = self.mint;
        c.min_hold = self.min_hold;
        c.pool_bps = self.pool_bps;
        c.house_bps = self.house_bps;
        c.house = self.house;
        c.min_pool = self.min_pool;
        c.tee_validator = self.tee_validator;
        c.er_validator = self.er_validator;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = admin,
        space = 8 + Draw::INIT_SPACE,
        seeds = [DRAW_SEED],
        bump
    )]
    pub draw: Account<'info, Draw>,
    /// CHECK: treasury PDA. Holds nothing itself: it is the authority over the
    /// treasury's pool-token account and every table's vault.
    #[account(seeds = [TREASURY_SEED], bump)]
    pub treasury: UncheckedAccount<'info>,
    /// The token pools are held and paid in (USDC). Can't be changed later.
    pub pool_mint: Box<Account<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = admin,
        associated_token::mint = pool_mint,
        associated_token::authority = treasury,
    )]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn init_config(ctx: Context<InitConfig>, params: ConfigParams) -> Result<()> {
    let c = &mut ctx.accounts.config;
    c.admin = ctx.accounts.admin.key();
    params.apply(c)?;
    c.pool_mint = ctx.accounts.pool_mint.key();
    c.registry = Pubkey::default();
    c.game_count = 0;
    c.active_game = 0;
    c.paused = false;
    c.bump = ctx.bumps.config;
    c.treasury_bump = ctx.bumps.treasury;
    ctx.accounts.draw.bump = ctx.bumps.draw;
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ MoneyError::NotAdmin)]
    pub config: Account<'info, Config>,
}

pub fn update_config(ctx: Context<UpdateConfig>, params: ConfigParams, paused: bool) -> Result<()> {
    let c = &mut ctx.accounts.config;
    params.apply(c)?;
    c.paused = paused;
    Ok(())
}

/// Writes off a table whose rollup can no longer commit it back: its escrow
/// returns to the treasury and the next table may open. The game and hand
/// accounts stay delegated and are simply never used again.
#[derive(Accounts)]
#[instruction(game_id: u64)]
pub struct AbandonGame<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ MoneyError::NotAdmin)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [VAULT_SEED, &game_id.to_le_bytes()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    /// CHECK: treasury PDA, authority over the vault.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = config.pool_mint, associated_token::authority = treasury)]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn abandon_game(ctx: Context<AbandonGame>, game_id: u64) -> Result<()> {
    require!(ctx.accounts.config.active_game == game_id, MoneyError::NotActiveGame);
    let seeds: &[&[u8]] = &[TREASURY_SEED, &[ctx.accounts.config.treasury_bump]];
    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            token::Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.treasury_pool.to_account_info(),
                authority: ctx.accounts.treasury.to_account_info(),
            },
            &[seeds],
        ),
        ctx.accounts.vault.amount,
    )?;
    ctx.accounts.config.active_game = 0;
    msg!("table {} abandoned; escrow back to treasury", game_id);
    Ok(())
}

/// Claims a registry account the admin created client-side at full size
/// (`Registry::SIZE` bytes, owned by this program).
#[derive(Accounts)]
pub struct InitRegistry<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ MoneyError::NotAdmin)]
    pub config: Account<'info, Config>,
    #[account(zero)]
    pub registry: AccountLoader<'info, Registry>,
}

pub fn init_registry(ctx: Context<InitRegistry>) -> Result<()> {
    let mut r = ctx.accounts.registry.load_init()?;
    r.count = 0;
    ctx.accounts.config.registry = ctx.accounts.registry.key();
    Ok(())
}

/// Anyone can top up the treasury. The keeper sweeps pump.fun creator
/// rewards (USDC) in here; each table's escrow is carved out of it.
#[derive(Accounts)]
pub struct FundTreasury<'info> {
    pub funder: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, token::mint = config.pool_mint, token::authority = funder)]
    pub funder_pool: Box<Account<'info, TokenAccount>>,
    /// CHECK: treasury PDA, authority over `treasury_pool`.
    #[account(seeds = [TREASURY_SEED], bump = config.treasury_bump)]
    pub treasury: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = config.pool_mint, associated_token::authority = treasury)]
    pub treasury_pool: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
}

pub fn fund_treasury(ctx: Context<FundTreasury>, amount: u64) -> Result<()> {
    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            token::Transfer {
                from: ctx.accounts.funder_pool.to_account_info(),
                to: ctx.accounts.treasury_pool.to_account_info(),
                authority: ctx.accounts.funder.to_account_info(),
            },
        ),
        amount,
    )
}
