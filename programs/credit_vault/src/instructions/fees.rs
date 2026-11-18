use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token_2022::spl_token_2022;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::errors::VaultError;
use crate::events::*;
use crate::math;
use crate::state::*;
use crate::util;

#[derive(Accounts)]
pub struct CollectFees<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = token_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, mint::token_program = token_2022_program)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [FEE_VAULT_SEED], bump)]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
}

/// Permissionless. Pulls fees already harvested to the mint (callers first send Token-2022's
/// permissionless `harvest_withheld_tokens_to_mint` in the same transaction) into the
/// program's fee vault, then burns `burn_bps` of them. Only this program can sign as the
/// mint's withheld-fee authority, so the tax has nowhere else to go.
pub fn collect_fees_handler(ctx: Context<CollectFees>) -> Result<()> {
    let before = ctx.accounts.fee_vault.amount;
    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];

    let ix = spl_token_2022::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_mint(
        &ctx.accounts.token_2022_program.key(),
        &ctx.accounts.token_mint.key(),
        &ctx.accounts.fee_vault.key(),
        &ctx.accounts.config.key(),
        &[],
    )?;
    invoke_signed(
        &ix,
        &[
            ctx.accounts.token_mint.to_account_info(),
            ctx.accounts.fee_vault.to_account_info(),
            ctx.accounts.config.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
        ],
        &[seeds],
    )?;
    ctx.accounts.fee_vault.reload()?;
    let collected = ctx
        .accounts
        .fee_vault
        .amount
        .checked_sub(before)
        .ok_or(VaultError::MathOverflow)?;
    require!(collected > 0, VaultError::ZeroAmount);

    let burned = math::burn_amount(collected, ctx.accounts.config.params.splits.burn_bps);
    util::burn(
        &ctx.accounts.token_2022_program,
        &ctx.accounts.token_mint,
        &ctx.accounts.fee_vault,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        burned,
    )?;

    let now = Clock::get()?.unix_timestamp;
    let config = &mut ctx.accounts.config;
    config.total_fees_collected = config.total_fees_collected.checked_add(collected).ok_or(VaultError::MathOverflow)?;
    config.total_fees_burned = config.total_fees_burned.checked_add(burned).ok_or(VaultError::MathOverflow)?;
    // A fresh lot starts a new auction: from the last sale price (not from wherever the price
    // decayed to), within the growth cap. "Fresh" means the vault was empty, or the new fees are at
    // least as large as what was left, so leaving dust behind cannot make a big new batch sell at
    // a decayed price.
    let added = collected - burned;
    if added > 0 && (before == 0 || added >= before) {
        let start = math::new_lot_start(&config.auction, now);
        config.auction.start_price = start;
        config.auction.started_at = now;
        config.auction.tuning = config.params.auction;
        emit!(AuctionStarted { start_price: start });
    }
    emit!(FeesCollected { collected, burned });
    Ok(())
}

#[derive(Accounts)]
pub struct BuyFees<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = token_mint,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_2022_program)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [FEE_VAULT_SEED], bump)]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [RESERVE_VAULT_SEED], bump)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [DISCOUNT_VAULT_SEED], bump)]
    pub discount_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [OPS_VAULT_SEED], bump)]
    pub ops_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = buyer,
        token::token_program = token_program
    )]
    pub buyer_usdc: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = token_mint, token::token_program = token_2022_program)]
    pub buyer_token: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
}

/// Permissionless Dutch auction: anyone buys harvested fee tokens for USDC at the current
/// decaying price. Buys never reset or mark up the running auction (audit: dust buys used to
/// compound the price), so a purchase of any size simply pays the curve. Proceeds split into
/// reserve / discount / ops. The buyer receives the tokens minus the token's own transfer fee,
/// which recycles into the vault.
pub fn buy_fees_handler(ctx: Context<BuyFees>, token_amount: u64, max_usdc: u64) -> Result<()> {
    require!(token_amount > 0, VaultError::ZeroAmount);
    require!(token_amount <= ctx.accounts.fee_vault.amount, VaultError::AuctionEmpty);

    let now = Clock::get()?.unix_timestamp;
    let price = math::auction_price(&ctx.accounts.config.auction, now);
    let usdc = math::usdc_for_tokens(token_amount, price, ctx.accounts.config.token_decimals)?;
    require!(usdc > 0, VaultError::ZeroAmount);
    require!(usdc <= max_usdc, VaultError::SlippageExceeded);
    let split = math::split_proceeds(usdc, &ctx.accounts.config.params.splits);

    let buyer = ctx.accounts.buyer.to_account_info();
    for (vault, amount) in [
        (&ctx.accounts.reserve_vault, split.reserve),
        (&ctx.accounts.discount_vault, split.discount),
        (&ctx.accounts.ops_vault, split.ops),
    ] {
        util::transfer(
            &ctx.accounts.token_program,
            &ctx.accounts.buyer_usdc,
            vault,
            &ctx.accounts.usdc_mint,
            buyer.clone(),
            &[],
            amount,
        )?;
    }

    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_2022_program,
        &ctx.accounts.fee_vault,
        &ctx.accounts.buyer_token,
        &ctx.accounts.token_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        token_amount,
    )?;

    let config = &mut ctx.accounts.config;
    config.total_reserve_in = config.total_reserve_in.checked_add(split.reserve).ok_or(VaultError::MathOverflow)?;
    config.auction.last_clear_price = price;
    emit!(FeesSold {
        buyer: ctx.accounts.buyer.key(),
        tokens: token_amount,
        usdc,
        price,
        to_reserve: split.reserve,
        to_discount: split.discount,
        to_ops: split.ops,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct RestartAuction<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds = [FEE_VAULT_SEED], bump)]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// Permissionless. If a lot sat unsold through a full auction, restart from the floor so the
