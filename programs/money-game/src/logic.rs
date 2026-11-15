//! Pure game rules. Instructions load accounts, collect the sealed moves from
//! the hands, and call into here; nothing in this file touches account infos.

use anchor_lang::prelude::*;

use crate::constants::*;
use crate::errors::MoneyError;
use crate::state::*;

/// Sealed moves for the current turn, one per seat (None = no move).
pub type Moves = [Option<u32>; SEATS];

/// A turn can never be shorter than this, even when an earlier turn ran
/// late (e.g. waiting on VRF). Prevents a laggy draw from instantly
/// expiring the next turn and voiding everyone.
pub const MIN_TURN_SECS: i64 = 45;

/// splitmix64 stream seeded from VRF output.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: &[u8; 32], salt: u64) -> Self {
        let mut rng = Rng(salt ^ 0x6A09_E667_F3BC_C908);
        for chunk in seed.chunks_exact(8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(chunk);
            rng.0 ^= u64::from_le_bytes(b);
            rng.next();
        }
        rng
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, n). Rejection sampling, so no modulo bias.
    pub fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0);
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next();
            if v < zone {
                return v % n;
            }
        }
    }
}

pub fn round_pool(round: usize) -> u32 {
    MINIGAME_POINTS * ROUND_SHARE_PCT[round] / 100
}

/// Prize Case boards as fractions of the mini-game slice (1,250 points).
/// `None` marks a medal case.
pub fn prize_board(round: usize, boosted: bool) -> ([i32; SEATS], u8) {
    let s = MINIGAME_POINTS as i32;
    let (mut board, medals): ([i32; SEATS], u8) = match round {
        0 => ([s / 10, s * 7 / 100, s * 3 / 100, 0, 1], 0b1_0000),
        1 => ([s / 10, s / 10, 0, -(s / 10), 1], 0b1_0000),
        _ => ([s / 5, s / 10, 0, 1, 1], 0b1_1000),
    };
    if boosted {
        board[0] *= 2;
    }
    (board, medals)
}

impl Game {
    pub fn kind(&self) -> u8 {
        TURN_KINDS[self.turn as usize]
    }

    pub fn round(&self) -> usize {
        TURN_ROUND[self.turn as usize] as usize
    }

    /// Stamp used in `Seat::acted` / `Hand::turn_stamp` for the current turn.
    pub fn stamp(&self) -> u8 {
        self.turn + 1
    }

    pub fn push_log(&mut self, kind: u8, seat: u8, other: u8, amount: i32) {
        self.push_log_at(self.turn, kind, seat, other, amount);
    }

    fn push_log_at(&mut self, turn: u8, kind: u8, seat: u8, other: u8, amount: i32) {
        let i = (self.log_seq as usize) % LOG_LEN;
        self.log[i] = LogEntry {
            turn,
            kind,
            seat,
            other,
            amount,
        };
        self.log_seq = self.log_seq.wrapping_add(1);
    }

    pub fn seat_index(&self, seat: u8) -> Result<usize> {
        require!((seat as usize) < SEATS, MoneyError::BadSeat);
        Ok(seat as usize)
    }

    /// Checks the signer currently holds an active seat.
    pub fn require_holder(&self, seat: usize, signer: &Pubkey) -> Result<()> {
        let s = &self.seats[seat];
        require!(
            s.status == SEAT_ACTIVE && s.wallet == *signer,
            MoneyError::NotSeatHolder
        );
        Ok(())
    }

    pub fn require_playable(&self, now: i64) -> Result<()> {
        require!(self.status == STATUS_LIVE, MoneyError::NotLive);
        require!(self.pending == 0, MoneyError::AwaitingRandomness);
        require!(now <= self.turn_deadline, MoneyError::TurnOver);
        Ok(())
    }

    pub fn has_acted(&self, i: usize) -> bool {
        self.seats[i].acted == self.stamp()
    }

    /// True until the decision, which is the last `DECISION_SECS` of the turn.
    /// A sealed move can be taken back only while this is true. Transfer windows
    /// have no discussion.
    pub fn discussion_open(&self, now: i64) -> bool {
        self.kind() != K_TRANSFER && now < self.turn_deadline.saturating_sub(DECISION_SECS)
    }

    /// Must this seat act this turn or lose it?
    pub fn is_required(&self, i: usize) -> bool {
        let s = &self.seats[i];
        if s.status != SEAT_ACTIVE || s.seated == self.stamp() {
            return false;
        }
        match self.kind() {
            K_TRANSFER => false,
            K_DEAL_PROPOSE => i == self.dealer as usize,
            K_DEAL_VOTE => i != self.dealer as usize,
            _ => true,
        }
    }

    pub fn active_count(&self) -> usize {
        self.seats.iter().filter(|s| s.status == SEAT_ACTIVE).count()
    }

    /// True once everyone who needs to move has moved.
    pub fn everyone_moved(&self) -> bool {
        match self.kind() {
            // Transfer windows close early when every seated player hits "ready".
            K_TRANSFER => {
                self.active_count() > 0
                    && (0..SEATS)
                        .all(|i| self.seats[i].status != SEAT_ACTIVE || self.has_acted(i))
            }
            // The dealer's proposal advances the turn by itself.
            K_DEAL_PROPOSE => false,
            _ => (0..SEATS).all(|i| !self.is_required(i) || self.has_acted(i)),
        }
    }

