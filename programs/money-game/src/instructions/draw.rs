//! MagicBlock VRF. Requests and callbacks run on the base layer and land in
//! the `Draw` mailbox; the PER reads that mailbox (as a cloned, read-only
//! account) and consumes each fulfilled nonce exactly once.
//!
//! Randomness decides who gets seated, who replaces a voided seat, and who
//! wins each raffle. Raffle tickets are sealed in the PER until the draw is
//! applied, so nobody who sees the randomness first can grind on it.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{vrf, vrf_callback};
use ephemeral_rollups_sdk::vrf::instructions::{
    create_request_randomness_ix, RequestRandomnessParams,
};
use ephemeral_rollups_sdk::vrf::types::SerializableAccountMeta;

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

/// If an oracle never answers, the crank may re-request after this many slots.
const REQUEST_TIMEOUT_SLOTS: u64 = 150;

#[vrf]
#[derive(Accounts)]
pub struct RequestDraw<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds = [DRAW_SEED], bump = draw.bump)]
    pub draw: Account<'info, Draw>,
    /// CHECK: validated by the VRF program.
    #[account(mut)]
    pub oracle_queue: UncheckedAccount<'info>,
}

/// Crank-only: one outstanding request at a time.
pub fn request_draw(ctx: Context<RequestDraw>) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.payer.key(),
        ctx.accounts.config.crank,
        MoneyError::NotCrank
    );
    let slot = Clock::get()?.slot;
    let draw = &mut ctx.accounts.draw;
    let outstanding = draw.requested_nonce != draw.fulfilled_nonce;
    require!(
        !outstanding || slot > draw.requested_slot + REQUEST_TIMEOUT_SLOTS,
        MoneyError::DrawNotReady
    );
    let nonce = draw.requested_nonce + 1;
    draw.requested_nonce = nonce;
    draw.requested_slot = slot;

    let mut caller_seed = [0u8; 32];
    caller_seed[..8].copy_from_slice(&nonce.to_le_bytes());
    caller_seed[8..16].copy_from_slice(&slot.to_le_bytes());

    let ix = create_request_randomness_ix(RequestRandomnessParams {
        payer: ctx.accounts.payer.key(),
        oracle_queue: ctx.accounts.oracle_queue.key(),
        callback_program_id: crate::ID,
        callback_discriminator: crate::instruction::OnDraw::DISCRIMINATOR.to_vec(),
        caller_seed,
        accounts_metas: Some(vec![SerializableAccountMeta {
            pubkey: draw.key(),
            is_signer: false,
            is_writable: true,
        }]),
        callback_args: Some(nonce.to_le_bytes().to_vec()),
        ..Default::default()
    });
    ctx.accounts
        .invoke_signed_vrf(&ctx.accounts.payer.to_account_info(), &ix)?;
    Ok(())
}

#[vrf_callback]
#[derive(Accounts)]
pub struct OnDraw<'info> {
    #[account(mut, seeds = [DRAW_SEED], bump = draw.bump)]
    pub draw: Account<'info, Draw>,
}

pub fn on_draw(ctx: Context<OnDraw>, randomness: [u8; 32], nonce: u64) -> Result<()> {
    let draw = &mut ctx.accounts.draw;
    // Late answers to a superseded request are ignored.
    if nonce == draw.requested_nonce && nonce > draw.fulfilled_nonce {
        draw.randomness = randomness;
        draw.fulfilled_nonce = nonce;
        draw.fulfilled_slot = Clock::get()?.slot;
    }
    Ok(())
}
