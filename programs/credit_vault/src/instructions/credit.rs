use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::errors::VaultError;
use crate::events::*;
use crate::math;
use crate::state::*;
use crate::util;

/// Promote the pending root once its delay has passed. Returns true if it changed anything.
pub(crate) fn activate_due_root(config: &mut Config, now: i64) -> bool {
    if config.has_pending_root && now >= config.pending_activates_at {
        config.active_root = config.pending_root;
        config.pending_root = RootSlot::default();
        config.has_pending_root = false;
        emit!(RootActivated { epoch: config.active_root.epoch, total_vested: config.active_root.total_vested });
        return true;
    }
    false
}

pub(crate) fn is_paused(config: &Config, now: i64) -> bool {
    now < config.paused_until
}

#[derive(Accounts)]
pub struct PostRoot<'info> {
    pub indexer: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.roles.indexer == indexer.key() @ VaultError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
}

/// The indexer posts a new Merkle root of (wallet, cumulative vested credit).
/// - total vested can never exceed the USDC that has entered backing, and never decrease;
/// - epochs go up by exactly one, so a bad epoch cannot lock out future roots (audit);
/// - the root becomes usable only after `root_delay_secs`, during which the admin can veto it.
pub fn post_root_handler(ctx: Context<PostRoot>, root: [u8; 32], epoch: u64, total_vested: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let config = &mut ctx.accounts.config;
    activate_due_root(config, now);
    require!(!config.has_pending_root, VaultError::RootPending);
    require!(epoch == config.active_root.epoch.checked_add(1).ok_or(VaultError::MathOverflow)?, VaultError::StaleEpoch);
    require!(total_vested >= config.active_root.total_vested, VaultError::VestedDecreased);
    require!(total_vested <= config.total_reserve_in, VaultError::Insolvent);
    let slot = RootSlot { root, epoch, total_vested };
    let activates_at = now.checked_add(config.params.root_delay_secs).ok_or(VaultError::MathOverflow)?;
    config.root_posted_at = now;
    if config.params.root_delay_secs == 0 {
        config.active_root = slot;
        emit!(RootActivated { epoch, total_vested });
    } else {
        config.pending_root = slot;
        config.has_pending_root = true;
        config.pending_activates_at = activates_at;
    }
    emit!(RootPosted { epoch, root, total_vested, total_reserve_in: config.total_reserve_in, activates_at });
    Ok(())
}

#[derive(Accounts)]
pub struct ActivateRoot<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
}

/// Permissionless: make a pending root active once its delay has passed.
pub fn activate_root_handler(ctx: Context<ActivateRoot>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.config.has_pending_root, VaultError::NoPendingRoot);
    require!(activate_due_root(&mut ctx.accounts.config, now), VaultError::RootNotDue);
    Ok(())
}

#[derive(Accounts)]
pub struct VetoRoot<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
}

/// Throw away a pending root before it activates (e.g. a buggy or compromised indexer).
pub fn veto_root_handler(ctx: Context<VetoRoot>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let config = &mut ctx.accounts.config;
    require!(config.has_pending_root, VaultError::NoPendingRoot);
    require!(now < config.pending_activates_at, VaultError::RootAlreadyDue);
    let epoch = config.pending_root.epoch;
    config.pending_root = RootSlot::default();
    config.has_pending_root = false;
    emit!(RootVetoed { epoch });
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RedeemArgs {
    pub request_id: u64,
    pub amount: u64,
    /// The wallet's cumulative vested credit in the active root.
    pub cumulative: u64,
    pub proof: Vec<[u8; 32]>,
    pub item: u16,
    pub platform: u8,
    pub region: [u8; 2],
}

#[derive(Accounts)]
#[instruction(args: RedeemArgs)]
pub struct RequestRedemption<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + UserState::INIT_SPACE,
        seeds = [USER_SEED, user.key().as_ref()],
        bump
    )]
    pub user_state: Box<Account<'info, UserState>>,
    #[account(
        init,
        payer = user,
        space = 8 + Redemption::INIT_SPACE,
        seeds = [REDEMPTION_SEED, user.key().as_ref(), &args.request_id.to_le_bytes()],
        bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
    #[account(mut, seeds = [RESERVE_VAULT_SEED], bump)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [PENDING_VAULT_SEED], bump)]
    pub pending_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