    /// Validates a sealed move for the current turn. `supply` is the seat's
    /// Money Makers supply, from its hand.
    pub fn validate_move(&self, seat: usize, value: u32, supply: u32) -> Result<()> {
        let ok = match self.kind() {
            K_RAFFLE => value <= RAFFLE_MAX_TICKETS,
            K_SIGNAL => (1..=5).contains(&value),
            K_MONEY_MAKERS => value <= supply,
            K_PRIZE_CASE => (value as usize) < SEATS,
            K_DEAL_VOTE => {
                require!(seat != self.dealer as usize, MoneyError::WrongTurn);
                value <= 1
            }
            K_TRANSFER => true,
            _ => return err!(MoneyError::WrongTurn),
        };
        require!(ok, MoneyError::BadValue);
        Ok(())
    }

    fn exclude(&mut self, wallet: Pubkey) {
        let len = self.excluded_len as usize;
        if len < MAX_EXCLUDED {
            self.excluded[len] = wallet;
            self.excluded_len += 1;
        } else {
            self.excluded.rotate_left(1);
            self.excluded[MAX_EXCLUDED - 1] = wallet;
        }
    }

    pub fn void_seat(&mut self, i: usize) {
        let wallet = self.seats[i].wallet;
        self.exclude(wallet);
        let s = &mut self.seats[i];
        s.status = SEAT_VACANT;
        s.acted = 0;
        self.pending |= PENDING_SEATS;
        self.push_log(EV_VOIDED, i as u8, NO_SEAT, 0);
    }

    /// Voids every seat that had to move this turn and didn't.
    pub fn void_absent(&mut self) -> u8 {
        let mut n = 0;
        for i in 0..SEATS {
            if self.is_required(i) && !self.has_acted(i) {
                self.void_seat(i);
                n += 1;
            }
        }
        n
    }

    /// Points won in a round: kept in the record, and only made public with the
    /// mini-game's total once its last round is over.
    fn earn(&self, rec: &mut Record, i: usize, amount: i32) {
        rec.earned[i] = rec.earned[i].saturating_add(amount);
        let r = &mut rec.rounds[self.turn as usize];
        r.delta[i] = r.delta[i].saturating_add(amount);
    }

    /// Points everyone may see straight away. `turn` is the turn they belong to.
    fn credit_public(&mut self, turn: u8, i: usize, amount: i32, kind: u8) {
        let s = &mut self.seats[i];
        s.balance = add_signed(s.balance, amount);
        self.push_log_at(turn, EV_TOTAL, i as u8, kind, amount);
    }

    /// Files a round's moves and outcome in the record. The public reveal only
    /// says which seats moved.
    fn seal(&mut self, rec: &mut Record, moves: &Moves, winner: u8, amount: i32) {
        let mut mask = 0u8;
        let mut values = [0u32; SEATS];
        for (i, m) in moves.iter().enumerate() {
            if let Some(v) = m {
                mask |= 1 << i;
                values[i] = *v;
            }
        }
        let r = &mut rec.rounds[self.turn as usize];
        r.stamp = self.stamp();
        r.mask = mask;
        r.values = values;
        r.winner = winner;
        r.amount = amount;
        self.reveal = Reveal {
            turn: self.stamp(),
            mask,
            values: [0; SEATS],
            winner: NO_SEAT,
            amount: 0,
        };
    }

    /// Posts what each seat won over a mini-game, once its last round is over.
    fn post_totals(&mut self, rec: &mut Record, turn: u8, kind: u8) {
        for i in 0..SEATS {
            let e = rec.earned[i];
            rec.earned[i] = 0;
            if e != 0 {
                self.credit_public(turn, i, e, kind);
            }
        }
    }

    /// Runs after a resolution that may have moved the table on: a mini-game
    /// whose last round just ended posts its totals, and Money Makers hands out
    /// supplies as it opens.
    fn entered(&mut self, rec: &mut Record, from: u8) {
        if self.turn == from && self.status != STATUS_FINISHED {
            return;
        }
        let was = TURN_KINDS[from as usize];
        let minigame = matches!(was, K_RAFFLE | K_SIGNAL | K_MONEY_MAKERS | K_PRIZE_CASE);
        if minigame && (self.status == STATUS_FINISHED || self.kind() != was) {
            self.post_totals(rec, from, was);
        }
        if self.status == STATUS_LIVE && self.kind() == K_MONEY_MAKERS && self.round() == 0 {
            self.init_money_makers(rec);
        }
    }

    /// Mirrors the public balances kept on the `Ledger`, where transfers happen.
    pub fn sync_balances(&mut self, balances: [u32; SEATS]) -> Result<()> {
        require!(self.kind() == K_TRANSFER, MoneyError::WrongTurn);
        for (s, b) in self.seats.iter_mut().zip(balances) {
            s.balance = b;
        }
        Ok(())
    }

    pub fn raffle_tickets(moves: &Moves) -> u64 {
        moves.iter().map(|m| m.unwrap_or(0) as u64).sum()
    }

    /// Weighted draw: each ticket is one entry. The more tickets the table
    /// buys in total, the smaller the prize (pool / total tickets).
    pub fn resolve_raffle(&mut self, rec: &mut Record, moves: &Moves, rng: &mut Rng) {
        let round = self.round();
        let total = Self::raffle_tickets(moves);
        if total == 0 {
            self.seal(rec, moves, NO_SEAT, 0);
            return;
        }
        let mut pick = rng.below(total);
        let mut winner = 0usize;
        for (i, m) in moves.iter().enumerate() {
            let t = m.unwrap_or(0) as u64;
            if pick < t {
                winner = i;
                break;
            }
            pick -= t;
        }
        let prize = (round_pool(round) as u64 / total) as i32;
        self.earn(rec, winner, prize);
        self.seal(rec, moves, winner as u8, prize);
        if round == 2 {
            self.seats[winner].medals += 1;
            self.push_log(EV_MEDAL, winner as u8, NO_SEAT, 1);
        }
    }

