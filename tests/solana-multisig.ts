import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import { assert } from "chai";
import { SolanaMultisig } from "../target/types/solana_multisig";

describe("solana-multisig", () => {
  anchor.setProvider(anchor.AnchorProvider.env());
  const provider = anchor.getProvider() as anchor.AnchorProvider;
  const program = anchor.workspace.solanaMultisig as Program<SolanaMultisig>;

  const creator = Keypair.generate();
  const ownerA = Keypair.generate();
  const ownerB = Keypair.generate();
  const ownerC = Keypair.generate();
  const outsider = Keypair.generate();
  const recipient = Keypair.generate();

  const MULTISIG_ID = new BN(1);
  const THRESHOLD = 2;
  const TRANSFER_AMOUNT = 0.3 * LAMPORTS_PER_SOL;

  let multisigPda: PublicKey;
  let vaultPda: PublicKey;

  const findMultisigPda = () =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("multisig"), creator.publicKey.toBuffer(), MULTISIG_ID.toArrayLike(Buffer, "le", 8)],
      program.programId
    )[0];

  const findVaultPda = (multisig: PublicKey) =>
    PublicKey.findProgramAddressSync([Buffer.from("vault"), multisig.toBuffer()], program.programId)[0];

  const findTransactionPda = (multisig: PublicKey, txIndex: BN) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("transaction"), multisig.toBuffer(), txIndex.toArrayLike(Buffer, "le", 8)],
      program.programId
    )[0];

  before(async () => {
    for (const kp of [creator, ownerA, ownerB, ownerC, outsider]) {
      const sig = await provider.connection.requestAirdrop(kp.publicKey, 2 * LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction(sig, "confirmed");
    }

    multisigPda = findMultisigPda();
    vaultPda = findVaultPda(multisigPda);
  });

  it("creates a 2-of-3 multisig", async () => {
    await program.methods
      .createMultisig(MULTISIG_ID, [ownerA.publicKey, ownerB.publicKey, ownerC.publicKey], THRESHOLD)
      .accounts({
        creator: creator.publicKey,
        multisig: multisigPda,
        vault: vaultPda,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const multisig = await program.account.multisig.fetch(multisigPda);
    assert.equal(multisig.ownerCount, 3);
    assert.equal(multisig.threshold, THRESHOLD);
    assert.equal(multisig.transactionCount.toNumber(), 0);
  });

  it("funds the vault with a plain SOL transfer (it's just an ordinary address)", async () => {
    const sig = await provider.connection.requestAirdrop(vaultPda, 2 * LAMPORTS_PER_SOL);
    await provider.connection.confirmTransaction(sig, "confirmed");
    const balance = await provider.connection.getBalance(vaultPda);
    assert.isAbove(balance, TRANSFER_AMOUNT);
  });

  it("rejects a non-owner from proposing a transaction", async () => {
    const transferIx = SystemProgram.transfer({
      fromPubkey: vaultPda,
      toPubkey: recipient.publicKey,
      lamports: TRANSFER_AMOUNT,
    });
    const txIndex = new BN(0);
    const transactionPda = findTransactionPda(multisigPda, txIndex);

    try {
      await program.methods
        .createTransaction(
          transferIx.programId,
          transferIx.keys.map((k) => ({ pubkey: k.pubkey, isSigner: k.isSigner, isWritable: k.isWritable })),
          transferIx.data
        )
        .accounts({
          proposer: outsider.publicKey,
          multisig: multisigPda,
          transaction: transactionPda,
          systemProgram: SystemProgram.programId,
        })
        .signers([outsider])
        .rpc();
      assert.fail("expected create_transaction to fail for a non-owner");
    } catch (err) {
      assert.include(String(err), "NotAnOwner");
    }
  });

  it("rejects a proposal with too many accounts", async () => {
    const tooManyAccounts = Array.from({ length: 11 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isSigner: false,
      isWritable: false,
    }));
    const txIndex = new BN(0);
    const transactionPda = findTransactionPda(multisigPda, txIndex);

    try {
      await program.methods
        .createTransaction(SystemProgram.programId, tooManyAccounts, Buffer.from([]))
        .accounts({
          proposer: ownerA.publicKey,
          multisig: multisigPda,
          transaction: transactionPda,
          systemProgram: SystemProgram.programId,
        })
        .signers([ownerA])
        .rpc();
      assert.fail("expected create_transaction to fail with too many accounts");
    } catch (err) {
      assert.include(String(err), "TooManyAccounts");
    }
  });

  it("proposes, approves, and executes a transfer out of the vault", async () => {
    const transferIx = SystemProgram.transfer({
      fromPubkey: vaultPda,
      toPubkey: recipient.publicKey,
      lamports: TRANSFER_AMOUNT,
    });
    const txIndex = new BN(0);
    const transactionPda = findTransactionPda(multisigPda, txIndex);

    await program.methods
      .createTransaction(
        transferIx.programId,
        transferIx.keys.map((k) => ({ pubkey: k.pubkey, isSigner: k.isSigner, isWritable: k.isWritable })),
        transferIx.data
      )
      .accounts({
        proposer: ownerA.publicKey,
        multisig: multisigPda,
        transaction: transactionPda,
        systemProgram: SystemProgram.programId,
      })
      .signers([ownerA])
      .rpc();

    let tx = await program.account.transaction.fetch(transactionPda);
    assert.equal(tx.approvedMask, 0b001); // ownerA (index 0) auto-approved

    // executing now should fail: only 1 of 2 required approvals
    try {
      await program.methods
        .executeTransaction()
        .accounts({
          multisig: multisigPda,
          transaction: transactionPda,
          targetProgram: SystemProgram.programId,
        })
        .remainingAccounts([
          { pubkey: vaultPda, isWritable: true, isSigner: false },
          { pubkey: recipient.publicKey, isWritable: true, isSigner: false },
        ])
        .rpc();
      assert.fail("expected execute_transaction to fail before threshold is met");
    } catch (err) {
      assert.include(String(err), "NotEnoughApprovals");
    }

    // ownerB approves (index 1)
    await program.methods
      .approve()
      .accounts({
        owner: ownerB.publicKey,
        multisig: multisigPda,
        transaction: transactionPda,
      })
      .signers([ownerB])
      .rpc();

    tx = await program.account.transaction.fetch(transactionPda);
    assert.equal(tx.approvedMask, 0b011);

    // a non-owner can't approve
    try {
      await program.methods
        .approve()
        .accounts({
          owner: outsider.publicKey,
          multisig: multisigPda,
          transaction: transactionPda,
        })
        .signers([outsider])
        .rpc();
      assert.fail("expected approve to fail for a non-owner");
    } catch (err) {
      assert.include(String(err), "NotAnOwner");
    }

    // double-approving fails
    try {
      await program.methods
        .approve()
        .accounts({
          owner: ownerB.publicKey,
          multisig: multisigPda,
          transaction: transactionPda,
        })
        .signers([ownerB])
        .rpc();
      assert.fail("expected the second approval from the same owner to fail");
    } catch (err) {
      assert.include(String(err), "AlreadyApproved");
    }

    const recipientBalanceBefore = await provider.connection.getBalance(recipient.publicKey);

    await program.methods
      .executeTransaction()
      .accounts({
        multisig: multisigPda,
        transaction: transactionPda,
        targetProgram: SystemProgram.programId,
      })
      .remainingAccounts([
        { pubkey: vaultPda, isWritable: true, isSigner: false },
        { pubkey: recipient.publicKey, isWritable: true, isSigner: false },
      ])
      .rpc();

    const recipientBalanceAfter = await provider.connection.getBalance(recipient.publicKey);
    assert.equal(recipientBalanceAfter - recipientBalanceBefore, TRANSFER_AMOUNT);

    tx = await program.account.transaction.fetch(transactionPda);
    assert.isTrue(tx.executed);
  });

  it("rejects a double-execute", async () => {
    const txIndex = new BN(0);
    const transactionPda = findTransactionPda(multisigPda, txIndex);

    try {
      await program.methods
        .executeTransaction()
        .accounts({
          multisig: multisigPda,
          transaction: transactionPda,
          targetProgram: SystemProgram.programId,
        })
        .remainingAccounts([
          { pubkey: vaultPda, isWritable: true, isSigner: false },
          { pubkey: recipient.publicKey, isWritable: true, isSigner: false },
        ])
        .rpc();
      assert.fail("expected the second execute to fail");
    } catch (err) {
      assert.include(String(err), "AlreadyExecuted");
    }
  });
});
