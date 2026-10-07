use std::path::PathBuf;
use std::rc::Rc;

use anchor_client::{
    solana_sdk::{
        commitment_config::CommitmentConfig,
        instruction::AccountMeta,
        pubkey::Pubkey,
        signature::{read_keypair_file, Keypair, Signer},
    },
    Client, Cluster,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use solana_multisig::{accounts, instruction, Multisig, Transaction as MultisigTransaction, TxAccountMeta};

#[derive(Parser)]
#[command(name = "multisig-cli", about = "CLI client for the solana-multisig Anchor program")]
struct Cli {
    /// JSON-RPC URL of the cluster to talk to
    #[arg(long, global = true, default_value = "http://127.0.0.1:8899")]
    url: String,

    /// WebSocket URL of the cluster (used for transaction confirmation)
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8900")]
    ws_url: String,

    #[command(subcommand)]
    command: Command,
}

/// Parses "PUBKEY:is_signer(0|1):is_writable(0|1)" into a TxAccountMeta.
fn parse_account_spec(spec: &str) -> Result<TxAccountMeta> {
    let parts: Vec<&str> = spec.split(':').collect();
    anyhow::ensure!(
        parts.len() == 3,
        "expected PUBKEY:is_signer(0|1):is_writable(0|1), got '{spec}'"
    );
    let pubkey: Pubkey = parts[0].parse().context("invalid pubkey in --account")?;
    let is_signer = parts[1] == "1";
    let is_writable = parts[2] == "1";
    Ok(TxAccountMeta {
        pubkey,
        is_signer,
        is_writable,
    })
}

#[derive(Subcommand)]
enum Command {
    /// Create a new M-of-N multisig
    CreateMultisig {
        /// Keypair file for the multisig creator (pays for setup)
        #[arg(long)]
        keypair: PathBuf,
        /// Arbitrary id so one creator can set up multiple multisigs
        #[arg(long)]
        multisig_id: u64,
        /// Owner public keys (comma-separated)
        #[arg(long, value_delimiter = ',')]
        owners: Vec<Pubkey>,
        /// Number of owner approvals required to execute a transaction
        #[arg(long)]
        threshold: u8,
    },
    /// Propose an instruction for the multisig to execute later
    CreateTransaction {
        /// Keypair file for the proposing owner
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        multisig_id: u64,
        /// Program the proposed instruction targets
        #[arg(long)]
        program_id: Pubkey,
        /// An account the instruction references, as PUBKEY:is_signer(0|1):is_writable(0|1).
        /// Repeat for each account, in order.
        #[arg(long = "account")]
        accounts: Vec<String>,
        /// Instruction data, base64-encoded
        #[arg(long)]
        data_base64: String,
    },
    /// Approve a pending proposal
    Approve {
        /// Keypair file for the approving owner
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        multisig_id: u64,
        #[arg(long)]
        tx_index: u64,
    },
    /// Execute a proposal once enough owners have approved
    ExecuteTransaction {
        /// Keypair file to pay the transaction fee (anyone can call this)
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        multisig_id: u64,
        #[arg(long)]
        tx_index: u64,
    },
    /// Print a multisig's state, and optionally one proposal's state
    Show {
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        multisig_id: u64,
        #[arg(long)]
        tx_index: Option<u64>,
    },
}

fn multisig_pda(creator: &Pubkey, multisig_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"multisig", creator.as_ref(), &multisig_id.to_le_bytes()],
        &solana_multisig::ID,
    )
    .0
}

fn vault_pda(multisig: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", multisig.as_ref()], &solana_multisig::ID).0
}

fn transaction_pda(multisig: &Pubkey, tx_index: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"transaction", multisig.as_ref(), &tx_index.to_le_bytes()],
        &solana_multisig::ID,
    )
    .0
}

