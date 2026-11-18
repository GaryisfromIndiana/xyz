use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::errors::VaultError;
use crate::events::*;
use crate::math;
use crate::state::*;
use crate::util;

#[derive(Accounts)]
pub struct OperatorRedemption<'info> {
    pub operator: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.roles.operator == operator.key() @ VaultError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(
        mut,
        seeds = [REDEMPTION_SEED, redemption.user.as_ref(), &redemption.id.to_le_bytes()],
        bump = redemption.bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
}

/// The operator is about to buy the code. From here the user cannot cancel for
/// MAX_PROCESSING_SECS, so a user cannot cancel mid-delivery and keep both the code and the
/// refund (audit). The operator should mark, buy, fulfil, and only then reveal the code.
pub fn mark_processing_handler(ctx: Context<OperatorRedemption>) -> Result<()> {
    let r = &mut ctx.accounts.redemption;
    require!(r.status == status::PENDING, VaultError::NotPending);
    r.status = status::PROCESSING;
    r.processing_at = Clock::get()?.unix_timestamp;
    emit!(RedemptionProcessing { user: r.user, id: r.id });
    Ok(())
}

#[derive(Accounts)]
pub struct FulfillRedemption<'info> {
    pub operator: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.roles.operator == operator.key() @ VaultError::Unauthorized,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [REDEMPTION_SEED, redemption.user.as_ref(), &redemption.id.to_le_bytes()],
        bump = redemption.bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
    #[account(mut, seeds = [PENDING_VAULT_SEED], bump)]
    pub pending_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Pays for the code purchase off-chain (Bitrefill Business, Tango).
    #[account(mut, address = config.roles.operator_treasury @ VaultError::TreasuryInvalid)]
    pub operator_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// The code was bought; release the pending USDC that paid for it and record the receipt hash.
/// Settlement is capped per day, which bounds what a compromised operator key can take.
pub fn fulfill_redemption_handler(ctx: Context<FulfillRedemption>, receipt_hash: [u8; 32]) -> Result<()> {
    let st = ctx.accounts.redemption.status;
    require!(st == status::PENDING || st == status::PROCESSING, VaultError::NotPending);
    let now = Clock::get()?.unix_timestamp;
    let amount = ctx.accounts.redemption.amount;
    {
        let config = &mut ctx.accounts.config;
        let day = math::day_index(now);
        if config.fulfill_day != day {
            config.fulfill_day = day;
            config.fulfilled_today = 0;
        }
        let after = config.fulfilled_today.checked_add(amount).ok_or(VaultError::MathOverflow)?;
        require!(after <= config.params.max_fulfill_per_day, VaultError::FulfillCapExceeded);
        config.fulfilled_today = after;
    }
    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.pending_vault,
        &ctx.accounts.operator_treasury,
        &ctx.accounts.usdc_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        amount,
    )?;
    let r = &mut ctx.accounts.redemption;
    r.status = status::FULFILLED;
    r.settled_at = now;
    r.receipt_hash = receipt_hash;
    emit!(RedemptionFulfilled { user: r.user, id: r.id, amount, receipt_hash });
    Ok(())
}

#[derive(Accounts)]
pub struct CancelRedemption<'info> {
    /// The redemption's user (after its cancel time) or the operator (any time).
    pub authority: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        seeds = [REDEMPTION_SEED, redemption.user.as_ref(), &redemption.id.to_le_bytes()],
        bump = redemption.bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
    #[account(mut, seeds = [USER_SEED, redemption.user.as_ref()], bump = user_state.bump)]
    pub user_state: Box<Account<'info, UserState>>,
    #[account(mut, seeds = [PENDING_VAULT_SEED], bump)]
    pub pending_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [RESERVE_VAULT_SEED], bump)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [DISCOUNT_VAULT_SEED], bump)]
    pub discount_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Required only for purchases: the user's USDC account that receives the refund.
    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = redemption.user,
        token::token_program = token_program
    )]
    pub user_usdc: Option<Box<InterfaceAccount<'info, TokenAccount>>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// Undo an unsettled redemption. Credit redemptions return USDC to backing and restore the
