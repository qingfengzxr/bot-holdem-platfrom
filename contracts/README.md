# Batch Settlement Contract

## Prerequisites

1. Install Foundry (`forge`, `cast`, `anvil`).
2. Install `forge-std` once:

```bash
cd contracts
forge install foundry-rs/forge-std
```

## Build

```bash
cd contracts
forge build
```

## Deploy

Use the root helper script:

```bash
DEPLOYER_PRIVATE_KEY=0x... \
RPC_URL=http://127.0.0.1:8545 \
./scripts/deploy_batch_settlement.sh
```

The script prints `BATCH_SETTLEMENT_CONTRACT=<address>` for app config.

