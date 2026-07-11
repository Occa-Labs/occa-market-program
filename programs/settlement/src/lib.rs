// OCCA Settlement Program — non-custodial per-agent USDC vaults.
//
// The cash register of the Open Market's x402 machine rail. Each agent gets a
// program-owned USDC vault; buyers pay into the vault directly (the vault's ATA
// is the x402 `payTo`), and the program — not OCCA — splits every claim into
// the provider's take (the listed price, in full) and the protocol fee (on
// top). There is deliberately no instruction that moves the provider's share
// to anyone but the provider's own wallet.
//
// Owns two account types:
//   • MarketConfig — singleton: authority, pinned USDC mint, fee treasury, fee.
//   • AgentVault   — one per agent: provider payout wallet + a snapshot of the
//                    fee rate its deposits were priced with. Owns the vault ATA.
//
// Companion program:
//   • Registry (occaTHMv…) — owns AgentIdentity, seeded by the same
//     `agent_pubkey` this program seeds a vault with, so a vault and its
//     provenance identity share one key.
//
// Truth model: the chain is authoritative. The market's off-chain
// `x402_charges` table is a rebuildable index over vault deposits and claims,
// never the source of truth for balances.

use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{transfer_checked, Mint, Token, TokenAccount, TransferChecked},
};

declare_id!("occaFcDiKh65LtKoNd7TpDn14YaioRFvVR7wHibdMQo");

/// Registry program ID — the sibling that owns agent identity. Kept for
/// off-chain cross-referencing and future CPI; not dereferenced here yet.
#[allow(dead_code)]
const REGISTRY_PROGRAM_ID: Pubkey = pubkey!("occaTHMv5eYG5aZ85jimxTvHkBfsDCvndXC6J2k8kxr");

/// Account schema version, bumped on any layout change.
const ACCOUNT_VERSION: u8 = 1;

/// Fee ceiling. The market fee is 10% (1000 bps); refuse a config that sets an
/// absurd rate, a cheap guard against a fat-fingered or hostile `set_config`.
const MAX_FEE_BPS: u16 = 2_000;

/// Smallest claimable vault balance, in USDC micro-units (6 decimals → $1).
/// Keeps dust balances from generating spam claim cranks that cost more gas
/// than they move.
const MIN_CLAIM_MICROS: u64 = 1_000_000;

/// Max stored length of the human-readable agent id (e.g. "ape-check").
const MAX_AGENT_ID_LEN: usize = 64;

#[program]
pub mod settlement {
    use super::*;

    /// One-time bootstrap of the singleton config. The signer becomes the
    /// authority (make this a multisig before mainnet). `usdc_mint` is pinned
    /// for the program's life; everything else is forward-tunable.
    pub fn init_config(ctx: Context<InitConfig>, fee_treasury: Pubkey, fee_bps: u16) -> Result<()> {
        require!(fee_bps <= MAX_FEE_BPS, SettlementError::FeeTooHigh);
        let config = &mut ctx.accounts.config;
        config.version = ACCOUNT_VERSION;
        config.authority = ctx.accounts.authority.key();
        config.usdc_mint = ctx.accounts.usdc_mint.key();
        config.fee_treasury = fee_treasury;
        config.fee_bps = fee_bps;
        config.bump = ctx.bumps.config;
        Ok(())
    }

    /// Forward-tune the fee treasury and/or fee rate. The pinned mint is
    /// immutable. A fee change never re-splits money already in a vault —
    /// each vault carries its own fee snapshot (see `init_vault`).
    pub fn set_config(ctx: Context<SetConfig>, params: SetConfigParams) -> Result<()> {
        let config = &mut ctx.accounts.config;
        if let Some(treasury) = params.fee_treasury {
            config.fee_treasury = treasury;
        }
        if let Some(bps) = params.fee_bps {
            require!(bps <= MAX_FEE_BPS, SettlementError::FeeTooHigh);
            config.fee_bps = bps;
        }
        Ok(())
    }

    /// Create an agent's vault and its USDC ATA. Authority-only (called at
    /// agent publish). The vault snapshots the CURRENT config fee so that every
    /// deposit priced against this vault splits at the same rate, whatever the
    /// global fee becomes later.
    pub fn init_vault(
        ctx: Context<InitVault>,
        agent_pubkey: Pubkey,
        agent_id: String,
        provider_wallet: Pubkey,
        identity: Pubkey,
    ) -> Result<()> {
        require!(
            agent_id.len() <= MAX_AGENT_ID_LEN,
            SettlementError::AgentIdTooLong
        );
        require!(
            provider_wallet != Pubkey::default(),
            SettlementError::ProviderWalletUnset
        );
        let vault = &mut ctx.accounts.vault;
        vault.version = ACCOUNT_VERSION;
        vault.agent_pubkey = agent_pubkey;
        vault.agent_id = agent_id;
        vault.provider_wallet = provider_wallet;
        vault.identity = identity;
        vault.fee_bps = ctx.accounts.config.fee_bps;
        vault.claimed_provider = 0;
        vault.claimed_fee = 0;
        vault.bump = ctx.bumps.vault;
        Ok(())
    }