fn touch_user_state(user_state: &mut UserState, user: Pubkey, bump: u8, request_id: u64) -> Result<()> {
    if user_state.user == Pubkey::default() {
        user_state.user = user;
        user_state.bump = bump;
    }
    require!(request_id == user_state.request_count, VaultError::BadRequestId);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_redemption(
    r: &mut Redemption,
    user: Pubkey,
    id: u64,
    amount: u64,
    user_paid: u64,
    rebate: u64,
    covered: u64,
    src: u8,
    item: u16,
    platform: u8,
    region: [u8; 2],
    now: i64,
    cancel_after: i64,
    bump: u8,
) -> Result<()> {
    r.user = user;
    r.id = id;
    r.amount = amount;
    r.user_paid = user_paid;
    r.rebate = rebate;
    r.covered = covered;
    r.source = src;
    r.item = item;
    r.platform = platform;
    r.region = region;
    r.status = status::PENDING;
    r.created_at = now;
    r.cancel_at = now.checked_add(cancel_after).ok_or(VaultError::MathOverflow)?;
    r.processing_at = 0;
    r.settled_at = 0;
    r.receipt_hash = [0u8; 32];
    r.bump = bump;
    Ok(())
}

/// Spend vested credit on a GTA 6 item. USDC moves from backing to the pending vault until the
/// operator delivers the code (fulfil) or the request is cancelled (refund to backing).
pub fn request_redemption_handler(ctx: Context<RequestRedemption>, args: RedeemArgs) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    activate_due_root(&mut ctx.accounts.config, now);
    let (root, root_total, min_redemption, cancel_after, bump_cfg, paused) = {
        let c = &ctx.accounts.config;
        (c.active_root.root, c.active_root.total_vested, c.params.min_redemption, c.params.cancel_after_secs, c.bump, is_paused(c, now))
    };
    require!(!paused, VaultError::Paused);
    require!(args.proof.len() <= MAX_PROOF_LEN, VaultError::ProofTooLong);
    let user_key = ctx.accounts.user.key();
    touch_user_state(&mut ctx.accounts.user_state, user_key, ctx.bumps.user_state, args.request_id)?;

    let leaf = math::leaf_hash(&user_key, args.cumulative);
    require!(math::verify_proof(&args.proof, &root, leaf), VaultError::InvalidProof);
    let available = args
        .cumulative
        .checked_sub(ctx.accounts.user_state.consumed)
        .ok_or(VaultError::InsufficientCredit)?;
    require!(args.amount > 0, VaultError::ZeroAmount);
    require!(args.amount <= available, VaultError::InsufficientCredit);
    require!(args.amount >= min_redemption, VaultError::BelowMinimum);
    // On-chain cap regardless of what the tree's leaves add up to (audit): total spending can never
    // exceed the vested total the program accepted, which itself never exceeds backing inflow.
    let consumed_after = ctx
        .accounts
        .config
        .total_credit_consumed
        .checked_add(args.amount)
        .ok_or(VaultError::MathOverflow)?;
    require!(consumed_after <= root_total, VaultError::Overissued);

    let bump = [bump_cfg];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.reserve_vault,
        &ctx.accounts.pending_vault,
        &ctx.accounts.usdc_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        args.amount,
    )?;

    let user_state = &mut ctx.accounts.user_state;
    user_state.consumed = user_state.consumed.checked_add(args.amount).ok_or(VaultError::MathOverflow)?;
    user_state.request_count = user_state.request_count.checked_add(1).ok_or(VaultError::MathOverflow)?;
    ctx.accounts.config.total_credit_consumed = consumed_after;

    // Backing must still cover every unspent vested credit.
    ctx.accounts.reserve_vault.reload()?;
    let outstanding = root_total.checked_sub(consumed_after).ok_or(VaultError::MathOverflow)?;
    require!(ctx.accounts.reserve_vault.amount >= outstanding, VaultError::BackingShortfall);

    write_redemption(
        &mut ctx.accounts.redemption,
        user_key,
        args.request_id,
        args.amount,
        0,
        0,
        0,
        source::CREDIT,
        args.item,
        args.platform,
        args.region,
        now,
        cancel_after,
        ctx.bumps.redemption,
    )?;
    emit!(RedemptionRequested {
        user: user_key,
        id: args.request_id,
        amount: args.amount,
        source: source::CREDIT,
        item: args.item,
        platform: args.platform,
        region: args.region,
    });
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PurchaseArgs {
    pub request_id: u64,
    /// Face value of GTA 6 credit to buy, paid in full in USDC.
    pub price: u64,
    pub item: u16,
    pub platform: u8,
    pub region: [u8; 2],
}

