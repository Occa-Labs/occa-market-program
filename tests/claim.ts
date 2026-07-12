// Full claim-flow proof on devnet.
//
// Fresh agent vault each run → mint 1.10 fake USDC into the vault ATA (as if a
// buyer paid price + 10% fee) → claim → assert the provider wallet received
// 1.00 and the fee treasury received 0.10, and the vault counters advanced.
//
//   ANCHOR_PROVIDER_URL=<rpc> ANCHOR_WALLET=~/.config/solana/id.json \
//   npx ts-node tests/claim.ts

import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import {
  getAssociatedTokenAddressSync,
  getAccount,
  mintTo,
} from "@solana/spl-token";
import { ensureConfig, getProgram, usd, vaultPda, wallet } from "./lib";

const PRICE = 1_000_000; // $1.00 listed price
const FEE = 100_000; //   $0.10 fee (10% on top)
const DEPOSIT = PRICE + FEE; // $1.10 the buyer paid into the vault

async function balance(connection: Connection, ata: PublicKey): Promise<number> {
  try {
    const acc = await getAccount(connection, ata);
    return Number(acc.amount);
  } catch {
    return 0; // ATA not created yet
  }
}

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(`ASSERT FAILED: ${msg}`);
  console.log("  ✓", msg);
}

async function main() {
  const program = getProgram();
  const provider = program.provider as anchor.AnchorProvider;
  const authority = wallet(program);
  const payer = (provider.wallet as anchor.Wallet).payer;

  const cfg = await ensureConfig(program);
  const usdcMint = cfg.usdcMint;
  console.log("config:", cfg.config.toBase58(), "| mint:", usdcMint.toBase58(), "| fee_bps:", cfg.feeBps);

  // Fresh agent + a distinct provider wallet so we can read an exact balance.
  const agent = Keypair.generate();
  const providerWallet = Keypair.generate().publicKey;
  const vault = vaultPda(agent.publicKey);
  const vaultAta = getAssociatedTokenAddressSync(usdcMint, vault, true);
  const providerAta = getAssociatedTokenAddressSync(usdcMint, providerWallet);
  const feeAta = getAssociatedTokenAddressSync(usdcMint, cfg.feeTreasury);

  console.log("\nagent:", agent.publicKey.toBase58());
  console.log("vault:", vault.toBase58());

  // 1. init_vault (authority-only) — creates the vault + its ATA.
  await program.methods
    .initVault(agent.publicKey, "test-agent", providerWallet, anchor.web3.PublicKey.default)
    .accountsPartial({ authority, usdcMint })
    .rpc();
  console.log("init_vault ✓");

  // 2. Fund the vault ATA — simulate a buyer's x402 payment of price + fee.
  await mintTo(provider.connection, payer, usdcMint, vaultAta, authority, DEPOSIT);
  const vaultBefore = await balance(provider.connection, vaultAta);
  const providerBefore = await balance(provider.connection, providerAta);
  const feeBefore = await balance(provider.connection, feeAta);
  console.log(`funded vault with $${usd(DEPOSIT)} (vault ATA now $${usd(vaultBefore)})`);

  // 3. claim — permissionless; the authority cranks it here.
  const sig = await program.methods
    .claim(agent.publicKey)
    .accountsPartial({
      vault,
      usdcMint,
      providerWallet,
      feeTreasury: cfg.feeTreasury,
      cranker: authority,
    })
    .rpc();
  console.log("claim tx:", sig);

  // 4. Assert the split.
  const vaultAfter = await balance(provider.connection, vaultAta);
  const providerAfter = await balance(provider.connection, providerAta);
  const feeAfter = await balance(provider.connection, feeAta);
  const v = await program.account.agentVault.fetch(vault);

  console.log("\nresult:");
  console.log(`  vault ATA    : $${usd(vaultBefore)} -> $${usd(vaultAfter)}`);
  console.log(`  provider ATA : $${usd(providerBefore)} -> $${usd(providerAfter)}`);
  console.log(`  fee ATA      : $${usd(feeBefore)} -> $${usd(feeAfter)}`);
  console.log(`  counters     : provider=$${usd(v.claimedProvider.toNumber())} fee=$${usd(v.claimedFee.toNumber())}`);

  console.log("\nassertions:");
  assert(providerAfter - providerBefore === PRICE, `provider received exactly $${usd(PRICE)} (the listed price)`);
  assert(feeAfter - feeBefore === FEE, `fee treasury received exactly $${usd(FEE)} (the 10% fee)`);
  assert(vaultAfter === 0, "vault ATA fully drained");
  assert((providerAfter - providerBefore) + (feeAfter - feeBefore) === DEPOSIT, "split sums to the deposit exactly");
  assert(v.claimedProvider.toNumber() === PRICE && v.claimedFee.toNumber() === FEE, "vault counters match the payout");

  console.log("\nALL ASSERTIONS PASSED ✓");
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
