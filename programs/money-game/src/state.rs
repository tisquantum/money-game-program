use anchor_lang::prelude::*;

use crate::constants::*;

/// Global settings. Lives on the base layer; the PER reads a cloned copy.
#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    /// Keeper key: starts tables, delegates accounts, requests VRF.
    pub crank: Pubkey,
    /// The pump.fun token whose holders can be seated.
    pub mint: Pubkey,
    /// Minimum raw token amount (base units) to register.
    pub min_hold: u64,
    /// Share of the treasury moved into each game's escrow (bps).
    pub pool_bps: u16,
    /// Optional house cut of each pool (bps), paid to `house`'s pool-token account at game start.
    pub house_bps: u16,
    pub house: Pubkey,
    /// The token every pool is held and paid out in (USDC). Fixed at init.
    pub pool_mint: Pubkey,
    /// No table starts if its escrow (pool-token base units) would be smaller than this.
    pub min_pool: u64,
    /// TEE validator the game + hands are delegated to (Private ER).
    pub tee_validator: Pubkey,
    /// Public ER validator each game's `Ledger` is delegated to.
    pub er_validator: Pubkey,
    pub registry: Pubkey,
    pub game_count: u64,
    /// Id of the table currently running, 0 when none.
    pub active_game: u64,
    pub paused: bool,
    pub bump: u8,
    pub treasury_bump: u8,
}

/// Opt-in registry of wallets that proved they hold the token.
/// Created client-side at full size (too big for a CPI init), then claimed
/// with `#[account(zero)]`.
#[account(zero_copy)]
#[repr(C)]
pub struct Registry {
    pub count: u32,
    pub _pad: u32,
    pub holders: [Pubkey; REGISTRY_CAPACITY],
}

impl Registry {
    pub const SIZE: usize = 8 + 4 + 4 + 32 * REGISTRY_CAPACITY;

    pub fn active(&self) -> &[Pubkey] {
        &self.holders[..self.count as usize]
    }
}

/// One per registered wallet; prevents duplicates and remembers the slot.
#[account]
#[derive(InitSpace)]
pub struct HolderEntry {
    pub wallet: Pubkey,
    pub index: u32,
    pub bump: u8,
}

/// Base-layer VRF mailbox. The oracle writes randomness here; the game in
/// the PER consumes each fulfilled nonce exactly once.
#[account]
#[derive(InitSpace)]
pub struct Draw {
    pub requested_nonce: u64,
    pub requested_slot: u64,
    pub fulfilled_nonce: u64,
    pub randomness: [u8; 32],
    pub fulfilled_slot: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq)]
pub struct Seat {
    pub wallet: Pubkey,
    pub status: u8,
    /// Points this seat has been publicly credited: mini-game totals once each
    /// mini-game is over, The Deal, and transfers synced from the `Ledger`.
    /// Follows the seat, not the wallet.
    pub balance: u32,
    pub medals: u8,
    /// `turn + 1` of the last turn this seat acted in (0 = never).
    pub acted: u8,
    /// `turn + 1` of the turn this wallet was seated in. A wallet is never
    /// voided for the turn it arrived in.
    pub seated: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq)]
pub struct LogEntry {
    pub turn: u8,
    pub kind: u8,
    pub seat: u8,
    pub other: u8,
    pub amount: i32,
}

/// The most recently resolved turn, as far as the public may know: which
/// seats moved. The moves themselves stay in the private `Record`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq)]
pub struct Reveal {
    /// `turn + 1` of the resolved turn (0 = nothing yet).
    pub turn: u8,
    /// Bit i set = seat i submitted `values[i]`.
    pub mask: u8,
    pub values: [u32; SEATS],
    pub winner: u8,
    pub amount: i32,
}