#[derive(Accounts)]
#[instruction(args: PurchaseArgs)]
pub struct BuyCredit<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint, has_one = token_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = token_2022_program)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + UserState::INIT_SPACE,
        seeds = [USER_SEED, user.key().as_ref()],
        bump
    )]
    pub user_state: Box<Account<'info, UserState>>,
    #[account(
        init,
        payer = user,
        space = 8 + Redemption::INIT_SPACE,
        seeds = [REDEMPTION_SEED, user.key().as_ref(), &args.request_id.to_le_bytes()],
        bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = user,
        token::token_program = token_program
    )]
    pub user_usdc: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The wallet's own associated token account. ATAs carry ImmutableOwner, so one bag cannot be
    /// reassigned between wallets with SetAuthority to farm the rebate (audit).
    #[account(
        associated_token::mint = token_mint,
        associated_token::authority = user,
        associated_token::token_program = token_2022_program
    )]
    pub user_token: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [DISCOUNT_VAULT_SEED], bump)]
    pub discount_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [PENDING_VAULT_SEED], bump)]
    pub pending_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// Buy GTA 6 credit at face with USDC. Members (ATA balance >= min_balance) get
/// `member.discount_bps` back from the discount pool, up to the monthly cap and whatever the pool
/// holds. A drained pool means no rebate, never a failed purchase and never a touch on backing.
pub fn buy_credit_handler(ctx: Context<BuyCredit>, args: PurchaseArgs) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let (member, min_redemption, cancel_after, bump_cfg, paused) = {
        let c = &ctx.accounts.config;
        (c.params.member, c.params.min_redemption, c.params.cancel_after_secs, c.bump, is_paused(c, now))
    };
    require!(!paused, VaultError::Paused);
    require!(args.price > 0, VaultError::ZeroAmount);
    require!(args.price >= min_redemption, VaultError::BelowMinimum);
    let user_key = ctx.accounts.user.key();
    touch_user_state(&mut ctx.accounts.user_state, user_key, ctx.bumps.user_state, args.request_id)?;

    let month = math::month_index(now);
    if ctx.accounts.user_state.member_month != month {
        ctx.accounts.user_state.member_month = month;
        ctx.accounts.user_state.member_spent = 0;
    }
    let is_member = ctx.accounts.user_token.amount >= member.min_balance;
    let rebate = if is_member {
        let allowance_left = member.monthly_cap.saturating_sub(ctx.accounts.user_state.member_spent);
        math::member_rebate(args.price, member.discount_bps, ctx.accounts.discount_vault.amount, allowance_left)
    } else {
        0
    };
    // Face value the rebate covered, stored on the request so a cancel restores it exactly.
    let covered = if rebate > 0 {
        ((rebate as u128 * BPS as u128).div_ceil(member.discount_bps.max(1) as u128) as u64).min(args.price)
    } else {
        0
    };

    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.user_usdc,
        &ctx.accounts.pending_vault,
        &ctx.accounts.usdc_mint,
        ctx.accounts.user.to_account_info(),
        &[],
        args.price,
    )?;
    let bump = [bump_cfg];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.discount_vault,
        &ctx.accounts.user_usdc,
        &ctx.accounts.usdc_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        rebate,
    )?;

    let user_state = &mut ctx.accounts.user_state;
    user_state.member_spent = user_state.member_spent.saturating_add(covered);
    user_state.request_count = user_state.request_count.checked_add(1).ok_or(VaultError::MathOverflow)?;

    write_redemption(
        &mut ctx.accounts.redemption,
        user_key,
        args.request_id,
        args.price,
        args.price,
        rebate,
        covered,
        source::PURCHASE,
        args.item,
        args.platform,
        args.region,
        now,
        cancel_after,
        ctx.bumps.redemption,
    )?;
    emit!(CreditPurchased { user: user_key, id: args.request_id, paid: args.price, rebate });
    emit!(RedemptionRequested {
        user: user_key,
        id: args.request_id,
        amount: args.price,
        source: source::PURCHASE,
        item: args.item,
        platform: args.platform,
        region: args.region,
    });
    Ok(())
}
