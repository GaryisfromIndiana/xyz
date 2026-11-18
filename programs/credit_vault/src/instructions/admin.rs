use anchor_lang::prelude::*;
use anchor_spl::token::Token;
use anchor_spl::token_2022::spl_token_2022::{
    self,
    extension::{transfer_fee::TransferFeeConfig, BaseStateWithExtensions, ExtensionType, StateWithExtensions},
};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

use crate::errors::VaultError;
use crate::events::*;
use crate::math;
use crate::state::*;
use crate::util;

/// Mint extensions the vault tolerates. Everything else is refused, because extensions like a
/// permanent delegate, a transfer hook, a default frozen state or a confidential-transfer fee
/// authority could move, block or divert the tax (audit).
const ALLOWED_MINT_EXTENSIONS: &[ExtensionType] = &[
    ExtensionType::TransferFeeConfig,
    ExtensionType::MetadataPointer,
    ExtensionType::TokenMetadata,
];

/// Program-owned vault addresses; the operator treasury may not be one of them.
pub(crate) fn is_vault(key: &Pubkey) -> bool {
    [
        FEE_VAULT_SEED,
        RESERVE_VAULT_SEED,
        DISCOUNT_VAULT_SEED,
        OPS_VAULT_SEED,
        PENDING_VAULT_SEED,
        BONUS_VAULT_SEED,
    ]
    .iter()
    .any(|seed| Pubkey::find_program_address(&[seed], &crate::ID).0 == *key)
}

/// Only this program can take the tax: the withheld-fee authority must be the config PDA, the
/// fee-rate authority the PDA or none, supply fixed, no freeze authority, no risky extensions.
fn check_token_mint(mint: &AccountInfo, config_key: &Pubkey) -> Result<()> {
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
        .map_err(|_| error!(VaultError::NotTransferFeeMint))?;
    let extensions = state
        .get_extension_types()
        .map_err(|_| error!(VaultError::MintExtensionNotAllowed))?;
    for ext in extensions {
        require!(ALLOWED_MINT_EXTENSIONS.contains(&ext), VaultError::MintExtensionNotAllowed);
    }
    require!(state.base.mint_authority.is_none(), VaultError::MintAuthorityPresent);
    require!(state.base.freeze_authority.is_none(), VaultError::FreezeAuthorityPresent);
    let fee = state
        .get_extension::<TransferFeeConfig>()
        .map_err(|_| error!(VaultError::NotTransferFeeMint))?;
    let withdraw: Option<Pubkey> = fee.withdraw_withheld_authority.into();
    require!(withdraw == Some(*config_key), VaultError::MintFeeAuthorityMismatch);
    let rate_authority: Option<Pubkey> = fee.transfer_fee_config_authority.into();
    require!(
        rate_authority.is_none() || rate_authority == Some(*config_key),
        VaultError::MintFeeAuthorityMismatch
    );
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct InitializeParams {
    pub indexer: Pubkey,
    pub operator: Pubkey,
    pub params: Params,
    pub start_price: u64,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + Config::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_2022_program)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Must be on the classic SPL Token program (no Token-2022 stablecoins with fees or delegates).
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(token::mint = usdc_mint, token::token_program = token_program)]
    pub operator_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Only the program's upgrade authority may initialize, so nobody can front-run the
    /// singleton config after deployment (audit).
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ VaultError::Unauthorized)]
    pub program: Program<'info, crate::program::CreditVault>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ VaultError::Unauthorized)]
    pub program_data: Box<Account<'info, ProgramData>>,
    pub token_program: Program<'info, Token>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn initialize_handler(ctx: Context<Initialize>, p: InitializeParams) -> Result<()> {
    math::validate_params(&p.params)?;
    require!(p.start_price > 0, VaultError::InvalidParams);
    require!(ctx.accounts.usdc_mint.decimals == USDC_DECIMALS, VaultError::InvalidUsdcMint);
    require!(!is_vault(&ctx.accounts.operator_treasury.key()), VaultError::TreasuryInvalid);
    check_token_mint(&ctx.accounts.token_mint.to_account_info(), &ctx.accounts.config.key())?;

    let now = Clock::get()?.unix_timestamp;
    let treasury = ctx.accounts.operator_treasury.key();
    let config = &mut ctx.accounts.config;
    config.admin = ctx.accounts.admin.key();
    config.pending_admin = Pubkey::default();
    config.roles = Roles { indexer: p.indexer, operator: p.operator, operator_treasury: treasury };
    config.token_mint = ctx.accounts.token_mint.key();
    config.usdc_mint = ctx.accounts.usdc_mint.key();
    config.token_decimals = ctx.accounts.token_mint.decimals;
    config.params = p.params;
    config.auction = Auction {
        start_price: p.start_price,
        started_at: now,
        tuning: p.params.auction,
        last_clear_price: 0,
    };
    config.active_root = RootSlot::default();
    config.pending_root = RootSlot::default();
    config.has_pending_root = false;
    config.pending_activates_at = 0;
    config.root_posted_at = 0;
    config.total_reserve_in = 0;
    config.total_credit_consumed = 0;
    config.total_bonus_promoted = 0;
    config.total_fees_collected = 0;
    config.total_fees_burned = 0;
    config.fulfill_day = 0;
    config.fulfilled_today = 0;
    config.paused_until = 0;
    config.pending_change = PendingChange {
        active: false,
        eta: 0,
        roles: config.roles,
        params: p.params,
        reanchor_price: 0,
    };
    config.bump = ctx.bumps.config;
    config.reserved = [0u8; 64];
    emit!(ConfigApplied { indexer: p.indexer, operator: p.operator, operator_treasury: treasury });
    Ok(())
}