    /// Rotate the provider payout wallet. Only the CURRENT provider wallet can
    /// sign this — the authority explicitly cannot, so a compromised OCCA key
    /// can never redirect a provider's revenue.
    pub fn set_provider_wallet(
        ctx: Context<SetProviderWallet>,
        _agent_pubkey: Pubkey,
        new_wallet: Pubkey,
    ) -> Result<()> {
        require!(
            new_wallet != Pubkey::default(),
            SettlementError::ProviderWalletUnset
        );
        ctx.accounts.vault.provider_wallet = new_wallet;
        Ok(())
    }

    /// Split the vault's full balance: the provider's take to their wallet's
    /// USDC ATA, the fee to the fee treasury's ATA — in one transaction.
    /// Permissionless: anyone may crank it (the provider normally), because the
    /// destinations are fixed by the vault and config, not by the caller.
    pub fn claim(ctx: Context<Claim>, agent_pubkey: Pubkey) -> Result<()> {
        let balance = ctx.accounts.vault_token_account.amount;
        require!(balance >= MIN_CLAIM_MICROS, SettlementError::BelowMinimumClaim);

        let (provider_share, fee_share) = split_accrued(balance, ctx.accounts.vault.fee_bps)?;

        // The vault PDA is the ATA authority; it signs both legs via its seeds.
        let bump = [ctx.accounts.vault.bump];
        let seeds: &[&[u8]] = &[b"vault", agent_pubkey.as_ref(), &bump];
        let signer_seeds: &[&[&[u8]]] = &[seeds];

        spl_transfer_signed(
            &ctx.accounts.token_program,
            &ctx.accounts.vault_token_account,
            &ctx.accounts.provider_token_account,
            &ctx.accounts.usdc_mint,
            ctx.accounts.vault.to_account_info(),
            signer_seeds,
            provider_share,
        )?;

        if fee_share > 0 {
            spl_transfer_signed(
                &ctx.accounts.token_program,
                &ctx.accounts.vault_token_account,
                &ctx.accounts.fee_token_account,
                &ctx.accounts.usdc_mint,
                ctx.accounts.vault.to_account_info(),
                signer_seeds,
                fee_share,
            )?;
        }

        let vault = &mut ctx.accounts.vault;
        vault.claimed_provider = vault
            .claimed_provider
            .checked_add(provider_share)
            .ok_or(SettlementError::ArithmeticOverflow)?;
        vault.claimed_fee = vault
            .claimed_fee
            .checked_add(fee_share)
            .ok_or(SettlementError::ArithmeticOverflow)?;
        Ok(())
    }
}

/// Reverse the "fee on top" pricing: a vault holding `balance = price + fee`
/// splits back into `(provider_share, fee_share)`.
///
///   provider = balance * 10_000 / (10_000 + fee_bps)   (floor)
///   fee      = balance - provider                       (remainder)
///
/// The rounding residue (≤ 1 micro-USD) lands on the fee side, so
/// `provider + fee == balance` holds exactly for any input. Integer math only.
fn split_accrued(balance: u64, fee_bps: u16) -> Result<(u64, u64)> {
    let denom = 10_000u128
        .checked_add(fee_bps as u128)
        .ok_or(SettlementError::ArithmeticOverflow)?;
    let provider = (balance as u128)
        .checked_mul(10_000u128)
        .ok_or(SettlementError::ArithmeticOverflow)?
        .checked_div(denom)
        .ok_or(SettlementError::ArithmeticOverflow)?;
    let provider_u64: u64 = provider
        .try_into()
        .map_err(|_| error!(SettlementError::ArithmeticOverflow))?;
    let fee = balance
        .checked_sub(provider_u64)
        .ok_or(SettlementError::ArithmeticOverflow)?;
    Ok((provider_u64, fee))
}

/// SPL `transfer_checked` CPI signed by the vault PDA. `transfer_checked`
/// re-validates the mint + decimals on-chain, so a wrong-mint token account is
/// rejected by the token program itself.
fn spl_transfer_signed<'info>(
    token_program: &Program<'info, Token>,
    from: &Account<'info, TokenAccount>,
    to: &Account<'info, TokenAccount>,
    mint: &Account<'info, Mint>,
    authority: AccountInfo<'info>,
    signer_seeds: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    let cpi = CpiContext::new_with_signer(
        token_program.key(),
        TransferChecked {
            from: from.to_account_info(),
            mint: mint.to_account_info(),
            to: to.to_account_info(),
            authority,
        },
        signer_seeds,
    );
    transfer_checked(cpi, amount, mint.decimals)
}

// ─────────────────────────────── State ───────────────────────────────

