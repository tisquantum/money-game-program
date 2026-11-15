use anchor_lang::prelude::*;

const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
const ATA_PROGRAM_ID: Pubkey = pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

/// Associated token account for (owner, mint) under the mint's own token
/// program (pump.fun mints may be classic SPL or Token-2022).
fn is_associated(token_account: &Pubkey, owner: &Pubkey, mint: &AccountInfo) -> bool {
    let tp = *mint.owner;
    if tp != TOKEN_PROGRAM_ID && tp != TOKEN_2022_PROGRAM_ID {
        return false;
    }
    let (ata, _) = Pubkey::find_program_address(
        &[owner.as_ref(), tp.as_ref(), mint.key.as_ref()],
        &ATA_PROGRAM_ID,
    );
    ata == *token_account
}

/// Reads (mint, owner, amount) from a classic or Token-2022 account. The
/// base layout is shared, extensions only append.
fn read_token_account(ai: &AccountInfo) -> Option<(Pubkey, Pubkey, u64)> {
    if *ai.owner != TOKEN_PROGRAM_ID && *ai.owner != TOKEN_2022_PROGRAM_ID {
        return None;
    }
    let data = ai.try_borrow_data().ok()?;
    if data.len() < 72 {
        return None;
    }
    let mint = Pubkey::try_from(&data[0..32]).ok()?;
    let owner = Pubkey::try_from(&data[32..64]).ok()?;
    let amount = u64::from_le_bytes(data[64..72].try_into().ok()?);
    Some((mint, owner, amount))
}

fn holds_enough(ai: &AccountInfo, owner: &Pubkey, config: &Config) -> bool {
    matches!(read_token_account(ai), Some((m, o, a)) if m == config.mint && o == *owner && a >= config.min_hold)
}

/// Swap-remove `index`; the entry that moved into its slot is re-pointed.
fn swap_remove<'info>(
    registry: &AccountLoader<'info, Registry>,
    index: u32,
    moved_entry: &mut Option<Account<'info, HolderEntry>>,
) -> Result<()> {
    let mut r = registry.load_mut()?;
    let last = r.count.checked_sub(1).ok_or(MoneyError::RegistryMismatch)?;
    if index != last {
        let moved = r.holders[last as usize];
        let entry = moved_entry.as_mut().ok_or(MoneyError::RegistryMismatch)?;
        require_keys_eq!(entry.wallet, moved, MoneyError::RegistryMismatch);
        r.holders[index as usize] = moved;
        entry.index = index;
    }
    r.holders[last as usize] = Pubkey::default();
    r.count = last;
    Ok(())
}

#[derive(Accounts)]
pub struct EnterRegistry<'info> {
    #[account(mut)]
    pub holder: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, address = config.registry)]
    pub registry: AccountLoader<'info, Registry>,
    #[account(
        init,
        payer = holder,
        space = 8 + HolderEntry::INIT_SPACE,
        seeds = [HOLDER_SEED, holder.key().as_ref()],
        bump
    )]
    pub entry: Account<'info, HolderEntry>,
    /// CHECK: the configured mint; its owner tells us which token program.
    #[account(address = config.mint)]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: parsed manually so classic SPL and Token-2022 (pump.fun) both work.
    pub token_account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn enter_registry(ctx: Context<EnterRegistry>) -> Result<()> {
    let holder = ctx.accounts.holder.key();
    let config = &ctx.accounts.config;
    let ta = &ctx.accounts.token_account;
    require!(
        is_associated(&ta.key(), &holder, &ctx.accounts.mint.to_account_info()),
        MoneyError::NotAssociatedTokenAccount
    );
    let (mint, owner, amount) =
        read_token_account(&ta.to_account_info()).ok_or(MoneyError::WrongMint)?;
    require_keys_eq!(mint, config.mint, MoneyError::WrongMint);
    require_keys_eq!(owner, holder, MoneyError::WrongTokenOwner);
    require!(amount >= config.min_hold, MoneyError::NotEnoughTokens);

    let mut r = ctx.accounts.registry.load_mut()?;
    require!((r.count as usize) < REGISTRY_CAPACITY, MoneyError::RegistryFull);
    let index = r.count;
    r.holders[index as usize] = holder;
    r.count += 1;

    let e = &mut ctx.accounts.entry;
    e.wallet = holder;
    e.index = index;
    e.bump = ctx.bumps.entry;
    Ok(())
}

#[derive(Accounts)]
pub struct LeaveRegistry<'info> {
    #[account(mut)]
    pub holder: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, address = config.registry)]
    pub registry: AccountLoader<'info, Registry>,
    #[account(
        mut,
        close = holder,
        seeds = [HOLDER_SEED, holder.key().as_ref()],
        bump = entry.bump
    )]
    pub entry: Account<'info, HolderEntry>,
    /// Entry of the wallet currently in the last registry slot (omit if
    /// that is the leaver).
    #[account(mut)]
    pub moved_entry: Option<Account<'info, HolderEntry>>,
}

pub fn leave_registry(ctx: Context<LeaveRegistry>) -> Result<()> {
    swap_remove(
        &ctx.accounts.registry,
        ctx.accounts.entry.index,
        &mut ctx.accounts.moved_entry,
    )
}

/// Permissionless clean-up: removes a wallet whose associated token
/// account no longer meets the minimum. Rent goes back to the holder.
#[derive(Accounts)]
pub struct PruneHolder<'info> {
    pub pruner: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(mut, address = config.registry)]
    pub registry: AccountLoader<'info, Registry>,
    #[account(
        mut,
        close = holder,
        seeds = [HOLDER_SEED, holder.key().as_ref()],
        bump = entry.bump
    )]
    pub entry: Account<'info, HolderEntry>,
    /// CHECK: rent refund target; bound by the entry's seeds.
    #[account(mut)]
    pub holder: UncheckedAccount<'info>,
    /// CHECK: the configured mint; its owner tells us which token program.
    #[account(address = config.mint)]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: the holder's ATA for the mint (may be closed).
    pub token_account: UncheckedAccount<'info>,
    #[account(mut)]
    pub moved_entry: Option<Account<'info, HolderEntry>>,
}

pub fn prune_holder(ctx: Context<PruneHolder>) -> Result<()> {
    let holder = ctx.accounts.holder.key();
    let config = &ctx.accounts.config;
    let ta = &ctx.accounts.token_account;
    require!(
        is_associated(&ta.key(), &holder, &ctx.accounts.mint.to_account_info()),
        MoneyError::NotAssociatedTokenAccount
    );
    require!(
        !holds_enough(&ta.to_account_info(), &holder, config),
        MoneyError::StillEligible
    );
    swap_remove(
        &ctx.accounts.registry,
        ctx.accounts.entry.index,
        &mut ctx.accounts.moved_entry,
    )
}
