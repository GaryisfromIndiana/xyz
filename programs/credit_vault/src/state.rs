use anchor_lang::prelude::*;

pub const CONFIG_SEED: &[u8] = b"config";
pub const FEE_VAULT_SEED: &[u8] = b"fee_vault";
pub const RESERVE_VAULT_SEED: &[u8] = b"reserve_vault";
pub const DISCOUNT_VAULT_SEED: &[u8] = b"discount_vault";
pub const OPS_VAULT_SEED: &[u8] = b"ops_vault";
pub const PENDING_VAULT_SEED: &[u8] = b"pending_vault";
pub const BONUS_VAULT_SEED: &[u8] = b"bonus_vault";
pub const USER_SEED: &[u8] = b"user";
pub const REDEMPTION_SEED: &[u8] = b"redemption";

pub const BPS: u64 = 10_000;
pub const USDC_DECIMALS: u8 = 6;
pub const DAY_SECS: i64 = 24 * 60 * 60;
pub const MONTH_SECS: i64 = 30 * DAY_SECS;
pub const MAX_PROOF_LEN: usize = 32;

/// Every role and parameter change waits this long after it is proposed (audit: single-key drain).
pub const CONFIG_DELAY_SECS: i64 = 2 * DAY_SECS;
/// An emergency pause expires on its own after this long (audit: unbounded pause).
pub const MAX_PAUSE_SECS: i64 = 7 * DAY_SECS;
/// A request the operator marked as processing can be cancelled by its user after this long.
pub const MAX_PROCESSING_SECS: i64 = 7 * DAY_SECS;
pub const MAX_ROOT_DELAY_SECS: i64 = DAY_SECS;

/// Bounds the admin can never exceed.
pub const MIN_RESERVE_BPS: u16 = 7_000;
pub const MAX_OPS_BPS: u16 = 1_500;
pub const MAX_BURN_BPS: u16 = 2_000;
pub const MAX_MEMBER_DISCOUNT_BPS: u16 = 2_000;
pub const MIN_CANCEL_AFTER_SECS: i64 = 3_600;
pub const MAX_CANCEL_AFTER_SECS: i64 = 7 * DAY_SECS;
pub const MIN_REDEMPTION_FLOOR: u64 = 1_000_000; // $1
pub const MIN_AUCTION_SECS: i64 = 900;
pub const MAX_AUCTION_SECS: i64 = 7 * DAY_SECS;
pub const MIN_AUCTION_FLOOR_BPS: u16 = 5_000;
pub const MAX_AUCTION_FLOOR_BPS: u16 = 9_900;
pub const MIN_RESTART_MARKUP_BPS: u16 = 100;
pub const MAX_RESTART_MARKUP_BPS: u16 = 5_000;
/// The transfer fee can only be lowered, and never set above this.
pub const MAX_TRANSFER_FEE_BPS: u16 = 1_000;

/// Where a redemption's money came from; decides how a cancel refunds it.
pub mod source {
    /// Paid from the wallet's vested credit (reserve backing).
    pub const CREDIT: u8 = 0;
    /// Bought at face with USDC, with an optional member rebate from the discount pool.
    pub const PURCHASE: u8 = 1;
}

pub mod status {
    pub const PENDING: u8 = 0;
    pub const FULFILLED: u8 = 1;
    pub const CANCELLED: u8 = 2;
    /// The operator has started buying the code; the user can no longer cancel until
    /// MAX_PROCESSING_SECS have passed.
    pub const PROCESSING: u8 = 3;
}

/// How the transfer fee is divided, as ratios of whatever the fee rate is, so the split keeps
/// working when the fee steps down. `burn_bps` is the share of harvested fee tokens burned on
/// collection; `reserve_bps + discount_bps + ops_bps == 10_000` divide the USDC proceeds.
/// At a 3% fee: burn 667, reserve 7500, discount 1071, ops 1429 (2.1 / 0.3 / 0.4 / 0.2% of volume).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct Splits {
    pub burn_bps: u16,
    pub reserve_bps: u16,
    pub discount_bps: u16,
    pub ops_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct MemberParams {
    /// Rebate on purchases at face, e.g. 1000 = 10% back.
    pub discount_bps: u16,
    /// Minimum token balance (base units) in the wallet's associated token account.
    pub min_balance: u64,
    /// Maximum face value (micro-USDC) per wallet per 30 days that earns the rebate.
    pub monthly_cap: u64,
}

