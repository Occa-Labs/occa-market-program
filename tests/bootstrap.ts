// Bootstrap the singleton MarketConfig on devnet (idempotent).
//
// Pins a fake USDC mint (deploy wallet is its authority, so vaults can be
// funded for tests) and sets the fee treasury + 10% fee. Safe to re-run — if
// config already exists it just prints it.
//
//   ANCHOR_PROVIDER_URL=<rpc> ANCHOR_WALLET=~/.config/solana/id.json \
//   npx ts-node tests/bootstrap.ts

import { configPda, ensureFakeUsdc, FEE_BPS, getProgram, wallet } from "./lib";

async function main() {
  const program = getProgram();
  const authority = wallet(program);
  const config = configPda();

  const existing = await program.account.marketConfig.fetchNullable(config);
  if (existing) {
    console.log("config already exists:");
    console.log("  authority   :", existing.authority.toBase58());
    console.log("  usdc_mint   :", existing.usdcMint.toBase58());
    console.log("  fee_treasury:", existing.feeTreasury.toBase58());
    console.log("  fee_bps     :", existing.feeBps);
    return;
  }

  const usdcMint = await ensureFakeUsdc(program);
  // Fee treasury is just a wallet whose ATA receives fees. Use the authority
  // for the test so we can read both sides with one keypair.
  const feeTreasury = authority;

  const sig = await program.methods
    .initConfig(feeTreasury, FEE_BPS)
    .accountsPartial({ authority, usdcMint })
    .rpc();

  console.log("init_config tx:", sig);
  const c = await program.account.marketConfig.fetch(config);
  console.log("config created:");
  console.log("  config PDA  :", config.toBase58());
  console.log("  usdc_mint   :", c.usdcMint.toBase58());
  console.log("  fee_treasury:", c.feeTreasury.toBase58());
  console.log("  fee_bps     :", c.feeBps);
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
