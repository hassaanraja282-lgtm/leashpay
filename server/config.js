// Configuration from environment variables (or a local, git-ignored .env file).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function loadDotEnv() {
  const file = process.env.DOTENV_PATH || path.join(ROOT, '.env');
  if (!fs.existsSync(file)) return;
  for (const line of fs.readFileSync(file, 'utf8').split(/\r?\n/)) {
    const m = line.match(/^\s*([A-Z0-9_]+)\s*=\s*(.*)\s*$/);
    if (!m || process.env[m[1]] !== undefined) continue;
    process.env[m[1]] = m[2].replace(/^["']|["']$/g, '');
  }
}
loadDotEnv();

const env = (k, d = undefined) => (process.env[k] !== undefined && process.env[k] !== '' ? process.env[k] : d);
const network = env('NEAR_NETWORK', 'testnet');
if (network !== 'testnet') {
  // Hard stop: this demo never touches mainnet.
  throw new Error('LeashPay demo runs on testnet only (NEAR_NETWORK must be "testnet").');
}

export const config = {
  root: ROOT,
  port: Number(env('PORT', 3000)),
  network,
  rpcUrl: env('NEAR_RPC_URL', 'https://test.rpc.fastnear.com'),
  explorer: env('EXPLORER_URL', 'https://testnet.nearblocks.io'),
  contractId: env('LEASHPAY_CONTRACT', 'vault.leashpay-c143d2.testnet'),
  agentId: env('AGENT_ACCOUNT_ID'),
  agentKey: env('AGENT_PRIVATE_KEY'), // function-call key: request_payment on the vault only
  ownerId: env('OWNER_ACCOUNT_ID'),
  ownerKey: env('OWNER_PRIVATE_KEY'), // optional: enables owner buttons in the demo dashboard
  ownerActions: env('ENABLE_OWNER_ACTIONS', 'true') === 'true',
  geminiKey: env('GEMINI_API_KEY'),
  geminiModel: env('GEMINI_MODEL', 'gemini-2.5-flash'),
  llmMode: env('LLM_MODE', 'auto'), // auto | gemini | rules
  dataDir: env('DATA_DIR', path.join(ROOT, 'data')),
  rateLimitPerMin: Number(env('RATE_LIMIT_PER_MIN', 12)),
};

// Every account this demo touches must be a testnet account.
for (const k of ['contractId', 'agentId', 'ownerId']) {
  const v = config[k];
  if (v && !/\.testnet$/.test(v)) throw new Error(`${k}=${v} is not a .testnet account; refusing to start.`);
}