/// Auction shape. Changes take effect only when a new auction starts (audit: live repricing).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct Tuning {
    pub duration_secs: i64,
    pub floor_bps: u16,
    pub restart_markup_bps: u16,
}

/// Dutch auction for harvested fee tokens. Prices are micro-USDC per whole token x PRICE_SCALE.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct Auction {
    pub start_price: u64,
    pub started_at: i64,
    /// Shape of the running auction, frozen when it started.
    pub tuning: Tuning,
    /// Price paid on the most recent sale; seeds the next lot's start price.
    pub last_clear_price: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct Params {
    pub splits: Splits,
    pub member: MemberParams,
    pub min_redemption: u64,
    pub cancel_after_secs: i64,
    /// Delay between posting a root and it becoming usable, so a bad root can be vetoed.
    pub root_delay_secs: i64,
    /// Most USDC the operator can settle per day (limits a compromised operator key).
    pub max_fulfill_per_day: u64,
    /// Applied to the next auction that starts.
    pub auction: Tuning,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct Roles {
    pub indexer: Pubkey,
    pub operator: Pubkey,
    pub operator_treasury: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug)]
pub struct PendingChange {
    pub active: bool,
    pub eta: i64,
    pub roles: Roles,
    pub params: Params,
    /// If non-zero, restarts the auction at this price when applied (recovery path).
    pub reanchor_price: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq, Debug, Default)]
pub struct RootSlot {
    pub root: [u8; 32],
    pub epoch: u64,
    pub total_vested: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    pub pending_admin: Pubkey,
    pub roles: Roles,
    pub token_mint: Pubkey,
    pub usdc_mint: Pubkey,
    pub token_decimals: u8,
    pub params: Params,
    pub auction: Auction,
    /// Root that redemptions verify against.
    pub active_root: RootSlot,
    /// Posted but not yet usable; activates at `pending_activates_at` unless vetoed.
    pub pending_root: RootSlot,
    pub has_pending_root: bool,
    pub pending_activates_at: i64,
    pub root_posted_at: i64,
    /// Cumulative USDC that has entered backing. Vested credit can never exceed it.
    pub total_reserve_in: u64,
    /// Credit spent on redemptions, net of cancels. Can never exceed active vested credit.
    pub total_credit_consumed: u64,
    pub total_bonus_promoted: u64,
    pub total_fees_collected: u64,
    pub total_fees_burned: u64,
    pub fulfill_day: i64,
    pub fulfilled_today: u64,
    /// Paused while `now < paused_until`.
    pub paused_until: i64,
    pub pending_change: PendingChange,
    pub bump: u8,
    pub reserved: [u8; 64],
}

#[account]
#[derive(InitSpace)]
pub struct UserState {
    pub user: Pubkey,
    /// Credit this wallet has spent, net of cancels.
    pub consumed: u64,
    pub member_month: i64,
    pub member_spent: u64,
    pub request_count: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Redemption {
    pub user: Pubkey,
    pub id: u64,
    /// Face value of game credit owed to the user (micro-USDC).
    pub amount: u64,
    /// For purchases: USDC the user paid, and the member rebate sent back from the discount pool.
    pub user_paid: u64,
    pub rebate: u64,
    /// Face value counted against the member monthly cap, restored exactly on cancel.
    pub covered: u64,
    pub source: u8,
    /// Catalogue item code (game, Ultimate upgrade, GTA 6 currency pack...).
    pub item: u16,
    /// 0 = PlayStation, 1 = Xbox.
    pub platform: u8,
    /// ISO 3166-1 alpha-2 country of the user's console account.
    pub region: [u8; 2],
    pub status: u8,
    pub created_at: i64,
    /// Fixed when the request is made, so later parameter changes cannot lock it.
    pub cancel_at: i64,
    pub processing_at: i64,
    pub settled_at: i64,
    /// Hash of the code purchase receipt, posted by the operator on fulfilment.
    pub receipt_hash: [u8; 32],
    pub bump: u8,
}
