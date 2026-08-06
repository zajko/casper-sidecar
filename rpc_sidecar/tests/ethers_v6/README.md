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
