import assert from "node:assert/strict";
import { JsonRpcProvider } from "ethers";

const rpcUrl = process.env.CASPER_RPC_URL ?? "http://127.0.0.1:7777/rpc";
const provider = new JsonRpcProvider(rpcUrl);

try {
  // JsonRpcProvider batches requests made in the same event-loop turn by default. Deliberately do
  // not set batchMaxCount: 1: this is a black-box regression test for sidecar batch envelopes.
  const [clientVersion, chainId, blockNumber, netVersion] = await Promise.all([
    provider.send("web3_clientVersion", []),
    provider.send("eth_chainId", []),
    provider.send("eth_blockNumber", []),
    provider.send("net_version", []),
  ]);

  assert.match(clientVersion, /^CasperSidecar\/v/);
  assert.match(chainId, /^0x[0-9a-f]+$/i);
  assert.match(blockNumber, /^0x[0-9a-f]+$/i);
  assert.match(netVersion, /^\d+$/);
  console.log(`ethers v6 default batching succeeded against ${rpcUrl}`);
} finally {
  provider.destroy();
}
