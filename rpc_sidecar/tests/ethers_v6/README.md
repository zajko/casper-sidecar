# ethers v6 default batching smoke test

Run a sidecar with Ethereum RPCs enabled, then execute:

```sh
npm install
CASPER_RPC_URL=http://127.0.0.1:7777/rpc npm test
```

The test intentionally constructs `JsonRpcProvider` without `batchMaxCount: 1` and issues three
requests in one event-loop turn. It fails if the sidecar cannot process ethers v6's default batch
envelope.

To smoke-test a transaction lookup and hydrated block, provide the hash of an
EVM transaction already included in a block:

```sh
CASPER_RPC_URL=http://127.0.0.1:7777/rpc \
CASPER_TX_HASH=0x... \
npm run test:transaction
```

This checks that `provider.getTransaction(hash)` parses successfully and that
the raw transaction object is identical to the corresponding object returned
by `eth_getBlockByNumber(..., true)`.

To smoke-test storage reads and every supported EIP-1898 selector form, provide
an address with a known, non-zero storage slot:

```sh
CASPER_RPC_URL=http://127.0.0.1:7777/rpc \
CASPER_CONTRACT_ADDRESS=0x... \
CASPER_STORAGE_SLOT=0x0 \
CASPER_EXPECTED_STORAGE=0x0000000000000000000000000000000000000000000000000000000000000001 \
npm run test:storage
```

The test reads the latest complete block first, then queries the same storage
slot by tag, number, raw block hash, and both EIP-1898 object forms. It also
sets `requireCanonical: true` for the hash-selected canonical block.
