// LeashPay demo server: agent chat endpoint + owner dashboard API + static UI.
import express from 'express';
import path from 'node:path';
import { config } from './config.js';
import {
  readState, agentRequestPayment, agentAttack, ownerApprove, ownerReject, ownerPause, ownerUnpause,
  ownerSetPolicy, capabilities, nearToYocto, yoctoToNear, txFor, shortError,
} from './near.js';
import { planTask, plannerName } from './agent.js';

const app = express();
app.use(express.json({ limit: '16kb' }));
app.use(express.static(path.join(config.root, 'public')));

// Tiny per-IP rate limit for writes (keeps the hosted demo's testnet gas allowance alive).
const hits = new Map();
function limit(req, res, next) {
  const ip = req.headers['x-forwarded-for']?.split(',')[0] || req.socket.remoteAddress;
  const now = Date.now();
  const arr = (hits.get(ip) || []).filter((t) => now - t < 60_000);
  if (arr.length >= config.rateLimitPerMin) return res.status(429).json({ error: 'Slow down: demo rate limit reached, try again in a minute.' });
  arr.push(now); hits.set(ip, arr); next();
}

const wrap = (fn) => async (req, res) => {
  try { res.json(await fn(req)); } catch (e) {
    res.status(e.status || 500).json({ error: shortError(e), txHash: e.txHash, explorerUrl: e.txHash ? `${config.explorer}/txns/${e.txHash}` : undefined });
  }
};

app.get('/api/health', (_req, res) => res.json({ ok: true, network: config.network, contract: config.contractId }));

app.get('/api/state', wrap(async () => {
  const s = await readState();
  return {
    ...s,
    log: s.log.map((e) => ({ ...e, txHash: txFor(e.id) || null })),
    meta: {
      network: config.network, contract: config.contractId, explorer: config.explorer,
      agentAccount: config.agentId || s.config.agent, planner: plannerName(), capabilities: capabilities(),
    },
  };
}));

app.post('/api/agent/task', limit, wrap(async (req) => {
  const text = String(req.body?.text || '').slice(0, 500);
  const { recipients } = await readState();
  const plan = await planTask(text, recipients);
  if (plan.action !== 'pay') return { plan };
  const amountYocto = nearToYocto(plan.amount_near);
  const { txHash, result } = await agentRequestPayment({ recipient: plan.recipient, amountYocto, memo: plan.memo || text.slice(0, 140) });
  return {
    plan, result, txHash, explorerUrl: `${config.explorer}/txns/${txHash}`,
    remainingNear: result ? yoctoToNear(result.remaining_today) : null,
  };
}));

app.post('/api/agent/attack', limit, wrap(async (req) => agentAttack(String(req.body?.kind))));

const ownerRoute = (fn) => wrap(async (req) => {
  const r = await fn(req.body || {});
  return { ok: true, txHash: r.hash, explorerUrl: `${config.explorer}/txns/${r.hash}` };
});
app.post('/api/owner/approve', limit, ownerRoute((b) => ownerApprove(b.id)));
app.post('/api/owner/reject', limit, ownerRoute((b) => ownerReject(b.id, b.reason)));
app.post('/api/owner/pause', limit, ownerRoute(() => ownerPause()));
app.post('/api/owner/unpause', limit, ownerRoute(() => ownerUnpause()));
app.post('/api/owner/policy', limit, ownerRoute((b) => ownerSetPolicy({ perTx: b.perTx, daily: b.daily, threshold: b.threshold })));

app.listen(config.port, () => {
  const c = capabilities();
  console.log(`LeashPay demo on http://localhost:${config.port}  (network=${config.network}, contract=${config.contractId}, agent=${c.agent ? 'ready' : 'read-only'}, owner actions=${c.owner ? 'on' : 'off'}, planner=${plannerName()})`);
});
