import assert from "node:assert/strict";
import { JsonRpcProvider } from "ethers";

const rpcUrl = process.env.CASPER_RPC_URL ?? "http://127.0.0.1:7777/rpc";
const transactionHash = process.env.CASPER_TX_HASH;
assert.ok(
  transactionHash,
  "CASPER_TX_HASH must identify a block-included EVM transaction",
);

const provider = new JsonRpcProvider(rpcUrl);

try {
  const transaction = await provider.getTransaction(transactionHash);
  assert.ok(transaction, "ethers should parse the transaction lookup response");
  assert.equal(transaction.hash.toLowerCase(), transactionHash.toLowerCase());
  assert.notEqual(
    transaction.blockNumber,
    null,
    "the smoke fixture must be included in a block",
  );

  const [rawTransaction, rawBlock] = await Promise.all([
    provider.send("eth_getTransactionByHash", [transactionHash]),
    provider.send("eth_getBlockByNumber", [
      `0x${transaction.blockNumber.toString(16)}`,
      true,
    ]),
  ]);
  const hydrated = rawBlock.transactions.find(
    (candidate) => candidate.hash.toLowerCase() === transactionHash.toLowerCase(),
  );

  assert.ok(hydrated, "the hydrated block should contain the transaction");
  assert.deepEqual(
    hydrated,
    rawTransaction,
    "block hydration and individual lookup must return identical transaction objects",
  );
  console.log(`ethers v6 transaction lookup succeeded against ${rpcUrl}`);
} finally {
  provider.destroy();
}
