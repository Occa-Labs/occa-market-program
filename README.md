# occa-market-programs

On-chain programs for the **OCCA Open Market** — separate from
[`occa-programs`](https://github.com/Occa-Labs/occa-programs) (which holds the
OS/company registry + treasury), because the market's money programs have their
own lifecycle and security bar.

## Programs

| Program | ID (devnet) | Purpose |
|---|---|---|
| `settlement` | `occaFcDiKh65LtKoNd7TpDn14YaioRFvVR7wHibdMQo` | Non-custodial per-agent USDC vaults for the x402 machine rail. |

### settlement

The cash register of the Open Market. Each agent gets a program-owned USDC
vault whose associated token account is the x402 `payTo`. Buyers pay in
directly; the program — not OCCA — splits every claim into the provider's take
(the listed price, in full) and the protocol fee (on top). There is no
instruction that moves the provider's share to anyone but the provider's own
wallet.

Accounts:
- `MarketConfig` (singleton) — authority, pinned USDC mint, fee treasury, fee bps.
- `AgentVault` (per agent, seed = the agent's registry `agent_pubkey`) — provider
  payout wallet, fee-rate snapshot, lifetime claim counters. Owns the vault ATA.

Instructions: `init_config`, `set_config`, `init_vault`, `set_provider_wallet`,
`claim` (permissionless).

## Build

```
anchor build -p settlement
```

Produces `target/deploy/settlement.so` + `target/idl/settlement.json`.

## Deploy (devnet)

Public devnet RPC rate-limits program deploys and can leave corrupt buffers;
deploy through a paid RPC with `--use-rpc`. Always verify the on-chain bytecode
hash against the local `.so` after deploy.

## Truth model

The chain is authoritative. The market server's off-chain records are a
rebuildable index over vault deposits and claims, never the source of truth for
balances.
