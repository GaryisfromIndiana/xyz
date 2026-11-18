use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Burn, Mint, TokenAccount, TokenInterface, TransferChecked};

/// Token transfer, skipped when `amount` is zero. `signer` is empty for user-signed transfers.
#[allow(clippy::too_many_arguments)]
pub fn transfer<'info>(
    token_program: &Interface<'info, TokenInterface>,
    from: &InterfaceAccount<'info, TokenAccount>,
    to: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    authority: AccountInfo<'info>,
    signer: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let accounts = TransferChecked {
        from: from.to_account_info(),
        mint: mint.to_account_info(),
        to: to.to_account_info(),
        authority,
    };
    let ctx = CpiContext::new_with_signer(token_program.to_account_info(), accounts, signer);
    token_interface::transfer_checked(ctx, amount, mint.decimals)
}

pub fn burn<'info>(
    token_program: &Interface<'info, TokenInterface>,
    mint: &InterfaceAccount<'info, Mint>,
    from: &InterfaceAccount<'info, TokenAccount>,
    authority: AccountInfo<'info>,
    signer: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let accounts = Burn {
        mint: mint.to_account_info(),
        from: from.to_account_info(),
        authority,
    };
    let ctx = CpiContext::new_with_signer(token_program.to_account_info(), accounts, signer);
    token_interface::burn(ctx, amount)
}
