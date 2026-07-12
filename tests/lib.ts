// Shared devnet test harness for the settlement program.
//
// Reads the provider from ANCHOR_PROVIDER_URL + ANCHOR_WALLET (so the RPC key
// is never committed). The fake USDC mint used across runs is persisted under
// target/ (gitignored) so `bootstrap` can pin it in config once and `claim`
// can reuse it — the deploy wallet is the mint authority, so we can always
// fund a vault.

import * as fs from "fs";
import * as path from "path";
import * as anchor from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { createMint } from "@solana/spl-token";

import idl from "../target/idl/settlement.json";
import type { Settlement } from "../target/types/settlement";

export const PROGRAM_ID = new PublicKey((idl as anchor.Idl).address);
export const USDC_DECIMALS = 6;
export const FEE_BPS = 1000; // 10%, the market fee

const ARTIFACT_DIR = path.join(__dirname, "..", "target", "test-artifacts");
const MINT_FILE = path.join(ARTIFACT_DIR, "fake-usdc.json");

export function getProgram(): anchor.Program<Settlement> {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  return new anchor.Program(idl as Settlement, provider);
}

export function wallet(program: anchor.Program<Settlement>): PublicKey {
  return (program.provider as anchor.AnchorProvider).wallet.publicKey;
}

export function configPda(): PublicKey {
  return PublicKey.findProgramAddressSync([Buffer.from("config")], PROGRAM_ID)[0];
}

export function vaultPda(agentPubkey: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("vault"), agentPubkey.toBuffer()],
    PROGRAM_ID,
  )[0];
}

/** Load the persisted fake-USDC mint keypair, or mint a fresh one and save it. */
export async function ensureFakeUsdc(program: anchor.Program<Settlement>): Promise<PublicKey> {
  if (fs.existsSync(MINT_FILE)) {
    const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(MINT_FILE, "utf8"))));
    return kp.publicKey;
  }
  const provider = program.provider as anchor.AnchorProvider;
  const authority = provider.wallet.publicKey;
  const mintKp = Keypair.generate();
  const mint = await createMint(
    provider.connection,
    (provider.wallet as anchor.Wallet).payer,
    authority,
    null,
    USDC_DECIMALS,
    mintKp,
  );
  fs.mkdirSync(ARTIFACT_DIR, { recursive: true });
  fs.writeFileSync(MINT_FILE, JSON.stringify(Array.from(mintKp.secretKey)));
  return mint;
}

/** Ensure the singleton config exists (fee treasury = the authority wallet). */
export async function ensureConfig(program: anchor.Program<Settlement>) {
  const config = configPda();
  let existing = await program.account.marketConfig.fetchNullable(config);
  if (!existing) {
    const authority = wallet(program);
    const usdcMint = await ensureFakeUsdc(program);
    await program.methods
      .initConfig(authority, FEE_BPS)
      .accountsPartial({ authority, usdcMint })
      .rpc();
    existing = await program.account.marketConfig.fetch(config);
  }
  return { config, ...existing };
}

export const usd = (micros: number) => (micros / 1_000_000).toFixed(6);
