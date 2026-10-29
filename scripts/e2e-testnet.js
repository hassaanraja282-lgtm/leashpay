#!/usr/bin/env node
// Live end-to-end test against the deployed testnet vault, using the same keys as the demo (.env).
// Spends at most ~0.02 testnet NEAR per run (recipient accounts are our own demo accounts).
import assert from 'node:assert/strict';
import { config } from '../server/config.js';
import { readState, agentRequestPayment, agentAttack, ownerReject, ownerPause, ownerUnpause, nearToYocto } from '../server/near.js';

const results = [];
async function step(name, fn) {
  try { await fn(); results.push(['PASS', name]); console.log('PASS', name); } catch (e) { results.push(['FAIL', name]); console.log('FAIL', name, '-', e.message); }
}

const s0 = await readState();
const allowed = s0.recipients[0]?.[0];
assert.ok(allowed, 'vault has no allow-listed recipient');
const th = s0.config.policy.approval_threshold;
console.log(`contract=${config.contractId} agent=${s0.config.agent} recipient=${allowed}`);

await step('small payment to allow-listed recipient is PAID', async () => {
  const { result } = await agentRequestPayment({ recipient: allowed, amountYocto: nearToYocto('0.01'), memo: 'e2e small payment' });
  assert.equal(result.status, 'paid');
});
await step('unknown recipient is BLOCKED with reason', async () => {
  const { result } = await agentRequestPayment({ recipient: 'unknown.testnet', amountYocto: nearToYocto('0.01'), memo: 'e2e unknown' });
  assert.equal(result.status, 'blocked'); assert.match(result.reason, /allow-list/);
});
await step('over per-tx cap is BLOCKED', async () => {
  const { result } = await agentRequestPayment({ recipient: allowed, amountYocto: (BigInt(s0.config.policy.per_tx_cap) + 1n).toString(), memo: 'e2e over cap' });
  assert.equal(result.status, 'blocked'); assert.match(result.reason, /per-transaction cap/);
});
if (th) {
  await step('above threshold is QUEUED, owner REJECTS it', async () => {
    const { result } = await agentRequestPayment({ recipient: allowed, amountYocto: (BigInt(th) + 1n).toString(), memo: 'e2e queued' });
    assert.equal(result.status, 'queued');
    await ownerReject(result.pending_id, 'e2e cleanup');
    const s = await readState();
    assert.ok(!s.pending.find((p) => p.id === result.pending_id));
  });
}
await step('kill switch: paused vault BLOCKS, unpause restores', async () => {
  await ownerPause();
  const { result } = await agentRequestPayment({ recipient: allowed, amountYocto: nearToYocto('0.01'), memo: 'e2e while paused' });
  await ownerUnpause();
  assert.equal(result.status, 'blocked'); assert.match(result.reason, /paused/);
});
for (const k of ['withdraw', 'policy', 'transfer']) {
  await step(`agent key cannot bypass the contract (${k})`, async () => {
    const r = await agentAttack(k);
    assert.equal(r.blocked, true, r.message);
  });
}
await step('audit log recorded the run', async () => {
  const s = await readState();
  assert.ok(s.config.log_count >= s0.config.log_count + 5);
});

const failed = results.filter((r) => r[0] === 'FAIL').length;
console.log(`\n${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