fn load_keypair(path: &PathBuf) -> Result<Keypair> {
    read_keypair_file(path)
        .map_err(|e| anyhow::anyhow!("failed to read keypair at {}: {e}", path.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cluster = Cluster::Custom(cli.url.clone(), cli.ws_url.clone());

    match cli.command {
        Command::CreateMultisig {
            keypair,
            multisig_id,
            owners,
            threshold,
        } => {
            let creator = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, creator.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_multisig::ID)?;
            let multisig = multisig_pda(&creator.pubkey(), multisig_id);
            let vault = vault_pda(&multisig);

            let sig = program
                .request()
                .accounts(accounts::CreateMultisig {
                    creator: creator.pubkey(),
                    multisig,
                    vault,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::CreateMultisig {
                    multisig_id,
                    owners,
                    threshold,
                })
                .send()
                .context("create_multisig transaction failed")?;

            println!("Multisig created at {multisig}");
            println!("Vault: {vault}");
            println!("Signature: {sig}");
        }

        Command::CreateTransaction {
            keypair,
            creator,
            multisig_id,
            program_id,
            accounts: account_specs,
            data_base64,
        } => {
            let proposer = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, proposer.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_multisig::ID)?;
            let multisig = multisig_pda(&creator, multisig_id);
            let multisig_state: Multisig = program
                .account(multisig)
                .context("failed to fetch multisig (does it exist?)")?;
            let transaction = transaction_pda(&multisig, multisig_state.transaction_count);

            let tx_accounts: Vec<TxAccountMeta> = account_specs
                .iter()
                .map(|s| parse_account_spec(s))
                .collect::<Result<_>>()?;
            let data = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data_base64)
                .context("--data-base64 is not valid base64")?;

            let sig = program
                .request()
                .accounts(accounts::CreateTransaction {
                    proposer: proposer.pubkey(),
                    multisig,
                    transaction,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::CreateTransaction {
                    program_id,
                    accounts: tx_accounts,
                    data,
                })
                .send()
                .context("create_transaction transaction failed")?;

            println!("Proposed transaction {} at {transaction}", multisig_state.transaction_count);
            println!("Signature: {sig}");
        }

        Command::Approve {
            keypair,
            creator,
            multisig_id,
            tx_index,
        } => {
            let owner = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, owner.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_multisig::ID)?;
            let multisig = multisig_pda(&creator, multisig_id);
            let transaction = transaction_pda(&multisig, tx_index);

            let sig = program
                .request()
                .accounts(accounts::Approve {
                    owner: owner.pubkey(),
                    multisig,
                    transaction,
                })
                .args(instruction::Approve {})
                .send()
                .context("approve transaction failed")?;

            println!("Approved transaction {tx_index}");
            println!("Signature: {sig}");
        }

        Command::ExecuteTransaction {
            keypair,
            creator,
            multisig_id,
            tx_index,
        } => {
            let payer = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, payer.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_multisig::ID)?;
            let multisig = multisig_pda(&creator, multisig_id);
            let transaction = transaction_pda(&multisig, tx_index);
            let tx_state: MultisigTransaction = program
                .account(transaction)
                .context("failed to fetch proposal (does it exist?)")?;

            let remaining: Vec<AccountMeta> = tx_state.accounts[..tx_state.account_count as usize]
                .iter()
                .map(|a| AccountMeta {
                    pubkey: a.pubkey,
                    is_signer: false, // the client never signs on these accounts' behalf; the
                    // vault PDA is authorized at runtime via invoke_signed inside the program
                    is_writable: a.is_writable,
                })
                .collect();

            let sig = program
                .request()
                .accounts(accounts::ExecuteTransaction {
                    multisig,
                    transaction,
                    target_program: tx_state.program_id,
                })
                .accounts(remaining)
                .args(instruction::ExecuteTransaction {})
                .send()
                .context("execute_transaction transaction failed")?;

            println!("Executed transaction {tx_index}");
            println!("Signature: {sig}");
        }

        Command::Show {
            creator,
            multisig_id,
            tx_index,
        } => {
            let dummy_payer = Rc::new(Keypair::new());
            let client = Client::new_with_options(cluster, dummy_payer, CommitmentConfig::confirmed());
            let program = client.program(solana_multisig::ID)?;
            let multisig_key = multisig_pda(&creator, multisig_id);
            let multisig: Multisig = program
                .account(multisig_key)
                .context("failed to fetch multisig (does it exist?)")?;

            println!("Multisig: {multisig_key}");
            println!("  creator:            {}", multisig.creator);
            println!("  vault:              {}", vault_pda(&multisig_key));
            println!("  threshold:          {} of {}", multisig.threshold, multisig.owner_count);
            for i in 0..multisig.owner_count as usize {
                println!("    [{i}] {}", multisig.owners[i]);
            }
            println!("  transaction_count:  {}", multisig.transaction_count);

            if let Some(idx) = tx_index {
                let tx_key = transaction_pda(&multisig_key, idx);
                let tx: MultisigTransaction = program
                    .account(tx_key)
                    .context("failed to fetch proposal (does it exist?)")?;
                let approvals: Vec<usize> = (0..multisig.owner_count as usize)
                    .filter(|i| tx.approved_mask & (1 << i) != 0)
                    .collect();
                println!("Transaction {idx}: {tx_key}");
                println!("  program_id:         {}", tx.program_id);
                println!("  account_count:      {}", tx.account_count);
                println!("  data_len:           {}", tx.data_len);
                println!("  approved by owners: {approvals:?}");
                println!(
                    "  approvals:          {}/{}",
                    approvals.len(),
                    multisig.threshold
                );
                println!("  executed:           {}", tx.executed);
            }
        }
    }

    Ok(())
}
