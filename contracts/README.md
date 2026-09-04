# Migo's on-chain contracts

The store's payment legs live here: **MGO**, the ERC-20 the prices are quoted in, and
the **Treasury**, the address every AVAX/USDT/BTC.b payment lands in. Foundry layout,
OpenZeppelin v5 underneath, [Solidity style guide] throughout.

```
contracts/
├── foundry.toml            # solc 0.8.24, optimizer, fmt profile
├── src/
│   ├── MgoToken.sol        # upgradeable ERC-20 + permit + burn, AccessManager-governed
│   └── Treasury.sol        # receives AVAX/ERC-20, logs PaymentReceived, owner-sweeps
├── script/
│   └── StorePayments.s.sol # Fuji deploy: AccessManager → MGO proxy → Treasury
├── test/
│   ├── MgoToken.t.sol      # the shapes the store drives + the ones it must refuse
│   └── mocks/ERC20Mock.sol
└── lib/                    # git submodules: openzeppelin-contracts v5.6.1, forge-std
```

## The payment model (what is on-chain and what is not)

The buyer pays the **Treasury** directly — native AVAX as a plain value transfer, or
USDT/BTC.b as an ERC-20 transfer with the treasury as recipient (BTC.b carries its own
8 decimals; the client converts the MGO price through a live USD pair and sends the
converted amount). The store's client (`clients/store/src/lib/chain-purchase.ts`) builds,
signs, and broadcasts that transaction from the account's own wallet-0 key; the Migo
server is never a blockchain proxy and never moves a payment.

The MGO amount is what the payment is _quoted_ in (1 coin = 1 MGO at the client's
constant): after the chain confirms, the client tells the server
`economy.purchase(sku, key, txHash)` and the _server_ writes the entitlement — the
chain records the money, the server records what it bought. The treasury does no
conversion and holds no price table; keeping that logic off-chain keeps the contract's
on-chain promise to receive, record, and sweep.

## Deploying to Fuji

```sh
cd contracts
forge build && forge test          # both must pass first

export MIGO_DEPLOYER_PRIVATE_KEY=0x…   # the deploy wallet (never committed, never logged)
export MIGO_ADMIN=0x…                  # optional: the operator wallet; defaults to the deployer

forge script script/StorePayments.s.sol \
  --rpc-url https://api.avax-test.network/ext/bc/C/rpc \
  --broadcast --verify
```

The run writes `deployment.json` — `accessManager`, `mgoToken`, `treasury` — which is
exactly the list the store's constants need:

```ts
// clients/store/src/lib/chain-purchase.ts
export const MGO_TOKEN_FUJI = '0x…'; // deployment.json's mgoToken
export const MGO_TREASURY_FUJI = '0x…'; // deployment.json's treasury
```

USDT and BTC.b on Fuji are third-party contracts (testnet tokens are redeployed at their
owners' whim); set their current addresses the same way. With all four constants real,
the store's currency chips enable and every payment quotes the exact fields the
signature covers.

CI (`contracts.yml`) runs `forge build`, `forge test`, and `forge fmt --check` on every
push — installing Foundry itself, so no local toolchain is needed to contribute.

[Solidity style guide]: https://docs.soliditylang.org/en/latest/style-guide.html
