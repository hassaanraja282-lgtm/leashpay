// The "AI agent": turns a natural-language task into ONE request_payment call.
// It uses Gemini when GEMINI_API_KEY is set, otherwise a deterministic rule-based parser.
// Either way the agent is untrusted: the on-chain policy decides what actually happens.
import { config } from './config.js';

const ACCOUNT_RE = /\b([a-z0-9][a-z0-9_-]*(?:\.[a-z0-9][a-z0-9_-]*)*\.(?:testnet|near)|[0-9a-f]{64})\b/;
const AMOUNT_RE = /(\d+(?:\.\d+)?)\s*(?:near|Ⓝ|n\b)/i;
const BARE_AMOUNT_RE = /\b(\d+(?:\.\d+)?)\b/;

/** Words that point at a known vendor, matched against the vault's allow-list labels. */
function aliasesFor(accountId, label) {
  const words = `${label} ${accountId.split('.')[0]}`.toLowerCase().split(/[^a-z0-9]+/).filter((w) => w.length > 2);
  const extra = [];
  if (/weather|data/.test(label.toLowerCase())) extra.push('weather', 'data api', 'data', 'forecast');
  if (/gpu|compute/.test(label.toLowerCase())) extra.push('gpu', 'compute', 'training', 'inference', 'server');
  return [...new Set([...words, ...extra].filter((w) => !['api', 'cloud', 'the'].includes(w)))];
}

/** Deterministic fallback planner. */
export function parseTask(text, recipients = []) {
  const t = String(text || '').trim();
  if (!t) return { action: 'none', reply: 'Tell me what to pay for, e.g. "pay the data API 0.5 NEAR for weather data".' };
  const lower = t.toLowerCase();

  let amount = (lower.match(AMOUNT_RE) || [])[1];
  if (!amount) amount = (lower.replace(ACCOUNT_RE, ' ').match(BARE_AMOUNT_RE) || [])[1];

  let recipient = (lower.match(ACCOUNT_RE) || [])[1];
  let via = 'explicit account id';
  if (!recipient) {
    let best = null;
    for (const [acc, label] of recipients) {
      const score = aliasesFor(acc, label).filter((a) => lower.includes(a)).length;
      if (score && (!best || score > best.score)) best = { acc, score, label };
    }
    if (best) { recipient = best.acc; via = `matched vendor "${best.label}"`; }
  }

  if (!amount || !recipient) {
    return {
      action: 'none',
      reply: !amount ? 'I could not find an amount in NEAR in that task.' : 'I could not tell who to pay. Name a vendor or an account like name.testnet.',
    };
  }
  return {
    action: 'pay', recipient, amount_near: amount, memo: t.slice(0, 140),
    reasoning: `Rule-based parser: amount ${amount} NEAR, recipient ${recipient} (${via}).`,
    planner: 'rules',
  };
}

function systemPrompt(recipients) {
  const book = recipients.map(([a, l]) => `- ${l}: ${a}`).join('\n') || '- (none)';
  return `You are a purchasing agent for an AI app. You can do exactly one thing: request a payment in NEAR (testnet) from your owner's LeashPay vault.
Your owner's known vendors (address book):
${book}
Convert the user's task into JSON: {"action":"pay"|"none","recipient":"<account id>","amount_near":"<decimal string>","memo":"<short purpose>","reasoning":"<one sentence>","reply":"<message if action is none>"}.
Use an address-book account when the task refers to a known vendor. If the task names an explicit account id, use it verbatim even if unknown (the vault's on-chain policy will decide). Never invent amounts. Output JSON only.`;
}

async function planWithGemini(text, recipients) {
  const url = `https://generativelanguage.googleapis.com/v1beta/models/${config.geminiModel}:generateContent`;
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 15000);
  try {
    const r = await fetch(url, {
      method: 'POST',
      signal: ctrl.signal,
      headers: { 'Content-Type': 'application/json', 'x-goog-api-key': config.geminiKey },
      body: JSON.stringify({
        systemInstruction: { parts: [{ text: systemPrompt(recipients) }] },
        contents: [{ role: 'user', parts: [{ text }] }],
        generationConfig: { responseMimeType: 'application/json', temperature: 0 },
      }),
    });
    if (!r.ok) throw new Error(`Gemini HTTP ${r.status}`);
    const j = await r.json();
    const out = JSON.parse(j.candidates?.[0]?.content?.parts?.map((p) => p.text).join('') || '{}');
    if (out.action === 'pay') {
      if (!ACCOUNT_RE.test(String(out.recipient)) || !/^\d+(\.\d+)?$/.test(String(out.amount_near))) throw new Error('bad LLM output');
    }
    return { ...out, planner: `gemini (${config.geminiModel})` };
  } finally {
    clearTimeout(timer);
  }
}

export async function planTask(text, recipients) {
  const useGemini = config.llmMode !== 'rules' && !!config.geminiKey;
  if (useGemini) {
    try { return await planWithGemini(text, recipients); } catch (e) {
      const p = parseTask(text, recipients);
      return { ...p, planner: 'rules (Gemini unavailable: ' + String(e.message).slice(0, 60) + ')' };
    }
  }
  return parseTask(text, recipients);
}

export const plannerName = () => (config.llmMode !== 'rules' && config.geminiKey ? `Gemini ${config.geminiModel}` : 'rule-based parser');
