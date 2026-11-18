use anchor_lang::prelude::*;

#[event]
pub struct FeesCollected {
    pub collected: u64,
    pub burned: u64,
}

#[event]
pub struct FeesSold {
    pub buyer: Pubkey,
    pub tokens: u64,
    pub usdc: u64,
    pub price: u64,
    pub to_reserve: u64,
    pub to_discount: u64,
    pub to_ops: u64,
}

#[event]
pub struct AuctionRestarted {
    pub start_price: u64,
}

#[event]
pub struct ReserveDonated {
    pub donor: Pubkey,
    pub amount: u64,
}

#[event]
pub struct RootPosted {
    pub epoch: u64,
    pub root: [u8; 32],
    pub total_vested: u64,
    pub total_reserve_in: u64,
    pub activates_at: i64,
}

#[event]
pub struct RootActivated {
    pub epoch: u64,
    pub total_vested: u64,
}

#[event]
pub struct RootVetoed {
    pub epoch: u64,
}

#[event]
pub struct AuctionStarted {
    pub start_price: u64,
}

#[event]
pub struct RedemptionProcessing {
    pub user: Pubkey,
    pub id: u64,
}

#[event]
pub struct ConfigProposed {
    pub eta: i64,
}

#[event]
pub struct ConfigApplied {
    pub indexer: Pubkey,
    pub operator: Pubkey,
    pub operator_treasury: Pubkey,
}

#[event]
pub struct ConfigCancelled {}

#[event]
pub struct AdminProposed {
    pub pending_admin: Pubkey,
}

#[event]
pub struct PauseSet {
    pub paused_until: i64,
}

#[event]
pub struct RedemptionRequested {
    pub user: Pubkey,
    pub id: u64,
    pub amount: u64,
    pub source: u8,
    pub item: u16,
    pub platform: u8,
    pub region: [u8; 2],
}

#[event]
pub struct RedemptionFulfilled {
    pub user: Pubkey,
    pub id: u64,
    pub amount: u64,
    pub receipt_hash: [u8; 32],
}

#[event]
pub struct RedemptionCancelled {
    pub user: Pubkey,
    pub id: u64,
    pub amount: u64,
    pub source: u8,
    pub by_operator: bool,
}

#[event]
pub struct CreditPurchased {
    pub user: Pubkey,
    pub id: u64,
    pub paid: u64,
    pub rebate: u64,
}

#[event]
pub struct BonusDeposited {
    pub from: Pubkey,
    pub amount: u64,
}

#[event]
pub struct BonusPromoted {
    pub amount: u64,
    pub total_reserve_in: u64,
}

#[event]
pub struct TransferFeeLowered {
    pub basis_points: u16,
    pub maximum_fee: u64,
}

#[event]
pub struct ConfigUpdated {
    pub admin: Pubkey,
}

#[event]
pub struct OpsWithdrawn {
    pub amount: u64,
    pub destination: Pubkey,
}
