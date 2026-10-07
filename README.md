# solana-multisig

An Anchor program for an M-of-N multisig wallet: N owners, any M of them must approve before an action executes. Used everywhere for treasuries and admin control instead of trusting a single key.

## Why this shape

This is the **generic, arbitrary-instruction multisig** pattern — the same shape as the long-standing `coral-xyz/multisig` reference implementation that real Solana multisig products (Squads, etc.) build on, not a narrower "just moves SOL" toy version. The wallet doesn't hold funds directly as a data field; it stores a *proposed instruction* (target program, account list, instruction data) on-chain, collects owner approvals, then replays that exact instruction via CPI once threshold is met — signed by a program-derived "vault" address using `invoke_signed`.

This generalizes to controlling anything a Solana account can be the authority over — a token account, a program's upgrade authority, another program's admin — not just a SOL balance. The test suite demonstrates it with a native `system_program::transfer`, but swapping in a different `program_id`/accounts/data proposes a completely different kind of action with no code changes.

## Instructions

| Instruction | Signer | Description |
|---|---|---|
| `create_multisig(multisig_id, owners, threshold)` | creator | Creates the `Multisig` config and derives its vault PDA. Max 10 owners. |
| `create_transaction(program_id, accounts, data)` | any owner | Proposes an instruction to run later. Auto-approves from the proposer. Max 10 accounts, 300 bytes of data. |
| `approve()` | any other owner | Adds their approval to the pending proposal. |
| `execute_transaction()` | anyone (permissionless) | Once approvals ≥ threshold, replays the stored instruction via CPI, signed by the vault PDA. |

## Accounts

**`Multisig`** — PDA at `["multisig", creator, multisig_id]`
- `creator`, `multisig_id`, `owners: [Pubkey; 10]`, `owner_count`, `threshold`
- `transaction_count` (nonce for proposals), `vault_bump`, `bump`

**Vault** — not its own data account, just the PDA `["vault", multisig]`. Holds no state; it's purely a signing authority. The test suite funds it with a plain SOL airdrop to prove it's just an ordinary address until the multisig signs something on its behalf.

**`Transaction`** (a proposal) — PDA at `["transaction", multisig, transaction_index]`
- `multisig`, `transaction_index`, `program_id`
- `accounts: [TxAccountMeta; 10]` (`{ pubkey, is_signer, is_writable }`), `account_count`
- `data: [u8; 300]`, `data_len`
- `approved_mask` (bit *i* = `owners[i]` has approved), `executed`

## How execution works

`execute_transaction` takes every account the proposal references as `remaining_accounts`, validates they match the stored list exactly (same pattern used for `remaining_accounts` validation in `solana-savings-circle` and `solana-quadratic-funding`), rebuilds a `solana_program::instruction::Instruction` from the stored `program_id`/`accounts`/`data`, and calls `invoke_signed` with the vault PDA's seeds. Any account in the proposal matching the vault's derived address is honored as a real signer in the inner CPI — exactly as if the vault had signed it itself.

## Building and testing

Requires `solana-cli`, `anchor-cli`, and Rust already installed. This machine needed `platform-tools` v1.57 to avoid an `edition2024` build error (same issue as the other repos in this org).

```bash
anchor build --no-idl -- --tools-version v1.57
anchor idl build -o target/idl/solana_multisig.json -t target/types/solana_multisig.ts
anchor test --skip-build --no-idl
```

`cargo clippy` (run from `programs/solana-multisig`) is clean.

## CLI client

`cli/` is a Rust CLI (`multisig-cli`, built with `anchor-client` + `clap`) covering every instruction, plus a `show` command for a multisig's (and optionally one proposal's) on-chain state. Defaults to a local validator (`http://127.0.0.1:8899` / `ws://127.0.0.1:8900`) — override with `--url`/`--ws-url` for devnet or mainnet.

`execute-transaction` fetches the proposal on-chain and derives its `remaining_accounts` automatically from the stored account list — you don't re-specify them.

```bash
cargo build -p multisig-cli
BIN=./target/debug/multisig-cli

# local validator + program deploy:
solana-test-validator --reset --quiet &
solana program deploy target/deploy/solana_multisig.so \
  --program-id target/deploy/solana_multisig-keypair.json

$BIN create-multisig --keypair ~/creator.json --multisig-id 1 \
  --owners <OWNER_A>,<OWNER_B>,<OWNER_C> --threshold 2

# the vault is just an address — fund it like any other account:
solana transfer <VAULT_PUBKEY> 1 --from ~/creator.json --allow-unfunded-recipient

# propose an instruction (here, a native SOL transfer out of the vault — instruction
# data is base64; --account entries are PUBKEY:is_signer(0|1):is_writable(0|1), in order):
$BIN create-transaction --keypair ~/ownerA.json --creator <CREATOR_PUBKEY> --multisig-id 1 \
  --program-id 11111111111111111111111111111111 \
  --account <VAULT_PUBKEY>:1:1 --account <RECIPIENT_PUBKEY>:0:1 \
  --data-base64 <BASE64_INSTRUCTION_DATA>

$BIN approve --keypair ~/ownerB.json --creator <CREATOR_PUBKEY> --multisig-id 1 --tx-index 0

$BIN execute-transaction --keypair ~/ownerA.json --creator <CREATOR_PUBKEY> --multisig-id 1 --tx-index 0

$BIN show --creator <CREATOR_PUBKEY> --multisig-id 1 --tx-index 0
```

Run `$BIN --help` or `$BIN <command> --help` for the full flag list.
`cargo clippy` (run from `programs/solana-multisig`) is clean.
