import assert from "node:assert/strict";
import { JsonRpcProvider } from "ethers";

const rpcUrl = process.env.CASPER_RPC_URL ?? "http://127.0.0.1:7777/rpc";
const address = process.env.CASPER_CONTRACT_ADDRESS;
const slot = process.env.CASPER_STORAGE_SLOT ?? "0x0";
const expected = process.env.CASPER_EXPECTED_STORAGE;

assert.ok(address, "CASPER_CONTRACT_ADDRESS must identify a deployed contract");
assert.ok(expected, "CASPER_EXPECTED_STORAGE must be the expected 32-byte storage word");
assert.match(expected, /^0x[0-9a-f]{64}$/i, "expected storage must be exactly 32 bytes");

const provider = new JsonRpcProvider(rpcUrl);

try {
  const block = await provider.getBlock("latest");
  assert.ok(block, "latest block should exist");
  assert.ok(block.hash, "latest complete block should have a hash");

  const blockNumber = `0x${block.number.toString(16)}`;
  const selectors = [
    "latest",
    blockNumber,
    block.hash,
    { blockNumber },
    { blockHash: block.hash },
    { blockHash: block.hash, requireCanonical: true },
  ];
  const values = await Promise.all(
    selectors.map((selector) =>
      provider.send("eth_getStorageAt", [address, slot, selector]),
    ),
  );

  for (const value of values) {
    assert.match(value, /^0x[0-9a-f]{64}$/i, "storage result must be exactly 32 bytes");
    assert.equal(value.toLowerCase(), expected.toLowerCase());
  }

  console.log(`ethers v6 storage and EIP-1898 selectors succeeded against ${rpcUrl}`);
} finally {
  provider.destroy();
}
