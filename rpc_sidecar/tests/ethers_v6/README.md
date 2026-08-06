# ethers v6 default batching smoke test

Run a sidecar with Ethereum RPCs enabled, then execute:

```sh
npm install
CASPER_RPC_URL=http://127.0.0.1:7777/rpc npm test
```

The test intentionally constructs `JsonRpcProvider` without `batchMaxCount: 1` and issues three
requests in one event-loop turn. It fails if the sidecar cannot process ethers v6's default batch
envelope.