/// Public table state. Delegated to the TEE validator for the whole game
/// with a *public* permission so anyone with a TEE token can watch.
#[account]
#[derive(InitSpace)]
pub struct Game {
    pub game_id: u64,
    pub bump: u8,
    pub vault_bump: u8,
    pub status: u8,
    pub turn: u8,
    pub pending: u8,
    pub created_ts: i64,
    pub live_ts: i64,
    pub turn_deadline: i64,
    pub finished_ts: i64,
    /// Pool-token base units locked in this game's vault.
    pub escrow: u64,
    /// Last VRF nonce consumed from the `Draw` mailbox.
    pub draw_consumed: u64,
    pub seats: [Seat; SEATS],
    pub excluded: [Pubkey; MAX_EXCLUDED],
    pub excluded_len: u8,
    /// Prize Case board for the current round; medal slots flagged in mask.
    pub prize_board: [i32; SEATS],
    pub prize_medal_mask: u8,
    pub prize_boost_next: bool,
    pub dealer: u8,
    pub offer: [u32; SEATS],
    pub offer_live: bool,
    pub reveal: Reveal,
    pub log: [LogEntry; LOG_LEN],
    pub log_seq: u32,
    // --- settlement (base layer) ---
    pub payouts: [u64; SEATS],
    pub claimed_mask: u8,
    pub claim_deadline: i64,
}

/// A seat's sealed move. Private permission: only the seat's wallet can
/// read it through the TEE RPC; the program reads all five to resolve.
#[account]
#[derive(InitSpace)]
pub struct Hand {
    pub game_id: u64,
    pub seat: u8,
    pub bump: u8,
    /// `turn + 1` this value was submitted for (0 = none).
    pub turn_stamp: u8,
    pub value: u32,
    /// Wallet currently allowed to read this hand.
    pub member: Pubkey,
    /// Unix time of the last seal or unseal, so a seat can't flicker its status.
    pub touched_ts: i64,
    /// `turn + 1` that `touched_ts` belongs to. A new turn starts the cooldown over.
    pub touched_stamp: u8,
    /// This seat's Money Makers supply, copied from the `Record` so only the
    /// seat's wallet can see it.
    pub supply: u32,
    /// `turn + 1` of the last round resolved for this seat (0 = none yet).
    pub result_stamp: u8,
    /// Points this seat won (or lost) in that round, from the `Record`.
    pub result: i32,
    /// This seat was that round's winner.
    pub result_won: bool,
}

/// One resolved turn, sealed until the game is committed to the base layer.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq)]
pub struct RoundRecord {
    /// `turn + 1` (0 = not resolved).
    pub stamp: u8,
    pub mask: u8,
    pub values: [u32; SEATS],
    /// Points each seat won or lost this turn.
    pub delta: [i32; SEATS],
    pub winner: u8,
    pub amount: i32,
}

/// Everything about the game nobody may see until it ends: every round's moves
/// and payouts, the money won in the mini-game in play, and Money Makers
/// supplies. Private permission with no members: only the program reads it,
/// and it becomes public when it is committed with the game.
#[account]
#[derive(InitSpace)]
pub struct Record {
    pub game_id: u64,
    pub bump: u8,
    /// Points won so far in the mini-game in play, posted when it ends.
    pub earned: [i32; SEATS],
    pub mm_supply: [u32; SEATS],
    pub rounds: [RoundRecord; TURN_COUNT],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq)]
pub struct LedgerSeat {
    pub wallet: Pubkey,
    pub active: bool,
    pub balance: u32,
    pub medals: u8,
}

/// The table's public money: balances, medals and transfers. Delegated to a
/// public ER so every change is an ordinary transaction anyone can inspect.
/// The keeper relays medals, mini-game totals and seat changes from the game
/// in order (`relayed` is the next game log seq); players transfer here
/// directly while `window` is open.
#[account]
#[derive(InitSpace)]
pub struct Ledger {
    pub game_id: u64,
    pub bump: u8,
    pub seats: [LedgerSeat; SEATS],
    pub relayed: u32,
    /// `turn + 1` of the open transfer window (0 = closed).
    pub window: u8,
    pub finished: bool,
    pub log: [LogEntry; LOG_LEN],
    pub log_seq: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The most a program can allocate when it creates an account itself.
    const MAX_CPI_ALLOC: usize = 10_240;

    #[test]
    fn program_created_accounts_fit_one_allocation() {
        for (name, size) in [
            ("game", 8 + Game::INIT_SPACE),
            ("hand", 8 + Hand::INIT_SPACE),
            ("record", 8 + Record::INIT_SPACE),
            ("ledger", 8 + Ledger::INIT_SPACE),
            ("config", 8 + Config::INIT_SPACE),
            ("draw", 8 + Draw::INIT_SPACE),
            ("holder entry", 8 + HolderEntry::INIT_SPACE),
        ] {
            assert!(size <= MAX_CPI_ALLOC, "{name} is {size} bytes");
        }
    }
}