    /// Lowest unique number wins `number × (round slice / 5)`.
    pub fn resolve_signal(&mut self, rec: &mut Record, moves: &Moves) {
        let round = self.round();
        let mut counts = [0u8; 6];
        for v in moves.iter().flatten() {
            counts[*v as usize] += 1;
        }
        let winning = (1..=5usize).find(|n| counts[*n] == 1);
        match winning {
            Some(n) => {
                let winner = moves.iter().position(|m| *m == Some(n as u32)).unwrap();
                let payout = (n as u32 * (round_pool(round) / 5)) as i32;
                self.earn(rec, winner, payout);
                self.seal(rec, moves, winner as u8, payout);
                if round == 2 {
                    self.seats[winner].medals += 1;
                    self.push_log(EV_MEDAL, winner as u8, NO_SEAT, 1);
                }
            }
            None => self.seal(rec, moves, NO_SEAT, 0),
        }
    }

    pub fn init_money_makers(&self, rec: &mut Record) {
        // Sized so full cooperation over three rounds pays out ~the slice.
        let product_x1000: u32 = MM_MULTIPLIER_X10.iter().product();
        let initial = MINIGAME_POINTS * 1000 / product_x1000 / SEATS as u32;
        rec.mm_supply = [initial; SEATS];
    }

    /// Contributions are pooled, multiplied, and split across all five seats.
    pub fn resolve_money_makers(&mut self, rec: &mut Record, moves: &Moves) {
        let round = self.round();
        let mut collective: u64 = 0;
        let mut applied: Moves = [None; SEATS];
        for (i, m) in moves.iter().enumerate() {
            if let Some(v) = m {
                let c = (*v).min(rec.mm_supply[i]);
                rec.mm_supply[i] -= c;
                collective += c as u64;
                applied[i] = Some(c);
            }
        }
        let share =
            (collective * MM_MULTIPLIER_X10[round] as u64 / 10 / SEATS as u64) as u32;
        for s in rec.mm_supply.iter_mut() {
            *s = s.saturating_add(share);
        }
        self.seal(rec, &applied, NO_SEAT, share as i32);

        if round == 2 {
            let mut best = 0usize;
            for i in 0..SEATS {
                let supply = rec.mm_supply[i];
                if supply > 0 {
                    self.earn(rec, i, supply as i32);
                }
                if supply > rec.mm_supply[best] {
                    best = i;
                }
            }
            self.seats[best].medals += MM_WINNER_MEDALS;
            self.push_log(EV_MEDAL, best as u8, NO_SEAT, MM_WINNER_MEDALS as i32);
            rec.rounds[self.turn as usize].winner = best as u8;
        }
    }

    pub fn load_prize_board(&mut self) {
        let round = self.round();
        // Round one always opens boosted; later rounds only if the table
        // spread out perfectly the round before.
        let boosted = round == 0 || self.prize_boost_next;
        let (board, medals) = prize_board(round, boosted);
        self.prize_board = board;
        self.prize_medal_mask = medals;
        self.prize_boost_next = false;
    }

    /// Each case pays only if exactly one seat picked it.
    pub fn resolve_prize_case(&mut self, rec: &mut Record, moves: &Moves) {
        let round = self.round();
        let mut counts = [0u8; SEATS];
        for v in moves.iter().flatten() {
            counts[*v as usize] += 1;
        }
        for (i, m) in moves.iter().enumerate() {
            let Some(case) = m else { continue };
            let case = *case as usize;
            if counts[case] != 1 {
                continue;
            }
            if self.prize_medal_mask & (1 << case) != 0 {
                self.seats[i].medals += 1;
                self.push_log(EV_MEDAL, i as u8, case as u8, 1);
            } else {
                let value = self.prize_board[case];
                self.earn(rec, i, value);
            }
        }
        let picks = moves.iter().flatten().count();
        let all_unique = picks > 0 && counts.iter().all(|c| *c <= 1);
        if all_unique && round < 2 {
            self.prize_boost_next = true;
        }
        self.seal(rec, moves, NO_SEAT, all_unique as i32);
    }

    /// Most medals deals; ties go to the higher balance, then the lower seat.
    pub fn choose_dealer(&mut self) {
        let mut best = 0usize;
        for i in 1..SEATS {
            let (a, b) = (&self.seats[i], &self.seats[best]);
            if (a.medals, a.balance) > (b.medals, b.balance) {
                best = i;
            }
        }
        self.dealer = best as u8;
        self.push_log(EV_DEALER, best as u8, NO_SEAT, self.seats[best].medals as i32);
    }

    pub fn propose(&mut self, offer: [u32; SEATS], now: i64) -> Result<()> {
        let mut offer = offer;
        offer[self.dealer as usize] = 0;
        let total: u32 = offer
            .iter()
            .try_fold(0u32, |acc, v| acc.checked_add(*v))
            .ok_or(MoneyError::MathOverflow)?;
        require!(total <= DEAL_LUMP, MoneyError::OfferTooLarge);
        self.offer = offer;
        self.offer_live = true;
        self.push_log(
            EV_DEAL_OFFER,
            self.dealer,
            self.round() as u8,
            (DEAL_LUMP - total) as i32,
        );
        self.advance(now);
        Ok(())
    }

    pub fn take_guaranteed(&mut self, now: i64) {
        let d = self.dealer as usize;
        self.push_log(EV_GUARANTEED, d as u8, NO_SEAT, DEAL_GUARANTEED as i32);
        self.credit_public(self.turn, d, DEAL_GUARANTEED as i32, K_DEAL_PROPOSE);
        self.finish(now);
    }

