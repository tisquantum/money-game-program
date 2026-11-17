//! Seeds, economics and the fixed turn schedule.

pub const CONFIG_SEED: &[u8] = b"config";
pub const TREASURY_SEED: &[u8] = b"treasury";
pub const HOLDER_SEED: &[u8] = b"holder";
pub const GAME_SEED: &[u8] = b"game";
pub const HAND_SEED: &[u8] = b"hand";
pub const VAULT_SEED: &[u8] = b"vault";
pub const DRAW_SEED: &[u8] = b"draw";
pub const RECORD_SEED: &[u8] = b"record";
pub const LEDGER_SEED: &[u8] = b"ledger";

/// Every table seats exactly five holders.
pub const SEATS: usize = 5;
/// The first table waits until this many wallets have registered. Later tables
/// open from whoever is still in the draw.
pub const FIRST_TABLE_REGISTRANTS: u32 = 50;
/// Opt-in holder registry size (one zero-copy account, ~64 KiB).
pub const REGISTRY_CAPACITY: usize = 2048;
/// Wallets voided in the current game are never redrawn into it.
pub const MAX_EXCLUDED: usize = 24;
/// Public event log ring on the game account.
pub const LOG_LEN: usize = 24;
/// Minimum gap between sealing and unsealing a move.
pub const SEAL_COOLDOWN_SECS: i64 = 3;

/// Points are an accounting unit: at settlement each seat is paid
/// `escrow * points / POOL_POINTS` of the pool token (USDC). Points nobody
/// wins roll back into the treasury and fund the next table.
pub const POOL_POINTS: u64 = 10_000;
/// Half the pool is split across the four mini-games...
pub const MINIGAME_POINTS: u32 = 1_250;
/// ...and the other half is the lump sum on the table during The Deal.
pub const DEAL_LUMP: u32 = 5_000;
/// The dealer can walk away with 15% of the lump instead of offering.
pub const DEAL_GUARANTEED: u32 = 750;
/// Accepts needed for an offer to pass (all but one of the four voters).
pub const DEAL_ACCEPTS_REQUIRED: u8 = 3;
pub const DEAL_ATTEMPTS: u8 = 3;

pub const RAFFLE_MAX_TICKETS: u32 = 20;
/// Share of each round's slice: 20% / 30% / 50%.
pub const ROUND_SHARE_PCT: [u32; 3] = [20, 30, 50];
/// Money Makers multipliers ×10 (1.2, 1.5, 2.0).
pub const MM_MULTIPLIER_X10: [u32; 3] = [12, 15, 20];
pub const MM_WINNER_MEDALS: u8 = 2;

pub const MIN_SEATED_TO_GO_LIVE: usize = 2;
pub const CLAIM_WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Turn schedule. A game is 22 turns that add up to 44 minutes 8 seconds.
// Deadlines are absolute (measured from `live_ts`), so if a turn resolves
// early because everyone has moved, the next turn gets the spare time and
// the table always ends at that mark (or sooner if a deal lands).
//
// Every round is a minute and a half of open discussion followed by thirty
// seconds to decide. The first turn of each mini-game is longer: it starts with a
// short introduction to the rules, and the table's very first turn also
// starts with an overview of the whole game. Every turn that follows a round
// starts with that round's result being played out. The program only sees turn
// lengths; the relay and the UI keep moves shut until the decision opens.
// ---------------------------------------------------------------------------

pub const K_RAFFLE: u8 = 0;
pub const K_TRANSFER: u8 = 1;
pub const K_SIGNAL: u8 = 2;
pub const K_MONEY_MAKERS: u8 = 3;
pub const K_PRIZE_CASE: u8 = 4;
pub const K_DEAL_PROPOSE: u8 = 5;
pub const K_DEAL_VOTE: u8 = 6;

pub const TURN_COUNT: usize = 22;

pub const TURN_KINDS: [u8; TURN_COUNT] = [
    K_RAFFLE, K_RAFFLE, K_RAFFLE, K_TRANSFER,
    K_SIGNAL, K_SIGNAL, K_SIGNAL, K_TRANSFER,
    K_MONEY_MAKERS, K_MONEY_MAKERS, K_MONEY_MAKERS, K_TRANSFER,
    K_PRIZE_CASE, K_PRIZE_CASE, K_PRIZE_CASE, K_TRANSFER,
    K_DEAL_PROPOSE, K_DEAL_VOTE, K_DEAL_PROPOSE, K_DEAL_VOTE, K_DEAL_PROPOSE, K_DEAL_VOTE,
];