/// price keeps falling until the market clears it. Refused while there is nothing to sell, so the
/// price cannot be walked down to zero between lots (audit).
pub fn restart_auction_handler(ctx: Context<RestartAuction>) -> Result<()> {
    require!(ctx.accounts.fee_vault.amount > 0, VaultError::FeeVaultEmpty);
    let now = Clock::get()?.unix_timestamp;
    let a = ctx.accounts.config.auction;
    require!(now - a.started_at >= a.tuning.duration_secs, VaultError::AuctionNotExpired);
    let floor = math::floor_price(&a);
    let config = &mut ctx.accounts.config;
    config.auction.start_price = floor;
    config.auction.started_at = now;
    config.auction.tuning = config.params.auction;
    emit!(AuctionRestarted { start_price: floor });
    Ok(())
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    pub depositor: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = usdc_mint)]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = usdc_mint,
        token::authority = depositor,
        token::token_program = token_program
    )]
    pub depositor_usdc: Box<InterfaceAccount<'info, TokenAccount>>,
    /// The reserve (backing) vault for `donate_to_reserve`, the bonus vault for `deposit_bonus`.
    #[account(mut, token::mint = usdc_mint, token::authority = config, token::token_program = token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

fn reserve_vault_key() -> Pubkey {
    Pubkey::find_program_address(&[RESERVE_VAULT_SEED], &crate::ID).0
}

fn bonus_vault_key() -> Pubkey {
    Pubkey::find_program_address(&[BONUS_VAULT_SEED], &crate::ID).0
}

/// Anyone can add USDC straight to backing (counts toward `total_reserve_in` immediately).
pub fn donate_to_reserve_handler(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::ZeroAmount);
    require_keys_eq!(ctx.accounts.vault.key(), reserve_vault_key(), VaultError::InvalidParams);
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.depositor_usdc,
        &ctx.accounts.vault,
        &ctx.accounts.usdc_mint,
        ctx.accounts.depositor.to_account_info(),
        &[],
        amount,
    )?;
    let config = &mut ctx.accounts.config;
    config.total_reserve_in = config.total_reserve_in.checked_add(amount).ok_or(VaultError::MathOverflow)?;
    emit!(ReserveDonated { donor: ctx.accounts.depositor.key(), amount });
    Ok(())
}

/// Anyone can add USDC to the bonus vault (yield, launch-week bonus). It does not back credit
/// until the admin promotes it.
pub fn deposit_bonus_handler(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::ZeroAmount);
    require_keys_eq!(ctx.accounts.vault.key(), bonus_vault_key(), VaultError::InvalidParams);
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.depositor_usdc,
        &ctx.accounts.vault,
        &ctx.accounts.usdc_mint,
        ctx.accounts.depositor.to_account_info(),
        &[],
        amount,
    )?;
    emit!(BonusDeposited { from: ctx.accounts.depositor.key(), amount });
    Ok(())
}

#[derive(Accounts)]
pub struct PromoteBonus<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [BONUS_VAULT_SEED], bump)]
    pub bonus_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [RESERVE_VAULT_SEED], bump)]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// Move bonus money into backing, e.g. to fund the store-open bonus. One-way: backing never
/// flows back to bonus, so promoting can only make the vault more solvent.
pub fn promote_bonus_handler(ctx: Context<PromoteBonus>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::ZeroAmount);
    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.bonus_vault,
        &ctx.accounts.reserve_vault,
        &ctx.accounts.usdc_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        amount,
    )?;
    let config = &mut ctx.accounts.config;
    config.total_reserve_in = config.total_reserve_in.checked_add(amount).ok_or(VaultError::MathOverflow)?;
    config.total_bonus_promoted = config.total_bonus_promoted.checked_add(amount).ok_or(VaultError::MathOverflow)?;
    emit!(BonusPromoted { amount, total_reserve_in: config.total_reserve_in });
    Ok(())
}
