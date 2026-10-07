use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

declare_id!("7Z8kKijWMdFhhuTXNoD1WhiE6ZSmy3Bt3ZZHW5cU9szo");

pub const MAX_OWNERS: usize = 10;
pub const MAX_TX_ACCOUNTS: usize = 10;
pub const MAX_TX_DATA_LEN: usize = 300;

#[program]
pub mod solana_multisig {
    use super::*;

    pub fn create_multisig(
        ctx: Context<CreateMultisig>,
        multisig_id: u64,
        owners: Vec<Pubkey>,
        threshold: u8,
    ) -> Result<()> {
        require!(!owners.is_empty(), MultisigError::InvalidThreshold);
        require!(owners.len() <= MAX_OWNERS, MultisigError::TooManyOwners);
        require!(
            threshold >= 1 && threshold as usize <= owners.len(),
            MultisigError::InvalidThreshold
        );
        for i in 0..owners.len() {
            for j in (i + 1)..owners.len() {
                require!(owners[i] != owners[j], MultisigError::DuplicateOwner);
            }
        }

        let multisig = &mut ctx.accounts.multisig;
        multisig.creator = ctx.accounts.creator.key();
        multisig.multisig_id = multisig_id;
        multisig.owners = [Pubkey::default(); MAX_OWNERS];
        for (i, owner) in owners.iter().enumerate() {
            multisig.owners[i] = *owner;
        }
        multisig.owner_count = owners.len() as u8;
        multisig.threshold = threshold;
        multisig.transaction_count = 0;
        multisig.vault_bump = ctx.bumps.vault;
        multisig.bump = ctx.bumps.multisig;

        Ok(())
    }

    pub fn create_transaction(
        ctx: Context<CreateTransaction>,
        program_id: Pubkey,
        accounts: Vec<TxAccountMeta>,
        data: Vec<u8>,
    ) -> Result<()> {
        require!(
            accounts.len() <= MAX_TX_ACCOUNTS,
            MultisigError::TooManyAccounts
        );
        require!(data.len() <= MAX_TX_DATA_LEN, MultisigError::DataTooLarge);

        let (proposer_index, transaction_index) = {
            let multisig = &ctx.accounts.multisig;
            let idx = multisig.owners[..multisig.owner_count as usize]
                .iter()
                .position(|o| *o == ctx.accounts.proposer.key())
                .ok_or(MultisigError::NotAnOwner)?;
            (idx, multisig.transaction_count)
        };

        let tx = &mut ctx.accounts.transaction;
        tx.multisig = ctx.accounts.multisig.key();
        tx.transaction_index = transaction_index;
        tx.program_id = program_id;
        tx.account_count = accounts.len() as u8;
        tx.accounts = [TxAccountMeta::default(); MAX_TX_ACCOUNTS];
        for (i, a) in accounts.iter().enumerate() {
            tx.accounts[i] = *a;
        }
        tx.data_len = data.len() as u16;
        tx.data = [0u8; MAX_TX_DATA_LEN];
        tx.data[..data.len()].copy_from_slice(&data);
        tx.approved_mask = 1 << proposer_index;
        tx.executed = false;
        tx.bump = ctx.bumps.transaction;

        ctx.accounts.multisig.transaction_count = transaction_index
            .checked_add(1)
            .ok_or(MultisigError::MathOverflow)?;

        Ok(())
    }

    pub fn approve(ctx: Context<Approve>) -> Result<()> {
        require!(
            !ctx.accounts.transaction.executed,
            MultisigError::AlreadyExecuted
        );

        let owner_index = ctx.accounts.multisig.owners[..ctx.accounts.multisig.owner_count as usize]
            .iter()
            .position(|o| *o == ctx.accounts.owner.key())
            .ok_or(MultisigError::NotAnOwner)?;

        require!(
            ctx.accounts.transaction.approved_mask & (1 << owner_index) == 0,
            MultisigError::AlreadyApproved
        );

        ctx.accounts.transaction.approved_mask |= 1 << owner_index;

        Ok(())
    }

