// Claim an EXISTING agent vault (by its agent pubkey) and show the split.
// Used to close the x402→vault→claim loop against a vault funded by real
// x402 payments.
//
//   AGENT_PUBKEY=<pubkey> ANCHOR_PROVIDER_URL=<rpc> ANCHOR_WALLET=<id.json> \
//   npx ts-node tests/claim-agent.ts

import * as anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, getAccount } from "@solana/spl-token";
import { configPda, getProgram, usd, vaultPda, wallet } from "./lib";

async function bal(connection: anchor.web3.Connection, ata: PublicKey): Promise<number> {
  try {
    return Number((await getAccount(connection, ata)).amount);
  } catch {
    return 0;
  }
}

async function main() {
  const program = getProgram();
  const conn = (program.provider as anchor.AnchorProvider).connection;
  const authority = wallet(program);
  const agent = new PublicKey(process.env.AGENT_PUBKEY!);

  const config = await program.account.marketConfig.fetch(configPda());
  const vault = vaultPda(agent);
  const v = await program.account.agentVault.fetch(vault);
  const usdcMint = config.usdcMint;

  const vaultAta = getAssociatedTokenAddressSync(usdcMint, vault, true);
  const providerAta = getAssociatedTokenAddressSync(usdcMint, v.providerWallet);
  const feeAta = getAssociatedTokenAddressSync(usdcMint, config.feeTreasury);

  const [vaultBefore, provBefore, feeBefore] = await Promise.all([
    bal(conn, vaultAta), bal(conn, providerAta), bal(conn, feeAta),
  ]);
  console.log("vault      :", vault.toBase58());
  console.log("provider   :", v.providerWallet.toBase58());
  console.log("fee treasury:", config.feeTreasury.toBase58());
  console.log(`vault ATA before: $${usd(vaultBefore)}`);

  const sig = await program.methods
    .claim(agent)
    .accountsPartial({
      vault,
      usdcMint,
      providerWallet: v.providerWallet,
      feeTreasury: config.feeTreasury,
      cranker: authority,
    })
    .rpc();
  console.log("claim tx:", sig);

  const [vaultAfter, provAfter, feeAfter] = await Promise.all([
    bal(conn, vaultAta), bal(conn, providerAta), bal(conn, feeAta),
  ]);
  console.log("\nsplit:");
  console.log(`  vault ATA   : $${usd(vaultBefore)} -> $${usd(vaultAfter)}`);
  console.log(`  provider    : +$${usd(provAfter - provBefore)}`);
  console.log(`  fee treasury: +$${usd(feeAfter - feeBefore)}`);
}

main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
