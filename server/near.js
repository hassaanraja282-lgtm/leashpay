// Thin NEAR layer: contract views, the agent's function-call-key signer, and owner actions.
import fs from 'node:fs';
import path from 'node:path';
import { JsonRpcProvider, Account } from 'near-api-js';
import { config } from './config.js';

export const YOCTO = 10n ** 24n;
const GAS = 30_000_000_000_000n; // 30 Tgas
const ONE_YOCTO = 1n;

export const provider = new JsonRpcProvider({ url: config.rpcUrl });
const agent = config.agentId && config.agentKey ? new Account(config.agentId, provider, config.agentKey) : null;
const owner = config.ownerId && config.ownerKey ? new Account(config.ownerId, provider, config.ownerKey) : null;

export const capabilities = () => ({
  agent: !!agent,
  owner: !!owner && config.ownerActions,
});

/** "0.5" NEAR -> yocto string (exact, no floats). */
export function nearToYocto(near) {
  const s = String(near).trim();
  if (!/^\d+(\.\d+)?$/.test(s)) throw new Error(`invalid NEAR amount: ${near}`);
  const [w, f = ''] = s.split('.');
  return (BigInt(w) * YOCTO + BigInt((f + '0'.repeat(24)).slice(0, 24))).toString();
}

/** yocto string -> "0.5" */
export function yoctoToNear(y, dp = 4) {
  const v = BigInt(y);
  const w = v / YOCTO;
  const f = (v % YOCTO).toString().padStart(24, '0').slice(0, dp).replace(/0+$/, '');
  return f ? `${w}.${f}` : `${w}`;
}

// --- tx hash memory: audit log ids -> tx hashes this server signed (for explorer links)
const txFile = path.join(config.dataDir, 'txmap.json');
let txmap = {};
try { txmap = JSON.parse(fs.readFileSync(txFile, 'utf8')); } catch { /* fresh */ }
function rememberTx(logId, hash) {
  if (logId === undefined || logId === null || !hash) return;
  txmap[`${config.contractId}:${logId}`] = hash;
  try { fs.mkdirSync(config.dataDir, { recursive: true }); fs.writeFileSync(txFile, JSON.stringify(txmap)); } catch { /* best effort */ }
}
export const txFor = (logId) => txmap[`${config.contractId}:${logId}`];

const view = (method, args = {}) => provider.callFunction({ contractId: config.contractId, method, args });

let cache = { at: 0, data: null };
export function invalidateState() { cache.at = 0; }
export async function readState({ fresh = false } = {}) {
  if (!fresh && cache.data && Date.now() - cache.at < 2500) return cache.data;
  let lastErr;
  for (let i = 0; i < 3; i++) {
    try { cache = { at: Date.now(), data: await readStateOnce() }; return cache.data; } catch (e) {
      lastErr = e; await new Promise((r) => setTimeout(r, 400 * (i + 1)));
    }
  }
  if (cache.data) return cache.data; // serve last good state if the RPC hiccups
  throw lastErr;
}

async function readStateOnce() {
  const [cfg, balance, allowance, recipients, pending, log] = await Promise.all([
    view('get_config'), view('get_balance'), view('get_allowance'), view('get_recipients'), view('get_pending'),
    view('get_log', { limit: 40 }),
  ]);
  return { config: cfg, balance, allowance, recipients, pending, log };
}

function decode(res) {
  const sv = res?.status?.SuccessValue;
  if (sv === undefined) return undefined;
  if (sv === '') return null;
  try { return JSON.parse(Buffer.from(sv, 'base64').toString('utf8')); } catch { return null; }
}

function failure(res) {
  const f = res?.status?.Failure;
  if (!f) return null;
  return JSON.stringify(f);
}

async function call(account, method, args, deposit = 0n) {
  const res = await account.callFunctionRaw({
    contractId: config.contractId, methodName: method, args, gas: GAS, deposit, waitUntil: 'EXECUTED_OPTIMISTIC',
  });
  const hash = res?.transaction?.hash || res?.transaction_outcome?.id;
  invalidateState();
  const err = failure(res);
  if (err) { const e = new Error(err); e.txHash = hash; throw e; }
  return { hash, value: decode(res) };
}

/** The ONLY thing the agent can do: ask the vault to pay. Signed with the function-call key. */
export async function agentRequestPayment({ recipient, amountYocto, memo }) {
  if (!agent) throw new Error('Agent key not configured (AGENT_ACCOUNT_ID / AGENT_PRIVATE_KEY).');
  const { hash, value } = await call(agent, 'request_payment', { recipient, amount: amountYocto, memo });
  rememberTx(value?.log_id, hash);
  return { txHash: hash, result: value };
}

/**
 * Bypass attempts with the agent's key, to prove the key itself is leashed.
 * These are expected to FAIL at the protocol level (before the contract even runs).
 */
export async function agentAttack(kind) {
  if (!agent) throw new Error('Agent key not configured.');
  const attempts = {
    withdraw: () => agent.callFunctionRaw({ contractId: config.contractId, methodName: 'withdraw', args: { amount: nearToYocto('3') }, gas: GAS, deposit: ONE_YOCTO }),
    policy: () => agent.callFunctionRaw({ contractId: config.contractId, methodName: 'set_policy', args: { per_tx_cap: nearToYocto('100'), daily_cap: nearToYocto('100'), approval_threshold: null }, gas: GAS, deposit: 0n }),
    transfer: () => agent.transfer({ receiverId: 'unknown-attacker.testnet', amount: nearToYocto('0.01') }),
  };
  const fn = attempts[kind];
  if (!fn) throw new Error('unknown attack');
  try {
    const res = await fn();
    const f = failure(res);
    return { blocked: !!f, message: f || 'unexpectedly succeeded', txHash: res?.transaction?.hash };
  } catch (e) {
    return { blocked: true, message: shortError(e) };
  }
}

export function shortError(e) {
  const s = String(e?.message || e);
  const m = s.match(/(MethodNameMismatch|DepositWithFunctionCall|InvalidAccessKeyError|ReceiverMismatch|RequiresFullAccess|NotEnoughAllowance)[^"}]*/);
  if (m) return m[0];
  const p = s.match(/Smart contract panicked: ([^"\\]+)/);
  if (p) return p[1];
  return s.slice(0, 300);
}

function needOwner() {
  if (!owner || !config.ownerActions) throw new Error('Owner actions are disabled on this deployment.');
  return owner;
}

async function ownerCall(method, args) {
  const r = await call(needOwner(), method, args, ONE_YOCTO);
  if (typeof r.value === 'number') rememberTx(r.value, r.hash);
  else {
    // methods that don't return a log id: the newest entry is ours
    try { const [last] = await view('get_log', { limit: 1 }); if (last) rememberTx(last.id, r.hash); } catch { /* ignore */ }
  }
  return r;
}

export const ownerApprove = (id) => ownerCall('approve', { pending_id: Number(id) });
export const ownerReject = (id, reason) => ownerCall('reject', { pending_id: Number(id), reason: reason || 'rejected by owner' });
export const ownerPause = () => ownerCall('pause', {});
export const ownerUnpause = () => ownerCall('unpause', {});
export const ownerSetPolicy = ({ perTx, daily, threshold }) => ownerCall('set_policy', {
  per_tx_cap: nearToYocto(perTx), daily_cap: nearToYocto(daily), approval_threshold: threshold ? nearToYocto(threshold) : null,
});