    pub fn execute_transaction<'info>(
        ctx: Context<'_, '_, 'info, 'info, ExecuteTransaction<'info>>,
    ) -> Result<()> {
        require!(
            !ctx.accounts.transaction.executed,
            MultisigError::AlreadyExecuted
        );
        let approvals = ctx.accounts.transaction.approved_mask.count_ones();
        require!(
            approvals >= ctx.accounts.multisig.threshold as u32,
            MultisigError::NotEnoughApprovals
        );
        require_keys_eq!(
            ctx.accounts.target_program.key(),
            ctx.accounts.transaction.program_id,
            MultisigError::AccountMismatch
        );

        let account_count = ctx.accounts.transaction.account_count as usize;
        let stored_accounts = ctx.accounts.transaction.accounts;
        let stored_program_id = ctx.accounts.transaction.program_id;
        let stored_data_len = ctx.accounts.transaction.data_len as usize;
        let stored_data = ctx.accounts.transaction.data;

        require!(
            ctx.remaining_accounts.len() == account_count,
            MultisigError::AccountMismatch
        );
        for (i, account_info) in ctx.remaining_accounts.iter().enumerate() {
            require_keys_eq!(
                account_info.key(),
                stored_accounts[i].pubkey,
                MultisigError::AccountMismatch
            );
        }

        let ix_accounts: Vec<AccountMeta> = stored_accounts[..account_count]
            .iter()
            .map(|a| AccountMeta {
                pubkey: a.pubkey,
                is_signer: a.is_signer,
                is_writable: a.is_writable,
            })
            .collect();
        let ix = Instruction {
            program_id: stored_program_id,
            accounts: ix_accounts,
            data: stored_data[..stored_data_len].to_vec(),
        };

        let mut account_infos: Vec<AccountInfo> = ctx.remaining_accounts.to_vec();
        account_infos.push(ctx.accounts.target_program.to_account_info());

        let multisig_key = ctx.accounts.multisig.key();
        let vault_bump = ctx.accounts.multisig.vault_bump;
        let vault_seeds: &[&[u8]] = &[b"vault", multisig_key.as_ref(), &[vault_bump]];

        invoke_signed(&ix, &account_infos, &[vault_seeds])?;

        ctx.accounts.transaction.executed = true;

        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct TxAccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[account]
pub struct Multisig {
    pub creator: Pubkey,
    pub multisig_id: u64,
    pub owners: [Pubkey; MAX_OWNERS],
    pub owner_count: u8,
    pub threshold: u8,
    pub transaction_count: u64,
    pub vault_bump: u8,
    pub bump: u8,
}

impl Multisig {
    pub const MAX_SIZE: usize = 8 // discriminator
        + 32 // creator
        + 8 // multisig_id
        + 32 * MAX_OWNERS // owners
        + 1 // owner_count
        + 1 // threshold
        + 8 // transaction_count
        + 1 // vault_bump
        + 1; // bump
}

#[account]
pub struct Transaction {
    pub multisig: Pubkey,
    pub transaction_index: u64,
    pub program_id: Pubkey,
    pub account_count: u8,
    pub accounts: [TxAccountMeta; MAX_TX_ACCOUNTS],
    pub data_len: u16,
    pub data: [u8; MAX_TX_DATA_LEN],
    pub approved_mask: u16,
    pub executed: bool,
    pub bump: u8,
}

impl Transaction {
    pub const MAX_SIZE: usize = 8 // discriminator
        + 32 // multisig
        + 8 // transaction_index
        + 32 // program_id
        + 1 // account_count
        + (32 + 1 + 1) * MAX_TX_ACCOUNTS // accounts
        + 2 // data_len
        + MAX_TX_DATA_LEN // data
        + 2 // approved_mask
        + 1 // executed
        + 1; // bump
}

#[derive(Accounts)]
#[instruction(multisig_id: u64)]
pub struct CreateMultisig<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(
        init,
        payer = creator,
        space = Multisig::MAX_SIZE,
        seeds = [b"multisig", creator.key().as_ref(), multisig_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub multisig: Account<'info, Multisig>,

    /// CHECK: pure PDA signing authority (the "vault"); holds no data of its own and is
    /// never deserialized, only ever used as the `authority`/`from` side of CPIs this
    /// multisig approves, signed via its seeds in `execute_transaction`.
    #[account(seeds = [b"vault", multisig.key().as_ref()], bump)]
    pub vault: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateTransaction<'info> {
    #[account(mut)]
    pub proposer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"multisig", multisig.creator.as_ref(), multisig.multisig_id.to_le_bytes().as_ref()],
        bump = multisig.bump,
    )]
    pub multisig: Account<'info, Multisig>,

    #[account(
        init,
        payer = proposer,
        space = Transaction::MAX_SIZE,
        seeds = [b"transaction", multisig.key().as_ref(), multisig.transaction_count.to_le_bytes().as_ref()],
        bump,
    )]
    pub transaction: Account<'info, Transaction>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Approve<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"multisig", multisig.creator.as_ref(), multisig.multisig_id.to_le_bytes().as_ref()],
        bump = multisig.bump,
    )]
    pub multisig: Account<'info, Multisig>,

    #[account(
        mut,
        constraint = transaction.multisig == multisig.key() @ MultisigError::AccountMismatch,
        seeds = [b"transaction", multisig.key().as_ref(), transaction.transaction_index.to_le_bytes().as_ref()],
        bump = transaction.bump,
    )]
    pub transaction: Account<'info, Transaction>,
}

#[derive(Accounts)]
pub struct ExecuteTransaction<'info> {
    #[account(
        seeds = [b"multisig", multisig.creator.as_ref(), multisig.multisig_id.to_le_bytes().as_ref()],
        bump = multisig.bump,
    )]
    pub multisig: Account<'info, Multisig>,

    #[account(
        mut,
        constraint = transaction.multisig == multisig.key() @ MultisigError::AccountMismatch,
        seeds = [b"transaction", multisig.key().as_ref(), transaction.transaction_index.to_le_bytes().as_ref()],
        bump = transaction.bump,
    )]
    pub transaction: Account<'info, Transaction>,

    /// CHECK: validated against transaction.program_id; invoke_signed will itself fail
    /// if this isn't actually an executable program.
    pub target_program: UncheckedAccount<'info>,
    // remaining_accounts: every account the stored instruction references, in the same
    // order it was proposed with.
}

#[error_code]
pub enum MultisigError {
    #[msg("A multisig can have at most MAX_OWNERS owners")]
    TooManyOwners,
    #[msg("threshold must be between 1 and the number of owners")]
    InvalidThreshold,
    #[msg("Duplicate owner in the owners list")]
    DuplicateOwner,
    #[msg("Signer is not an owner of this multisig")]
    NotAnOwner,
    #[msg("A proposed transaction can reference at most MAX_TX_ACCOUNTS accounts")]
    TooManyAccounts,
    #[msg("Proposed instruction data exceeds MAX_TX_DATA_LEN")]
    DataTooLarge,
    #[msg("This owner has already approved this transaction")]
    AlreadyApproved,
    #[msg("This transaction has already been executed")]
    AlreadyExecuted,
    #[msg("Not enough owner approvals to meet the threshold yet")]
    NotEnoughApprovals,
    #[msg("Provided accounts do not match what was proposed")]
    AccountMismatch,
    #[msg("Arithmetic overflow")]
    MathOverflow,
}
