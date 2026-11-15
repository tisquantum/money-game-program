use anchor_lang::prelude::*;

#[error_code]
pub enum MoneyError {
    #[msg("Only the configured crank can do this")]
    NotCrank,
    #[msg("Only the admin can do this")]
    NotAdmin,
    #[msg("Basis points must be <= 10000")]
    BadBps,
    #[msg("Token account is not for the configured mint")]
    WrongMint,
    #[msg("Token account is not owned by the signer")]
    WrongTokenOwner,
    #[msg("Wallet does not hold enough of the token")]
    NotEnoughTokens,
    #[msg("Holder registry is full")]
    RegistryFull,
    #[msg("Holder entry does not match the registry")]
    RegistryMismatch,
    #[msg("Holder still meets the minimum; cannot prune")]
    StillEligible,
    #[msg("Not the holder's associated token account")]
    NotAssociatedTokenAccount,
    #[msg("A table is already running")]
    TableAlreadyRunning,
    #[msg("Game is paused")]
    Paused,
    #[msg("Treasury cannot fund the minimum pool yet")]
    PoolTooSmall,
    #[msg("The first table waits for 50 registered holders")]
    NotEnoughPlayers,
    #[msg("Wrong validator for this account")]
    WrongValidator,
    #[msg("Game is not live")]
    NotLive,
    #[msg("Game is waiting on randomness")]
    AwaitingRandomness,
    #[msg("This turn's deadline has passed")]
    TurnOver,
    #[msg("Turn cannot be resolved yet")]
    TurnNotOver,
    #[msg("Signer does not hold this seat")]
    NotSeatHolder,
    #[msg("Seat index out of range")]
    BadSeat,
    #[msg("Already acted this turn")]
    AlreadyActed,
    #[msg("No move is sealed")]
    NotSealed,
    #[msg("A sealed move can't be taken back once the decision opens")]
    DecisionOpen,
    #[msg("Wait 3 seconds before changing your sealed move")]
    TooSoon,
    #[msg("This move is not valid for the current turn")]
    WrongTurn,
    #[msg("Move value out of range")]
    BadValue,
    #[msg("Only the dealer can do this")]
    NotDealer,
    #[msg("Offer exceeds the lump sum")]
    OfferTooLarge,
    #[msg("Insufficient balance")]
    InsufficientBalance,
    #[msg("No fresh randomness to consume")]
    DrawNotReady,
    #[msg("Nothing is waiting on randomness")]
    NothingPending,
    #[msg("Game is not finished")]
    NotFinished,
    #[msg("Game is not settled")]
    NotSettled,
    #[msg("Already claimed")]
    AlreadyClaimed,
    #[msg("Claim window still open")]
    ClaimWindowOpen,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("That table isn't the one running")]
    NotActiveGame,
    #[msg("Game log entries must be relayed in order")]
    RelayOutOfOrder,
    #[msg("The ledger hasn't caught up with the game")]
    LedgerBehind,
}
