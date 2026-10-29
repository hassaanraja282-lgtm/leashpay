#!/usr/bin/env bash
# Creates a complete, fresh LeashPay deployment on NEAR TESTNET (free, faucet-funded) and writes .env.
# Requires: near-cli-rs (`near`), cargo-near, Rust with wasm32-unknown-unknown, Node >= 20.
# Usage: ./scripts/setup-testnet.sh [owner-name]      (default: leashpay-<random>.testnet)
# Keys are stored only in ~/.near-credentials/testnet (legacy keychain) and the git-ignored .env.
set -euo pipefail
cd "$(dirname "$0")/.."

OWNER="${1:-leashpay-$(node -e 'console.log(require("crypto").randomBytes(3).toString("hex"))').testnet}"
case "$OWNER" in *.testnet) ;; *) echo "owner must end with .testnet (testnet only)"; exit 1;; esac
VAULT="vault.$OWNER"; AGENT="agent.$OWNER"; API="weather-api.$OWNER"; GPU="gpu-cloud.$OWNER"
NET="network-config testnet"
YOCTO=000000000000000000000000

echo "==> 1/6 creating owner $OWNER via the free testnet faucet"
near account create-account sponsor-by-faucet-service "$OWNER" autogenerate-new-keypair save-to-legacy-keychain $NET create

sub() { near account create-account fund-myself "$1" "$2" autogenerate-new-keypair save-to-legacy-keychain sign-as "$OWNER" $NET sign-with-legacy-keychain send; }
echo "==> 2/6 creating sub-accounts (vault, agent, two vendors)"
sub "$VAULT" '3.5 NEAR'; sub "$AGENT" '0.5 NEAR'; sub "$API" '0.1 NEAR'; sub "$GPU" '0.1 NEAR'

echo "==> 3/6 building and deploying the contract"
(cd contract && cargo near build non-reproducible-wasm)
WASM="${CARGO_TARGET_DIR:-contract/target}/near/leashpay.wasm"
near contract deploy "$VAULT" use-file "$WASM" with-init-call new json-args \
  "{\"owner\":\"$OWNER\",\"agent\":\"$AGENT\",\"per_tx_cap\":\"1$YOCTO\",\"daily_cap\":\"2$YOCTO\",\"approval_threshold\":\"5${YOCTO:1}\"}" \
  prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' $NET sign-with-legacy-keychain send

echo "==> 4/6 giving the agent a FUNCTION-CALL key: receiver=$VAULT, methods=[request_payment]"
near account add-key "$AGENT" grant-function-call-access --allowance '0.25 NEAR' --contract-account-id "$VAULT" \
  --function-names request_payment autogenerate-new-keypair save-to-legacy-keychain $NET sign-with-legacy-keychain send

call() { near contract call-function as-transaction "$VAULT" "$1" json-args "$2" prepaid-gas '50.0 Tgas' attached-deposit "$3" sign-as "$OWNER" $NET sign-with-legacy-keychain send; }
echo "==> 5/6 allow-listing vendors and funding the vault"
call add_recipient "{\"account_id\":\"$API\",\"label\":\"Weather Data API\"}" '1 yoctoNEAR'
call add_recipient "{\"account_id\":\"$GPU\",\"label\":\"GPU Cloud (compute)\"}" '1 yoctoNEAR'
call deposit '{}' '3 NEAR'

echo "==> 6/6 writing .env"
node scripts/make-env.js "$OWNER" "$VAULT"
echo "Done. Contract: https://testnet.nearblocks.io/address/$VAULT  — run: npm start"