/// wallet's credit. Purchases refund what the user paid net of the rebate, and the rebate goes
/// back to the discount pool. The user's cancel time is fixed on the request, so later parameter
/// changes cannot lock it (audit).
pub fn cancel_redemption_handler(ctx: Context<CancelRedemption>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let authority = ctx.accounts.authority.key();
    let by_operator = authority == ctx.accounts.config.roles.operator;
    let by_user = authority == ctx.accounts.redemption.user;
    require!(by_operator || by_user, VaultError::Unauthorized);
    let r = (*ctx.accounts.redemption).clone();
    require!(r.status == status::PENDING || r.status == status::PROCESSING, VaultError::NotPending);
    if !by_operator {
        let opens = if r.status == status::PENDING {
            r.cancel_at
        } else {
            r.processing_at.checked_add(MAX_PROCESSING_SECS).ok_or(VaultError::MathOverflow)?
        };
        require!(now >= opens, VaultError::CancelTooEarly);
    }

    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    match r.source {
        source::CREDIT => {
            util::transfer(
                &ctx.accounts.token_program,
                &ctx.accounts.pending_vault,
                &ctx.accounts.reserve_vault,
                &ctx.accounts.usdc_mint,
                ctx.accounts.config.to_account_info(),
                &[seeds],
                r.amount,
            )?;
            let user_state = &mut ctx.accounts.user_state;
            user_state.consumed = user_state.consumed.checked_sub(r.amount).ok_or(VaultError::MathOverflow)?;
            let config = &mut ctx.accounts.config;
            config.total_credit_consumed = config.total_credit_consumed.checked_sub(r.amount).ok_or(VaultError::MathOverflow)?;
        }
        source::PURCHASE => {
            let user_usdc = ctx.accounts.user_usdc.as_ref().ok_or(VaultError::MissingRefundAccount)?;
            let refund = r.user_paid.checked_sub(r.rebate).ok_or(VaultError::MathOverflow)?;
            util::transfer(
                &ctx.accounts.token_program,
                &ctx.accounts.pending_vault,
                user_usdc,
                &ctx.accounts.usdc_mint,
                ctx.accounts.config.to_account_info(),
                &[seeds],
                refund,
            )?;
            util::transfer(
                &ctx.accounts.token_program,
                &ctx.accounts.pending_vault,
                &ctx.accounts.discount_vault,
                &ctx.accounts.usdc_mint,
                ctx.accounts.config.to_account_info(),
                &[seeds],
                r.rebate,
            )?;
            if r.covered > 0 && ctx.accounts.user_state.member_month == math::month_index(r.created_at) {
                let user_state = &mut ctx.accounts.user_state;
                user_state.member_spent = user_state.member_spent.saturating_sub(r.covered);
            }
        }
        _ => return err!(VaultError::InvalidParams),
    }

    let red = &mut ctx.accounts.redemption;
    red.status = status::CANCELLED;
    red.settled_at = now;
    emit!(RedemptionCancelled { user: r.user, id: r.id, amount: r.amount, source: r.source, by_operator });
    Ok(())
}

#[derive(Accounts)]
pub struct CloseRedemption<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(
        mut,
        close = user,
        has_one = user @ VaultError::Unauthorized,
        seeds = [REDEMPTION_SEED, redemption.user.as_ref(), &redemption.id.to_le_bytes()],
        bump = redemption.bump
    )]
    pub redemption: Box<Account<'info, Redemption>>,
}

/// Return the rent of a settled request to its user. The receipt stays in the transaction logs.
pub fn close_redemption_handler(ctx: Context<CloseRedemption>) -> Result<()> {
    let st = ctx.accounts.redemption.status;
    require!(st == status::FULFILLED || st == status::CANCELLED, VaultError::NotSettled);
    Ok(())
}