    /// Returns true if the deal passed (the game is over either way after
    /// the third attempt). Whether it passed is public; the votes and how many
    /// accepted stay in the record.
    pub fn resolve_vote(&mut self, rec: &mut Record, moves: &Moves, now: i64) -> bool {
        let dealer = self.dealer as usize;
        let accepts = moves
            .iter()
            .enumerate()
            .filter(|(i, m)| *i != dealer && **m == Some(1))
            .count() as u8;
        self.seal(rec, moves, dealer as u8, accepts as i32);
        let attempt = self.round();
        self.offer_live = false;
        if accepts >= DEAL_ACCEPTS_REQUIRED {
            let offer = self.offer;
            let given: u32 = offer.iter().sum();
            let kept = (DEAL_LUMP - given) as i32;
            self.push_log(EV_DEAL_PASSED, dealer as u8, 0, kept);
            let turn = self.turn;
            for (i, amount) in offer.iter().enumerate() {
                if i != dealer && *amount > 0 {
                    rec.rounds[turn as usize].delta[i] = *amount as i32;
                    self.credit_public(turn, i, *amount as i32, K_DEAL_VOTE);
                }
            }
            rec.rounds[turn as usize].delta[dealer] = kept;
            self.credit_public(turn, dealer, kept, K_DEAL_VOTE);
            self.finish(now);
            true
        } else {
            self.push_log(EV_DEAL_FAILED, dealer as u8, 0, attempt as i32);
            if attempt + 1 >= DEAL_ATTEMPTS as usize {
                self.finish(now);
            } else {
                self.advance(now);
            }
            false
        }
    }

    /// Dealer let the propose window lapse: that attempt is burned.
    pub fn lapse_proposal(&mut self, now: i64) {
        let attempt = self.round();
        self.push_log(EV_DEAL_FAILED, self.dealer, 0, attempt as i32);
        if attempt + 1 >= DEAL_ATTEMPTS as usize {
            self.finish(now);
        } else {
            self.turn += 1; // skip the vote turn
            self.advance(now);
        }
    }

    /// Gives the current turn its full time again from `now`, pushing the rest
    /// of the schedule back by however long the table was paused.
    fn restart_turn(&mut self, now: i64) {
        let turn = self.turn as usize;
        let starts_at = turn_end_offset(turn) - TURN_SECS[turn];
        self.live_ts = now - starts_at;
        self.turn_deadline = self.deadline_for(turn, now);
    }

    pub fn deadline_for(&self, turn: usize, now: i64) -> i64 {
        (self.live_ts + turn_end_offset(turn)).max(now + MIN_TURN_SECS)
    }

    /// Moves to the next turn, running any phase setup.
    pub fn advance(&mut self, now: i64) {
        let next = self.turn as usize + 1;
        if next >= TURN_COUNT {
            self.finish(now);
            return;
        }
        self.turn = next as u8;
        self.turn_deadline = self.deadline_for(next, now);
        match (self.kind(), self.round()) {
            (K_PRIZE_CASE, _) => self.load_prize_board(),
            (K_DEAL_PROPOSE, 0) => self.choose_dealer(),
            _ => {}
        }
        if self.kind() == K_DEAL_PROPOSE {
            self.offer = [0; SEATS];
            self.offer_live = false;
        }
        if self.seats.iter().any(|s| s.status != SEAT_ACTIVE) {
            self.pending |= PENDING_SEATS;
        }
    }

    pub fn finish(&mut self, now: i64) {
        self.status = STATUS_FINISHED;
        self.finished_ts = now;
        self.pending = 0;
        self.offer_live = false;
        self.push_log(EV_FINISHED, NO_SEAT, NO_SEAT, 0);
    }

    /// Resolves the current turn after its deadline (or once everyone moved).
    /// Raffle turns with tickets on the table park on `PENDING_RAFFLE` and
    /// finish in `apply_randomness`.
    pub fn resolve_turn(&mut self, rec: &mut Record, moves: &Moves, now: i64) {
        let from = self.turn;
        self.void_absent();
        match self.kind() {
            K_RAFFLE => {
                if Self::raffle_tickets(moves) == 0 {
                    let mut unused = Rng(0);
                    self.resolve_raffle(rec, moves, &mut unused);
                    self.advance(now);
                } else {
                    self.pending |= PENDING_RAFFLE;
                }
            }
            K_SIGNAL => {
                self.resolve_signal(rec, moves);
                self.advance(now);
            }
            K_MONEY_MAKERS => {
                self.resolve_money_makers(rec, moves);
                self.advance(now);
            }
            K_PRIZE_CASE => {
                self.resolve_prize_case(rec, moves);
                self.advance(now);
            }
            K_TRANSFER => self.advance(now),
            K_DEAL_PROPOSE => self.lapse_proposal(now),
            K_DEAL_VOTE => {
                self.resolve_vote(rec, moves, now);
            }
            _ => {}
        }
        self.entered(rec, from);
    }

    /// Consumes one VRF output: finishes a parked raffle, then seats
    /// replacement wallets drawn from the registry.
    pub fn apply_randomness(
        &mut self,
        rec: &mut Record,
        randomness: &[u8; 32],
        moves: &Moves,
        holders: &[Pubkey],
        now: i64,
    ) {
        if self.pending & PENDING_RAFFLE != 0 {
            let from = self.turn;
            let mut rng = Rng::new(randomness, 1);
            self.pending &= !PENDING_RAFFLE;
            self.resolve_raffle(rec, moves, &mut rng);
            self.advance(now);
            self.entered(rec, from);
        }
        if self.pending & PENDING_SEATS != 0 && self.status != STATUS_FINISHED {
            let mut rng = Rng::new(randomness, 2);
            self.fill_seats(holders, &mut rng, now);
        }
    }

    fn is_seated(&self, wallet: &Pubkey) -> bool {
        self.seats
            .iter()
            .any(|s| s.status == SEAT_ACTIVE && s.wallet == *wallet)
    }

