//! GTA 6 credit vault.
//!
//! A Token-2022 transfer fee is the only income. The mint's withheld-fee authority is this
//! program's config PDA, so only the program can collect it. Collected fees are partly burned
//! and the rest sold for USDC through a permissionless Dutch auction. USDC is split into the
//! backing reserve, the member discount pool and operations.
//!
//! Holders' credit is computed off-chain (hourly time-weighted balances, 30-day vesting,
//! pro-rata forfeits) and posted as a Merkle root of (wallet, cumulative vested credit).
//! The program enforces the one invariant that matters: vested credit can never exceed the
//! USDC that has entered backing, and redemptions only pay out of backing.

use anchor_lang::prelude::*;

pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod state;
mod util;

pub use instructions::*;
pub use state::*;

declare_id!("FxpPCZhoY2ecs9g9w5M9VK7m5fMW8mWF28hjhrE8QGtY");

#[program]
pub mod credit_vault {
    use super::*;

    // ---- setup and admin ----

    pub fn initialize(ctx: Context<Initialize>, params: InitializeParams) -> Result<()> {
        instructions::admin::initialize_handler(ctx, params)
    }

    pub fn init_vaults_a(ctx: Context<InitVaultsA>) -> Result<()> {
        instructions::admin::init_vaults_a_handler(ctx)
    }

    pub fn init_vaults_b(ctx: Context<InitVaultsB>) -> Result<()> {
        instructions::admin::init_vaults_b_handler(ctx)
    }

    pub fn propose_config(ctx: Context<AdminOnly>, change: ConfigChangeArgs) -> Result<()> {
        instructions::admin::propose_config_handler(ctx, change)
    }

    pub fn apply_config(ctx: Context<ApplyConfig>) -> Result<()> {
        instructions::admin::apply_config_handler(ctx)
    }

    pub fn cancel_config(ctx: Context<AdminOnly>) -> Result<()> {
        instructions::admin::cancel_config_handler(ctx)
    }

    pub fn set_paused(ctx: Context<AdminOnly>, paused: bool) -> Result<()> {
        instructions::admin::set_paused_handler(ctx, paused)
    }

    pub fn transfer_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
        instructions::admin::transfer_admin_handler(ctx, new_admin)
    }

    pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
        instructions::admin::accept_admin_handler(ctx)
    }

    pub fn set_transfer_fee(ctx: Context<SetTransferFee>, basis_points: u16, maximum_fee: u64) -> Result<()> {
        instructions::admin::set_transfer_fee_handler(ctx, basis_points, maximum_fee)
    }

    pub fn withdraw_ops(ctx: Context<WithdrawOps>, amount: u64) -> Result<()> {
        instructions::admin::withdraw_ops_handler(ctx, amount)
    }

    // ---- fees in, USDC out ----

    pub fn collect_fees(ctx: Context<CollectFees>) -> Result<()> {
        instructions::fees::collect_fees_handler(ctx)
    }

    pub fn buy_fees(ctx: Context<BuyFees>, token_amount: u64, max_usdc: u64) -> Result<()> {
        instructions::fees::buy_fees_handler(ctx, token_amount, max_usdc)
    }

    pub fn restart_auction(ctx: Context<RestartAuction>) -> Result<()> {
        instructions::fees::restart_auction_handler(ctx)
    }

    pub fn donate_to_reserve(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        instructions::fees::donate_to_reserve_handler(ctx, amount)
    }

    pub fn deposit_bonus(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        instructions::fees::deposit_bonus_handler(ctx, amount)
    }

    pub fn promote_bonus(ctx: Context<PromoteBonus>, amount: u64) -> Result<()> {
        instructions::fees::promote_bonus_handler(ctx, amount)
    }

    // ---- credit ----

    pub fn post_root(ctx: Context<PostRoot>, root: [u8; 32], epoch: u64, total_vested: u64) -> Result<()> {
        instructions::credit::post_root_handler(ctx, root, epoch, total_vested)
    }

    pub fn activate_root(ctx: Context<ActivateRoot>) -> Result<()> {
        instructions::credit::activate_root_handler(ctx)
    }

    pub fn veto_root(ctx: Context<VetoRoot>) -> Result<()> {
        instructions::credit::veto_root_handler(ctx)
    }

    pub fn request_redemption(ctx: Context<RequestRedemption>, args: RedeemArgs) -> Result<()> {
        instructions::credit::request_redemption_handler(ctx, args)
    }

    pub fn buy_credit(ctx: Context<BuyCredit>, args: PurchaseArgs) -> Result<()> {
        instructions::credit::buy_credit_handler(ctx, args)
    }

    // ---- settlement ----

    pub fn mark_processing(ctx: Context<OperatorRedemption>) -> Result<()> {
        instructions::redemption::mark_processing_handler(ctx)
    }

    pub fn fulfill_redemption(ctx: Context<FulfillRedemption>, receipt_hash: [u8; 32]) -> Result<()> {
        instructions::redemption::fulfill_redemption_handler(ctx, receipt_hash)
    }

    pub fn cancel_redemption(ctx: Context<CancelRedemption>) -> Result<()> {
        instructions::redemption::cancel_redemption_handler(ctx)
    }

    pub fn close_redemption(ctx: Context<CloseRedemption>) -> Result<()> {
        instructions::redemption::close_redemption_handler(ctx)
    }
}