/// Round index (0-based) of each turn within its mini-game.
pub const TURN_ROUND: [u8; TURN_COUNT] = [
    0, 1, 2, 0, 0, 1, 2, 1, 0, 1, 2, 2, 0, 1, 2, 3, 0, 0, 1, 1, 2, 2,
];

/// Open discussion at the start of every round, before moves open.
pub const DISCUSSION_SECS: i64 = 90;
/// Time to move once the discussion ends.
pub const DECISION_SECS: i64 = 30;
/// A round of any mini-game, or an offer or a vote in the deal.
pub const ROUND_SECS: i64 = DISCUSSION_SECS + DECISION_SECS;
pub const TRANSFER_SECS: i64 = 60;
/// Introduction to the rules, ahead of each mini-game's first round.
pub const INTRO_SECS: i64 = 20;
/// Overview of the whole game, ahead of the first introduction.
pub const OVERVIEW_SECS: i64 = 12;
/// The previous round's result, played out at the start of the turn after it.
pub const RESULT_SECS: i64 = 8;

const OPENING_ROUND_SECS: i64 = INTRO_SECS + ROUND_SECS;
const NEXT_ROUND_SECS: i64 = RESULT_SECS + ROUND_SECS;
const NEXT_TRANSFER_SECS: i64 = RESULT_SECS + TRANSFER_SECS;

pub const TURN_SECS: [i64; TURN_COUNT] = [
    OVERVIEW_SECS + OPENING_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_TRANSFER_SECS,
    OPENING_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_TRANSFER_SECS,
    OPENING_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_TRANSFER_SECS,
    OPENING_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_TRANSFER_SECS,
    OPENING_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS, NEXT_ROUND_SECS,
];

pub const GAME_SECS: i64 = 44 * 60 + 8;

pub const fn turn_end_offset(turn: usize) -> i64 {
    let mut total = 0;
    let mut i = 0;
    while i <= turn && i < TURN_COUNT {
        total += TURN_SECS[i];
        i += 1;
    }
    total
}

const _: () = assert!(turn_end_offset(TURN_COUNT - 1) == GAME_SECS);

// Game status
pub const STATUS_SEATING: u8 = 0;
pub const STATUS_LIVE: u8 = 1;
pub const STATUS_FINISHED: u8 = 2;
pub const STATUS_SETTLED: u8 = 3;

// Pending work that needs VRF before the table can continue.
pub const PENDING_SEATS: u8 = 1 << 0;
pub const PENDING_RAFFLE: u8 = 1 << 1;

// Seat status
pub const SEAT_EMPTY: u8 = 0;
pub const SEAT_ACTIVE: u8 = 1;
/// Voided for missing a turn; earnings stay on the seat for the next wallet.
pub const SEAT_VACANT: u8 = 2;

pub const NO_SEAT: u8 = u8::MAX;

// Event log kinds. Round outcomes (wins, payouts, no-winner) are only kept in the
// private `Record` until the game is committed; the public log carries what
// everyone may know as it happens.
pub const EV_SEATED: u8 = 1;
pub const EV_VOIDED: u8 = 2;
pub const EV_RAFFLE_WIN: u8 = 3;
pub const EV_SIGNAL_WIN: u8 = 4;
pub const EV_MM_PAYOUT: u8 = 5;
pub const EV_PRIZE: u8 = 6;
pub const EV_MEDAL: u8 = 7;
pub const EV_TRANSFER: u8 = 8;
pub const EV_DEAL_OFFER: u8 = 9;
pub const EV_DEAL_PASSED: u8 = 10;
pub const EV_DEAL_FAILED: u8 = 11;
pub const EV_GUARANTEED: u8 = 12;
pub const EV_NO_WINNER: u8 = 13;
pub const EV_DEALER: u8 = 14;
pub const EV_LIVE: u8 = 15;
pub const EV_FINISHED: u8 = 16;
/// A seat's money for a mini-game, made public once its last round is over
/// (`other` is the mini-game's turn kind). The Deal's split is posted the same way.
pub const EV_TOTAL: u8 = 17;
/// That a round paid out, without saying to whom: `other` is the turn kind and
/// `amount` how many seats won (Raffle and Signal: 0 or 1; Prize Case: the
/// cases taken alone). Who won, and what, stays in the record and the winner's hand.
pub const EV_ROUND_WON: u8 = 18;