    fn is_excluded(&self, wallet: &Pubkey) -> bool {
        self.excluded[..self.excluded_len as usize].contains(wallet)
    }

    /// A random registry wallet that isn't seated. Wallets voided from this
    /// table are only eligible when `allow_voided` is set.
    fn pick_holder(&self, holders: &[Pubkey], rng: &mut Rng, allow_voided: bool) -> Option<Pubkey> {
        let eligible = |w: &&Pubkey| {
            **w != Pubkey::default() && !self.is_seated(w) && (allow_voided || !self.is_excluded(w))
        };
        let n = holders.iter().filter(eligible).count();
        if n == 0 {
            return None;
        }
        holders.iter().filter(eligible).nth(rng.below(n as u64) as usize).copied()
    }

    /// Seats wallets that haven't played this table first, and falls back to
    /// ones voided from it when the registry runs short. A live table is
    /// paused while seats are empty; its current turn restarts once seated.
    pub fn fill_seats(&mut self, holders: &[Pubkey], rng: &mut Rng, now: i64) {
        self.pending &= !PENDING_SEATS;
        for i in 0..SEATS {
            if self.seats[i].status == SEAT_ACTIVE {
                continue;
            }
            let Some(w) = self
                .pick_holder(holders, rng, false)
                .or_else(|| self.pick_holder(holders, rng, true))
            else {
                break;
            };
            let stamp = if self.status == STATUS_LIVE { self.stamp() } else { 0 };
            let s = &mut self.seats[i];
            s.wallet = w;
            s.status = SEAT_ACTIVE;
            s.acted = 0;
            s.seated = stamp;
            let inherited = s.balance as i32;
            self.push_log(EV_SEATED, i as u8, NO_SEAT, inherited);
        }
        if self.status == STATUS_LIVE {
            self.restart_turn(now);
        }
        if self.status == STATUS_SEATING {
            if self.active_count() >= MIN_SEATED_TO_GO_LIVE {
                self.status = STATUS_LIVE;
                self.live_ts = now;
                self.turn = 0;
                self.turn_deadline = self.deadline_for(0, now);
                self.push_log(EV_LIVE, NO_SEAT, NO_SEAT, self.active_count() as i32);
            } else {
                // Not enough holders yet; the keeper retries with backoff.
                self.pending |= PENDING_SEATS;
            }
        }
    }

}

fn add_signed(balance: u32, amount: i32) -> u32 {
    if amount >= 0 {
        balance.saturating_add(amount as u32)
    } else {
        balance.saturating_sub(amount.unsigned_abs())
    }
}

impl Ledger {
    /// Logs a public money or medal event. The seq goes in the transaction's
    /// logs so each entry can be matched to the transaction that made it.
    fn push(&mut self, turn: u8, kind: u8, seat: u8, other: u8, amount: i32) {
        let seq = self.log_seq;
        self.log[(seq as usize) % LOG_LEN] = LogEntry { turn, kind, seat, other, amount };
        self.log_seq = seq.wrapping_add(1);
        msg!("ledger seq {}", seq);
    }

    /// Applies the game's log entry `seq`. Entries arrive in order; one already
    /// applied is skipped, so a relay can safely send it again.
    pub fn relay(&mut self, seq: u32, e: &LogEntry, wallet: Pubkey) -> Result<bool> {
        if seq < self.relayed {
            return Ok(false);
        }
        require!(seq == self.relayed, MoneyError::RelayOutOfOrder);
        let i = e.seat as usize;
        let seat = || -> Result<usize> {
            require!(i < SEATS, MoneyError::BadSeat);
            Ok(i)
        };
        match e.kind {
            EV_SEATED => {
                let s = &mut self.seats[seat()?];
                s.wallet = wallet;
                s.active = true;
            }
            EV_VOIDED => self.seats[seat()?].active = false,
            EV_MEDAL => {
                let s = &mut self.seats[seat()?];
                s.medals = s.medals.saturating_add(e.amount.clamp(0, u8::MAX as i32) as u8);
                self.push(e.turn, EV_MEDAL, e.seat, e.other, e.amount);
            }
            EV_TOTAL => {
                let s = &mut self.seats[seat()?];
                s.balance = add_signed(s.balance, e.amount);
                self.push(e.turn, EV_TOTAL, e.seat, e.other, e.amount);
            }
            EV_FINISHED => {
                self.finished = true;
                self.window = 0;
            }
            _ => {}
        }
        self.relayed = seq + 1;
        Ok(true)
    }

    pub fn transfer(&mut self, from: usize, to: usize, holder: &Pubkey, amount: u32) -> Result<()> {
        require!(self.window != 0, MoneyError::WrongTurn);
        require!(from != to && from < SEATS && to < SEATS, MoneyError::BadSeat);
        require!(amount > 0, MoneyError::BadValue);
        let s = &self.seats[from];
        require!(s.active && s.wallet == *holder, MoneyError::NotSeatHolder);
        require!(self.seats[to].wallet != Pubkey::default(), MoneyError::BadSeat);
        require!(s.balance >= amount, MoneyError::InsufficientBalance);
        self.seats[from].balance -= amount;
        self.seats[to].balance += amount;
        self.push(self.window - 1, EV_TRANSFER, from as u8, to as u8, amount as i32);
        Ok(())
    }

    /// Pool-token units per seat at settlement. Points nobody earned stay
    /// behind and are returned to the treasury.
    pub fn compute_payouts(&self, distributable: u64, wallets: &[Seat; SEATS]) -> [u64; SEATS] {
        let total: u64 = self.seats.iter().map(|s| s.balance as u64).sum();
        let denom = total.max(POOL_POINTS) as u128;
        let mut out = [0u64; SEATS];
        for (i, s) in self.seats.iter().enumerate() {
            if wallets[i].wallet != Pubkey::default() {
                out[i] = (distributable as u128 * s.balance as u128 / denom) as u64;
            }
        }
        out
    }