#[account]
#[derive(InitSpace)]
pub struct MarketConfig {
    pub version: u8,
    /// Can init vaults and set_config. Multisig before mainnet.
    pub authority: Pubkey,
    /// Pinned USDC mint. Immutable for the program's life.
    pub usdc_mint: Pubkey,
    /// Wallet that owns the fee-share ATA (fee routing starts here).
    pub fee_treasury: Pubkey,
    /// Fee on top, basis points. 1000 = 10%.
    pub fee_bps: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct AgentVault {
    pub version: u8,
    /// The agent's registry key — also this vault's PDA seed.
    pub agent_pubkey: Pubkey,
    /// Human-readable catalog id, for off-chain readability.
    #[max_len(64)]
    pub agent_id: String,
    /// Where the provider share is paid on claim.
    pub provider_wallet: Pubkey,
    /// Optional link to the registry AgentIdentity PDA.
    pub identity: Pubkey,
    /// Fee rate snapshot at init — claims always split at this rate.
    pub fee_bps: u16,
    /// Lifetime provider payouts (transparency counter).
    pub claimed_provider: u64,
    /// Lifetime fee payouts (transparency counter).
    pub claimed_fee: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetConfigParams {
    pub fee_treasury: Option<Pubkey>,
    pub fee_bps: Option<u16>,
}

// ──────────────────────────── Instructions ───────────────────────────

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + MarketConfig::INIT_SPACE,
        seeds = [b"config"],
        bump,
    )]
    pub config: Account<'info, MarketConfig>,

    #[account(mut)]
    pub authority: Signer<'info>,

    /// The USDC mint to pin. Passed as an account so it must exist and be a
    /// real mint at bootstrap.
    pub usdc_mint: Account<'info, Mint>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetConfig<'info> {
    #[account(
        mut,
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ SettlementError::Unauthorized,
    )]
    pub config: Account<'info, MarketConfig>,

    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(agent_pubkey: Pubkey)]
pub struct InitVault<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
        has_one = authority @ SettlementError::Unauthorized,
    )]
    pub config: Account<'info, MarketConfig>,

    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + AgentVault::INIT_SPACE,
        seeds = [b"vault", agent_pubkey.as_ref()],
        bump,
    )]
    pub vault: Account<'info, AgentVault>,

    #[account(address = config.usdc_mint @ SettlementError::WrongMint)]
    pub usdc_mint: Account<'info, Mint>,

    /// The vault's own USDC ATA — this address is the x402 `payTo`.
    #[account(
        init,
        payer = authority,
        associated_token::mint = usdc_mint,
        associated_token::authority = vault,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(agent_pubkey: Pubkey)]
pub struct SetProviderWallet<'info> {
    #[account(
        mut,
        seeds = [b"vault", agent_pubkey.as_ref()],
        bump = vault.bump,
        has_one = provider_wallet @ SettlementError::Unauthorized,
    )]
    pub vault: Account<'info, AgentVault>,

    /// The current provider wallet — the only signer allowed to rotate.
    pub provider_wallet: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(agent_pubkey: Pubkey)]
pub struct Claim<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump,
    )]
    pub config: Account<'info, MarketConfig>,

    #[account(
        mut,
        seeds = [b"vault", agent_pubkey.as_ref()],
        bump = vault.bump,
        has_one = provider_wallet @ SettlementError::ProviderMismatch,
    )]
    pub vault: Account<'info, AgentVault>,

    #[account(address = config.usdc_mint @ SettlementError::WrongMint)]
    pub usdc_mint: Account<'info, Mint>,

    /// Vault's ATA — the source of funds.
    #[account(
        mut,
        associated_token::mint = usdc_mint,
        associated_token::authority = vault,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    /// Provider wallet (checked against the vault). Its ATA receives the take.
    /// CHECK: constrained by the vault's `has_one = provider_wallet`.
    pub provider_wallet: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = cranker,
        associated_token::mint = usdc_mint,
        associated_token::authority = provider_wallet,
    )]
    pub provider_token_account: Account<'info, TokenAccount>,

    /// Fee treasury wallet (checked against config). Its ATA receives the fee.
    /// CHECK: constrained by `address = config.fee_treasury`.
    #[account(address = config.fee_treasury @ SettlementError::FeeTreasuryMismatch)]
    pub fee_treasury: UncheckedAccount<'info>,

    #[account(
        init_if_needed,
        payer = cranker,
        associated_token::mint = usdc_mint,
        associated_token::authority = fee_treasury,
    )]
    pub fee_token_account: Account<'info, TokenAccount>,

    /// Anyone. Pays the tx fee and any first-time ATA rent.
    #[account(mut)]
    pub cranker: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

// ────────────────────────────── Errors ───────────────────────────────

#[error_code]
pub enum SettlementError {
    #[msg("Only the config authority may perform this action")]
    Unauthorized,
    #[msg("Fee exceeds the maximum allowed basis points")]
    FeeTooHigh,
    #[msg("Agent id exceeds the maximum length")]
    AgentIdTooLong,
    #[msg("Provider wallet cannot be the default pubkey")]
    ProviderWalletUnset,
    #[msg("Token mint does not match the pinned USDC mint")]
    WrongMint,
    #[msg("Provider wallet does not match the vault")]
    ProviderMismatch,
    #[msg("Fee treasury does not match the config")]
    FeeTreasuryMismatch,
    #[msg("Vault balance is below the minimum claim amount")]
    BelowMinimumClaim,
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,
}