#[derive(Accounts)]
pub struct InitVaultsA<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized,
        has_one = token_mint,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = admin,
        seeds = [FEE_VAULT_SEED],
        bump,
        token::mint = token_mint,
        token::authority = config,
        token::token_program = token_2022_program
    )]
    pub fee_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = admin,
        seeds = [RESERVE_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = config,
        token::token_program = token_program
    )]
    pub reserve_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = admin,
        seeds = [DISCOUNT_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = config,
        token::token_program = token_program
    )]
    pub discount_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn init_vaults_a_handler(_ctx: Context<InitVaultsA>) -> Result<()> {
    Ok(())
}

#[derive(Accounts)]
pub struct InitVaultsB<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = admin,
        seeds = [OPS_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = config,
        token::token_program = token_program
    )]
    pub ops_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = admin,
        seeds = [PENDING_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = config,
        token::token_program = token_program
    )]
    pub pending_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Yield, launch-week bonus money and top-ups. Never backs credit until promoted.
    #[account(
        init,
        payer = admin,
        seeds = [BONUS_VAULT_SEED],
        bump,
        token::mint = usdc_mint,
        token::authority = config,
        token::token_program = token_program
    )]
    pub bonus_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

pub fn init_vaults_b_handler(_ctx: Context<InitVaultsB>) -> Result<()> {
    Ok(())
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized
    )]
    pub config: Box<Account<'info, Config>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct ConfigChangeArgs {
    pub roles: Roles,
    pub params: Params,
    pub reanchor_price: u64,
}

/// Queue a change to roles and parameters. It can be applied only after CONFIG_DELAY_SECS,
/// which gives holders and monitors two days' notice of any new indexer, operator or treasury.
pub fn propose_config_handler(ctx: Context<AdminOnly>, change: ConfigChangeArgs) -> Result<()> {
    math::validate_params(&change.params)?;
    require!(!is_vault(&change.roles.operator_treasury), VaultError::TreasuryInvalid);
    let eta = Clock::get()?
        .unix_timestamp
        .checked_add(CONFIG_DELAY_SECS)
        .ok_or(VaultError::MathOverflow)?;
    ctx.accounts.config.pending_change = PendingChange {
        active: true,
        eta,
        roles: change.roles,
        params: change.params,
        reanchor_price: change.reanchor_price,
    };
    emit!(ConfigProposed { eta });
    Ok(())
}

pub fn cancel_config_handler(ctx: Context<AdminOnly>) -> Result<()> {
    require!(ctx.accounts.config.pending_change.active, VaultError::NoPendingChange);
    ctx.accounts.config.pending_change.active = false;
    emit!(ConfigCancelled {});
    Ok(())
}

#[derive(Accounts)]
pub struct ApplyConfig<'info> {
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
    /// The proposed treasury, checked to be a real USDC token account.
    #[account(
        address = config.pending_change.roles.operator_treasury @ VaultError::TreasuryInvalid,
        token::mint = usdc_mint,
        token::token_program = token_program
    )]
    pub operator_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

pub fn apply_config_handler(ctx: Context<ApplyConfig>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let config = &mut ctx.accounts.config;
    let change = config.pending_change;
    require!(change.active, VaultError::NoPendingChange);
    require!(now >= change.eta, VaultError::TimelockActive);
    config.roles = change.roles;
    // Auction tuning in `params` is picked up by the next auction that starts, never the live one.
    config.params = change.params;
    if change.reanchor_price > 0 {
        config.auction.start_price = change.reanchor_price;
        config.auction.started_at = now;
        config.auction.tuning = change.params.auction;
        emit!(AuctionStarted { start_price: change.reanchor_price });
    }
    config.pending_change.active = false;
    emit!(ConfigApplied {
        indexer: change.roles.indexer,
        operator: change.roles.operator,
        operator_treasury: change.roles.operator_treasury,
    });
    Ok(())
}