    pub fn balances(&self) -> [u32; SEATS] {
        core::array::from_fn(|i| self.seats[i].balance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    fn new_game() -> Game {
        Game {
            game_id: 1,
            bump: 0,
            vault_bump: 0,
            status: STATUS_SEATING,
            turn: 0,
            pending: PENDING_SEATS,
            created_ts: 0,
            live_ts: 0,
            turn_deadline: 0,
            finished_ts: 0,
            escrow: 0,
            draw_consumed: 0,
            seats: [Seat::default(); SEATS],
            excluded: [Pubkey::default(); MAX_EXCLUDED],
            excluded_len: 0,
            prize_board: [0; SEATS],
            prize_medal_mask: 0,
            prize_boost_next: false,
            dealer: 0,
            offer: [0; SEATS],
            offer_live: false,
            reveal: Reveal::default(),
            log: [LogEntry::default(); LOG_LEN],
            log_seq: 0,
            payouts: [0; SEATS],
            claimed_mask: 0,
            claim_deadline: 0,
        }
    }

    fn new_record() -> Record {
        Record {
            game_id: 1,
            bump: 0,
            earned: [0; SEATS],
            mm_supply: [0; SEATS],
            rounds: [RoundRecord::default(); TURN_COUNT],
        }
    }

    fn new_ledger() -> Ledger {
        Ledger {
            game_id: 1,
            bump: 0,
            seats: [LedgerSeat::default(); SEATS],
            relayed: 0,
            window: 0,
            finished: false,
            log: [LogEntry::default(); LOG_LEN],
            log_seq: 0,
        }
    }

    fn seated_game() -> (Game, Record) {
        let mut g = new_game();
        let mut rec = new_record();
        let holders: Vec<Pubkey> = (1..=8).map(wallet).collect();
        g.apply_randomness(&mut rec, &[7; 32], &[None; SEATS], &holders, 1_000);
        assert_eq!(g.status, STATUS_LIVE);
        assert_eq!(g.active_count(), 5);
        (g, rec)
    }

    fn all(v: u32) -> Moves {
        [Some(v); SEATS]
    }

    fn act_all(g: &mut Game) {
        let stamp = g.stamp();
        for s in g.seats.iter_mut() {
            s.acted = stamp;
        }
    }

    fn balances(g: &Game) -> [u32; SEATS] {
        core::array::from_fn(|i| g.seats[i].balance)
    }

    /// Public log entries of one kind, oldest first.
    fn logged(g: &Game, kind: u8) -> Vec<LogEntry> {
        let n = (g.log_seq as usize).min(LOG_LEN);
        (0..n)
            .map(|k| g.log[(g.log_seq as usize - n + k) % LOG_LEN])
            .filter(|e| e.kind == kind)
            .collect()
    }

    #[test]
    fn schedule_is_44_minutes_8_seconds() {
        assert_eq!(turn_end_offset(TURN_COUNT - 1), 2648);
    }

    #[test]
    fn seats_are_unique_and_registry_respected() {
        let (g, _) = seated_game();
        let mut ws: Vec<_> = g.seats.iter().map(|s| s.wallet).collect();
        ws.sort();
        ws.dedup();
        assert_eq!(ws.len(), 5);
        assert!(g.seats.iter().all(|s| (1..=8).map(wallet).any(|w| w == s.wallet)));
        assert_eq!(g.turn_deadline, 1_000 + TURN_SECS[0]);
    }

    #[test]
    fn small_registry_waits_for_two() {
        let mut g = new_game();
        let mut rec = new_record();
        g.apply_randomness(&mut rec, &[1; 32], &[None; SEATS], &[wallet(1)], 0);
        assert_eq!(g.status, STATUS_SEATING);
        assert_ne!(g.pending & PENDING_SEATS, 0);
    }

    #[test]
    fn missing_a_turn_voids_and_replacement_inherits() {
        let (mut g, mut rec) = seated_game();
        g.seats[2].balance = 400;
        let gone = g.seats[2].wallet;
        let stamp = g.stamp();
        for i in [0, 1, 3, 4] {
            g.seats[i].acted = stamp;
        }
        let moves: Moves = [Some(0), Some(0), None, Some(0), Some(0)];
        g.resolve_turn(&mut rec, &moves, 1_000 + TURN_SECS[0] + 1);
        assert_eq!(g.seats[2].status, SEAT_VACANT);
        assert_eq!(g.excluded[0], gone);
        assert_ne!(g.pending & PENDING_SEATS, 0);

        let holders: Vec<Pubkey> = (1..=8).map(wallet).collect();
        g.apply_randomness(&mut rec, &[9; 32], &[None; SEATS], &holders, 1_200);
        assert_eq!(g.seats[2].status, SEAT_ACTIVE);
        assert_ne!(g.seats[2].wallet, gone);
        assert_eq!(g.seats[2].balance, 400, "earnings follow the seat");
        assert!(!g.is_required(2));
    }

    #[test]
    fn voided_wallets_return_only_when_registry_runs_short() {
        let mut g = new_game();
        let mut rec = new_record();
        let holders: Vec<Pubkey> = (1..=5).map(wallet).collect();
        g.apply_randomness(&mut rec, &[7; 32], &[None; SEATS], &holders, 1_000);
        let gone = g.seats[2].wallet;
        g.void_seat(2);
        assert!(g.require_playable(1_001).is_err(), "paused while a seat is empty");

        g.apply_randomness(&mut rec, &[9; 32], &[None; SEATS], &holders, 1_050);
        assert_eq!(g.seats[2].status, SEAT_ACTIVE);
        assert_eq!(g.seats[2].wallet, gone, "nobody else left to seat");
        assert!(g.require_playable(1_051).is_ok());

        g.void_seat(2);
        let mut more = holders.clone();
        more.push(wallet(6));
        g.apply_randomness(&mut rec, &[9; 32], &[None; SEATS], &more, 1_100);
        assert_eq!(g.seats[2].wallet, wallet(6), "a fresh wallet beats a voided one");
    }

    #[test]
    fn seating_restarts_the_turn_and_shifts_the_schedule() {
        let (mut g, mut rec) = seated_game();
        g.turn = 4;
        g.void_seat(1);
        let holders: Vec<Pubkey> = (1..=8).map(wallet).collect();
        let now = 1_000 + turn_end_offset(3) + 30;
        g.apply_randomness(&mut rec, &[5; 32], &[None; SEATS], &holders, now);
        assert_eq!(g.turn_deadline, now + TURN_SECS[4]);
        assert_eq!(g.live_ts + turn_end_offset(TURN_COUNT - 1), now - turn_end_offset(3) + GAME_SECS);
    }

    #[test]
    fn raffle_parks_for_vrf_then_pays_in_secret() {
        let (mut g, mut rec) = seated_game();
        act_all(&mut g);
        let moves: Moves = [Some(5), Some(0), Some(0), Some(0), Some(0)];
        g.resolve_turn(&mut rec, &moves, 1_100);
        assert_ne!(g.pending & PENDING_RAFFLE, 0);
        assert_eq!(g.turn, 0);
        g.apply_randomness(&mut rec, &[3; 32], &moves, &[], 1_101);
        assert_eq!(g.turn, 1);
        assert_eq!(rec.earned[0], (round_pool(0) / 5) as i32);
        assert_eq!(rec.rounds[0].winner, 0);
        assert_eq!(rec.rounds[0].values[0], 5);
        assert_eq!(balances(&g), [0; SEATS], "nothing is public mid-game");
        assert_eq!(g.reveal.values, [0; SEATS], "the public reveal only says who moved");
        assert_eq!(g.reveal.winner, NO_SEAT);
        assert_eq!(g.reveal.mask, 0b1_1111);
    }

    #[test]
    fn signal_lowest_unique_wins_in_secret() {
        let (mut g, mut rec) = seated_game();
        g.turn = 4;
        act_all(&mut g);
        let moves: Moves = [Some(1), Some(1), Some(3), Some(2), Some(2)];
        g.resolve_turn(&mut rec, &moves, 2_000);
        assert_eq!(rec.earned[2], (3 * round_pool(0) / 5) as i32);
        assert_eq!(rec.rounds[4].delta[2], (3 * round_pool(0) / 5) as i32);
        assert_eq!(rec.rounds[4].winner, 2);
        assert_eq!(g.seats[2].balance, 0);
        assert!(logged(&g, EV_TOTAL).is_empty());
    }

    #[test]
    fn a_minigame_posts_its_totals_after_its_last_round() {
        let (mut g, mut rec) = seated_game();
        g.turn = 4;
        for (round, moves) in [
            [Some(1), Some(1), Some(3), Some(2), Some(2)],
            [Some(4), Some(5), Some(5), Some(4), Some(1)],
            [Some(2), Some(2), Some(3), Some(3), Some(4)],
        ]
        .iter()
        .enumerate()
        {
            act_all(&mut g);
            g.resolve_turn(&mut rec, moves, 2_000 + round as i64);
        }
        assert_eq!(g.kind(), K_TRANSFER);
        let slice = |r: usize| round_pool(r) / 5;
        let expected = [0, 0, 3 * slice(0), 0, slice(1) + 4 * slice(2)];
        assert_eq!(balances(&g), expected);
        assert_eq!(rec.earned, [0; SEATS]);
        let totals = logged(&g, EV_TOTAL);
        assert_eq!(totals.len(), 2);
        assert!(totals.iter().all(|e| e.turn == 6 && e.other == K_SIGNAL));
        assert_eq!(g.seats[4].medals, 1, "the last round's medal is public");
        assert_eq!(logged(&g, EV_MEDAL).len(), 1);
    }

    #[test]
    fn money_makers_supplies_stay_in_the_record() {
        let (mut g, mut rec) = seated_game();
        g.turn = 7;
        act_all(&mut g);
        g.resolve_turn(&mut rec, &[None; SEATS], 3_000);
        assert_eq!(g.kind(), K_MONEY_MAKERS);
        assert_eq!(rec.mm_supply, [69; SEATS]);
        for r in 0..3 {
            act_all(&mut g);
            let moves: Moves = core::array::from_fn(|i| Some(rec.mm_supply[i]));
            g.resolve_turn(&mut rec, &moves, 3_000 + r);
        }
        let paid: u32 = g.seats.iter().map(|s| s.balance).sum();
        assert!(paid <= MINIGAME_POINTS, "paid {paid}");
        assert!(paid > MINIGAME_POINTS * 9 / 10);
        assert_eq!(g.seats.iter().map(|s| s.medals as u32).sum::<u32>(), 2);
    }

    #[test]
    fn prize_case_unique_picks_pay_and_boost() {
        let (mut g, mut rec) = seated_game();
        g.turn = 11;
        g.advance(4_000);
        assert_eq!(g.prize_board[0], 250, "round one opens boosted");
        act_all(&mut g);
        g.resolve_turn(&mut rec, &[Some(0), Some(1), Some(2), Some(3), Some(4)], 4_000);
        assert_eq!(rec.earned[0], 250);
        assert_eq!(g.seats[4].medals, 1, "medal cases are public at once");
        assert_eq!(g.prize_board[0], 250, "round two boosted after a perfect spread");
        act_all(&mut g);
        g.resolve_turn(&mut rec, &[Some(3), Some(3), Some(0), None, None], 4_100);
        assert_eq!(rec.earned[2], 37 + 250, "round-one 3% case plus boosted top case");
        assert_eq!(rec.earned[0], 250, "collided picks pay nothing");
    }

    #[test]
    fn deal_passes_with_three_accepts_and_pays_in_public() {
        let (mut g, mut rec) = seated_game();
        g.seats[1].medals = 3;
        g.turn = 15;
        g.advance(5_000);
        assert_eq!(g.dealer, 1);
        g.seats[1].acted = g.stamp();
        g.propose([1000, 0, 1000, 1000, 1000], 5_001).unwrap();
        assert_eq!(g.kind(), K_DEAL_VOTE);
        act_all(&mut g);
        g.resolve_turn(&mut rec, &[Some(1), None, Some(1), Some(1), Some(0)], 5_100);
        assert_eq!(g.status, STATUS_FINISHED);
        assert_eq!(g.seats[1].balance, 1000);
        assert_eq!(g.seats[0].balance, 1000);
        assert_eq!(rec.rounds[17].amount, 3, "how many accepted stays sealed");
        assert_eq!(logged(&g, EV_DEAL_PASSED)[0].other, 0);
        assert_eq!(logged(&g, EV_TOTAL).len(), 5);
    }

    #[test]
    fn deal_failures_exhaust_after_three() {
        let (mut g, mut rec) = seated_game();
        g.turn = 15;
        g.advance(5_000);
        for attempt in 0..3 {
            assert_eq!(g.kind(), K_DEAL_PROPOSE, "attempt {attempt}");
            g.propose([0, 2000, 2000, 1000, 0], 5_001).unwrap();
            act_all(&mut g);
            g.resolve_turn(&mut rec, &all(0), 5_100);
        }
        assert_eq!(g.status, STATUS_FINISHED);
        assert_eq!(g.seats.iter().map(|s| s.balance).sum::<u32>(), 0);
    }

    #[test]
    fn dealer_lapse_is_voided_and_burns_attempt() {
        let (mut g, mut rec) = seated_game();
        g.turn = 15;
        g.advance(5_000);
        let d = g.dealer as usize;
        g.resolve_turn(&mut rec, &[None; SEATS], 6_000);
        assert_eq!(g.seats[d].status, SEAT_VACANT);
        assert_eq!(g.kind(), K_DEAL_PROPOSE);
        assert_eq!(g.round(), 1);
    }

    #[test]
    fn late_turns_get_minimum_time() {
        let (mut g, _) = seated_game();
        g.advance(1_000 + 2_000);
        assert_eq!(g.turn_deadline, 1_000 + 2_000 + MIN_TURN_SECS);
    }

    #[test]
    fn balances_sync_only_in_a_transfer_window() {
        let (mut g, _) = seated_game();
        assert!(g.sync_balances([1; SEATS]).is_err());
        g.turn = 3;
        g.sync_balances([5, 4, 3, 2, 1]).unwrap();
        assert_eq!(balances(&g), [5, 4, 3, 2, 1]);
    }

    fn relayed_ledger(g: &Game) -> Ledger {
        let mut l = new_ledger();
        let n = g.log_seq as usize;
        for seq in 0..n {
            let e = g.log[seq % LOG_LEN];
            let wallet = if e.kind == EV_SEATED { g.seats[e.seat as usize].wallet } else { Pubkey::default() };
            l.relay(seq as u32, &e, wallet).unwrap();
        }
        l
    }

    #[test]
    fn ledger_follows_the_game_log_in_order() {
        let (g, _) = seated_game();
        let mut l = relayed_ledger(&g);
        assert_eq!(l.relayed, g.log_seq);
        assert!(l.seats.iter().zip(g.seats.iter()).all(|(a, b)| a.active && a.wallet == b.wallet));
        let e = LogEntry { turn: 6, kind: EV_TOTAL, seat: 1, other: K_SIGNAL, amount: 75 };
        assert!(l.relay(g.log_seq + 1, &e, Pubkey::default()).is_err(), "no gaps");
        assert!(!l.relay(0, &e, Pubkey::default()).unwrap(), "replays are skipped");
        l.relay(g.log_seq, &e, Pubkey::default()).unwrap();
        assert_eq!(l.seats[1].balance, 75);
        assert_eq!(l.log_seq, 1);
    }

    #[test]
    fn transfers_only_in_an_open_window() {
        let (g, _) = seated_game();
        let mut l = relayed_ledger(&g);
        l.seats[0].balance = 100;
        let w = l.seats[0].wallet;
        assert!(l.transfer(0, 1, &w, 50).is_err());
        l.window = 4;
        assert!(l.transfer(0, 1, &wallet(99), 50).is_err(), "only the seat's wallet");
        l.transfer(0, 1, &w, 50).unwrap();
        assert_eq!((l.seats[0].balance, l.seats[1].balance), (50, 50));
        assert!(l.transfer(0, 1, &w, 51).is_err());
        assert_eq!(l.log[0].kind, EV_TRANSFER);
        assert_eq!(l.log[0].turn, 3);
    }

    #[test]
    fn payouts_never_exceed_escrow() {
        let (g, _) = seated_game();
        let mut l = relayed_ledger(&g);
        l.seats[0].balance = 9_000;
        l.seats[1].balance = 3_000;
        let p = l.compute_payouts(1_000_000, &g.seats);
        assert!(p.iter().sum::<u64>() <= 1_000_000);
        l.seats[0].balance = 2_500;
        l.seats[1].balance = 0;
        let p = l.compute_payouts(1_000_000, &g.seats);
        assert_eq!(p[0], 250_000);
    }
}
