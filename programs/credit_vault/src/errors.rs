use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("Splits must sum to 10000 bps and stay inside the hard bounds")]
    InvalidSplits,
    #[msg("A parameter is outside its allowed range")]
    InvalidParams,
    #[msg("Signer is not allowed to do this")]
    Unauthorized,
    #[msg("The vault is paused")]
    Paused,
    #[msg("The token mint has no transfer-fee extension")]
    NotTransferFeeMint,
    #[msg("The mint's withheld-fee authority must be the config PDA, and its fee-config authority the PDA or none")]
    MintFeeAuthorityMismatch,
    #[msg("USDC mint must have 6 decimals")]
    InvalidUsdcMint,
    #[msg("Epoch must increase")]
    StaleEpoch,
    #[msg("Total vested credit can never decrease")]
    VestedDecreased,
    #[msg("Total vested credit would exceed the USDC that has entered the reserve")]
    Insolvent,
    #[msg("Merkle proof is invalid")]
    InvalidProof,
    #[msg("Merkle proof is too long")]
    ProofTooLong,
    #[msg("Not enough available credit")]
    InsufficientCredit,
    #[msg("Amount is below the minimum redemption")]
    BelowMinimum,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Not enough harvested fee tokens in the vault")]
    AuctionEmpty,
    #[msg("Price moved past the buyer's limit")]
    SlippageExceeded,
    #[msg("The auction has not reached its floor yet")]
    AuctionNotExpired,
    #[msg("Token balance is below the member minimum")]
    NotMember,
    #[msg("Monthly member-price cap exceeded")]
    MonthlyCapExceeded,
    #[msg("Request id does not match the user's next request id")]
    BadRequestId,
    #[msg("Redemption is not pending")]
    NotPending,
    #[msg("The user can cancel only after the cancel window")]
    CancelTooEarly,
    #[msg("A refund account required for this redemption's source is missing")]
    MissingRefundAccount,
    #[msg("The reserve would hold less than the credit outstanding")]
    BackingShortfall,
    #[msg("The transfer fee can only be lowered")]
    FeeCanOnlyDecrease,
    #[msg("The token mint has an extension that could break the vault's guarantees")]
    MintExtensionNotAllowed,
    #[msg("The token mint must have no mint authority (fixed supply)")]
    MintAuthorityPresent,
    #[msg("The token mint must have no freeze authority")]
    FreezeAuthorityPresent,
    #[msg("The proposed change is still inside its timelock")]
    TimelockActive,
    #[msg("There is no pending change")]
    NoPendingChange,
    #[msg("Signer is not the pending admin")]
    NotPendingAdmin,
    #[msg("A root is already pending; activate or veto it first")]
    RootPending,
    #[msg("The pending root is not due yet")]
    RootNotDue,
    #[msg("There is no pending root")]
    NoPendingRoot,
    #[msg("The pending root is already due and can no longer be vetoed")]
    RootAlreadyDue,
    #[msg("Redemptions would exceed the vested credit in the active root")]
    Overissued,
    #[msg("This would exceed the operator's daily settlement cap")]
    FulfillCapExceeded,
    #[msg("Only settled redemptions can be closed")]
    NotSettled,
    #[msg("The operator treasury must be a USDC account outside the program's vaults")]
    TreasuryInvalid,
    #[msg("No harvested fee tokens to auction")]
    FeeVaultEmpty,
}