/// Emergency pause of new redemptions and purchases. Expires on its own after MAX_PAUSE_SECS.
pub fn set_paused_handler(ctx: Context<AdminOnly>, paused: bool) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let until = if paused { now.checked_add(MAX_PAUSE_SECS).ok_or(VaultError::MathOverflow)? } else { 0 };
    ctx.accounts.config.paused_until = until;
    emit!(PauseSet { paused_until: until });
    Ok(())
}

/// Step one of an admin handover; the new admin must accept.
pub fn transfer_admin_handler(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
    require_keys_neq!(new_admin, Pubkey::default(), VaultError::InvalidParams);
    ctx.accounts.config.pending_admin = new_admin;
    emit!(AdminProposed { pending_admin: new_admin });
    Ok(())
}

#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    pub new_admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
}

pub fn accept_admin_handler(ctx: Context<AcceptAdmin>) -> Result<()> {
    let config = &mut ctx.accounts.config;
    require_keys_eq!(ctx.accounts.new_admin.key(), config.pending_admin, VaultError::NotPendingAdmin);
    config.admin = config.pending_admin;
    config.pending_admin = Pubkey::default();
    emit!(ConfigUpdated { admin: config.admin });
    Ok(())
}

#[derive(Accounts)]
pub struct SetTransferFee<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized,
        has_one = token_mint
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, mint::token_program = token_2022_program)]
    pub token_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address = anchor_spl::token_2022::ID)]
    pub token_2022_program: Interface<'info, TokenInterface>,
}

/// Queue a lower transfer fee (the step-down 3% -> 2% -> 1%). Token-2022 applies it two epochs
/// later. The rate must strictly fall (a same-rate call would only postpone a queued cut) and the
/// per-transfer cap may not rise.
pub fn set_transfer_fee_handler(ctx: Context<SetTransferFee>, basis_points: u16, maximum_fee: u64) -> Result<()> {
    require!(basis_points <= MAX_TRANSFER_FEE_BPS, VaultError::InvalidParams);
    let (current_bps, current_max) = {
        let mint_ai = ctx.accounts.token_mint.to_account_info();
        let data = mint_ai.try_borrow_data()?;
        let state = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&data)
            .map_err(|_| error!(VaultError::NotTransferFeeMint))?;
        let fee = state
            .get_extension::<TransferFeeConfig>()
            .map_err(|_| error!(VaultError::NotTransferFeeMint))?;
        (
            u16::from(fee.newer_transfer_fee.transfer_fee_basis_points),
            u64::from(fee.newer_transfer_fee.maximum_fee),
        )
    };
    require!(basis_points < current_bps, VaultError::FeeCanOnlyDecrease);
    require!(maximum_fee <= current_max, VaultError::FeeCanOnlyDecrease);

    let ix = spl_token_2022::extension::transfer_fee::instruction::set_transfer_fee(
        &ctx.accounts.token_2022_program.key(),
        &ctx.accounts.token_mint.key(),
        &ctx.accounts.config.key(),
        &[],
        basis_points,
        maximum_fee,
    )?;
    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    anchor_lang::solana_program::program::invoke_signed(
        &ix,
        &[
            ctx.accounts.token_mint.to_account_info(),
            ctx.accounts.config.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
        ],
        &[seeds],
    )?;
    emit!(TransferFeeLowered { basis_points, maximum_fee });
    Ok(())
}

#[derive(Accounts)]
pub struct WithdrawOps<'info> {
    pub admin: Signer<'info>,
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = admin @ VaultError::Unauthorized,
        has_one = usdc_mint
    )]
    pub config: Box<Account<'info, Config>>,
    #[account(mint::token_program = token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, seeds = [OPS_VAULT_SEED], bump)]
    pub ops_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, token::mint = usdc_mint, token::token_program = token_program)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Interface<'info, TokenInterface>,
}

/// Only the operations share can leave the program at the admin's request.
pub fn withdraw_ops_handler(ctx: Context<WithdrawOps>, amount: u64) -> Result<()> {
    require!(amount > 0, VaultError::ZeroAmount);
    let bump = [ctx.accounts.config.bump];
    let seeds: &[&[u8]] = &[CONFIG_SEED, &bump];
    util::transfer(
        &ctx.accounts.token_program,
        &ctx.accounts.ops_vault,
        &ctx.accounts.destination,
        &ctx.accounts.usdc_mint,
        ctx.accounts.config.to_account_info(),
        &[seeds],
        amount,
    )?;
    emit!(OpsWithdrawn { amount, destination: ctx.accounts.destination.key() });
    Ok(())
}
