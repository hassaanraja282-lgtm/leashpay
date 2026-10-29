//! LeashPay — an on-chain spending leash for AI agents on NEAR.
//!
//! A human `owner` funds this vault and sets a policy. An AI `agent` account may only
//! call `request_payment`. The agent is expected to hold nothing but a NEAR
//! *function-call access key* whose receiver is this contract and whose only allowed
//! method is `request_payment`, so it cannot move funds any other way.
//!
//! Every request is checked against the policy (pause switch, recipient allow-list,
//! per-transaction cap, daily cap, vault balance, approval threshold) and the outcome
//! is written to a bounded on-chain audit log and emitted as a NEP-297 event.

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise,
};

/// One day in nanoseconds. The daily cap resets at 00:00 UTC.
pub const DAY_NS: u64 = 86_400_000_000_000;
/// Maximum number of audit entries kept on-chain (older ones are pruned, ring-buffer style).
pub const MAX_LOG: u64 = 500;
/// Maximum pending approvals at once (bounds storage an agent can make the vault pay for).
pub const MAX_PENDING: u32 = 25;
/// NEAR kept aside so the contract can always pay for its own storage.
pub const STORAGE_RESERVE: NearToken = NearToken::from_millinear(500);
const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(10);
const MAX_MEMO: usize = 140;

#[derive(BorshStorageKey)]
#[near]
enum Key {
    Recipients,
    Pending,
    Log,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// Largest single payment the agent may request (yoctoNEAR).
    pub per_tx_cap: U128,
    /// Total the agent may spend per UTC day (yoctoNEAR).
    pub daily_cap: U128,
    /// Payments strictly above this amount wait for owner approval. `None` = never.
    pub approval_threshold: Option<U128>,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPayment {
    pub id: u64,
    pub recipient: AccountId,
    pub amount: U128,
    pub memo: String,
    pub requested_at: u64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub id: u64,
    pub timestamp: u64,
    pub block_height: u64,
    /// paid | blocked | queued | approved | rejected | transfer_failed | deposit | withdraw |
    /// policy_updated | recipient_added | recipient_removed | paused | unpaused | agent_changed
    pub kind: String,
    pub actor: AccountId,
    pub recipient: Option<AccountId>,
    pub amount: Option<U128>,
    pub memo: Option<String>,
    pub reason: Option<String>,
    pub pending_id: Option<u64>,
}

/// What the agent gets back from `request_payment`.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct PaymentResult {
    /// "paid", "queued" or "blocked"
    pub status: String,
    pub reason: Option<String>,
    pub log_id: u64,
    pub pending_id: Option<u64>,
    pub remaining_today: U128,
}

#[near(serializers = [json])]
pub struct Allowance {
    pub daily_cap: U128,
    pub spent_today: U128,
    pub remaining_today: U128,
    pub per_tx_cap: U128,
    pub resets_at: u64,
}

#[near(serializers = [json])]
pub struct Config {
    pub owner: AccountId,
    pub agent: AccountId,
    pub policy: Policy,
    pub paused: bool,
    pub pending_count: u32,
    pub log_count: u64,
}

#[near(serializers = [json])]
pub struct VaultBalance {
    pub total: U128,
    pub storage_locked: U128,
    pub reserve: U128,
    pub available: U128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct LeashPay {
    owner: AccountId,
    agent: AccountId,
    policy: Policy,
    paused: bool,
    /// recipient -> human label ("Weather Data API")
    recipients: IterableMap<AccountId, String>,
    pending: IterableMap<u64, PendingPayment>,
    next_pending_id: u64,
    spent_today: u128,
    day_index: u64,
    log: LookupMap<u64, AuditEntry>,
    log_count: u64,
}

#[near]
impl LeashPay {
    #[init]
    pub fn new(
        owner: AccountId,
        agent: AccountId,
        per_tx_cap: U128,
        daily_cap: U128,
        approval_threshold: Option<U128>,
    ) -> Self {
        let policy = Policy { per_tx_cap, daily_cap, approval_threshold };
        Self::validate_policy(&policy);
        require!(owner != agent, "owner and agent must be different accounts");
        Self {
            owner,
            agent,
            policy,
            paused: false,
            recipients: IterableMap::new(Key::Recipients),
            pending: IterableMap::new(Key::Pending),
            next_pending_id: 0,
            spent_today: 0,
            day_index: env::block_timestamp() / DAY_NS,
            log: LookupMap::new(Key::Log),
            log_count: 0,
        }
    }

    // ------------------------------------------------------------------ agent

    /// The only method the agent's function-call key may call.
    /// Never panics on a policy violation: the refusal is recorded in the audit log instead.
    pub fn request_payment(&mut self, recipient: AccountId, amount: U128, memo: Option<String>) -> PaymentResult {
        let caller = env::predecessor_account_id();
        require!(caller == self.agent, "Unauthorized: only the registered agent can request payments");
        self.roll_day();
        let memo = memo.map(|m| m.chars().take(MAX_MEMO).collect::<String>());
        let amt = amount.0;

        if let Some(reason) = self.check_policy(&recipient, amt) {
            let id = self.record("blocked", Some(recipient), Some(amount), memo, Some(reason.clone()), None);
            return self.result("blocked", Some(reason), id, None);
        }

        if let Some(th) = self.policy.approval_threshold {
            if amt > th.0 {
                if self.pending.len() >= MAX_PENDING {
                    let reason = "approval queue full".to_string();
                    let id = self.record("blocked", Some(recipient), Some(amount), memo, Some(reason.clone()), None);
                    return self.result("blocked", Some(reason), id, None);
                }
                let pid = self.next_pending_id;
                self.next_pending_id += 1;
                self.pending.insert(
                    pid,
                    PendingPayment {
                        id: pid,
                        recipient: recipient.clone(),
                        amount,
                        memo: memo.clone().unwrap_or_default(),
                        requested_at: env::block_timestamp(),
                    },
                );
                let reason = format!("above approval threshold of {} yoctoNEAR", th.0);
                let id = self.record("queued", Some(recipient), Some(amount), memo, Some(reason.clone()), Some(pid));
                return self.result("queued", Some(reason), id, Some(pid));
            }
        }

        self.spent_today += amt;
        let id = self.record("paid", Some(recipient.clone()), Some(amount), memo, None, None);
        self.pay(recipient, amt, id);
        self.result("paid", None, id, None)
    }

    // ------------------------------------------------------------------ owner

    /// Anyone may top up the vault; it is logged.
    #[payable]
    pub fn deposit(&mut self) {
        let amount = env::attached_deposit();
        require!(amount.as_yoctonear() > 0, "attach NEAR to deposit");
        self.record("deposit", None, Some(U128(amount.as_yoctonear())), None, None, None);
    }

    #[payable]
    pub fn approve(&mut self, pending_id: u64) -> u64 {
        self.assert_owner();
        self.roll_day();
        let p = self.pending.remove(&pending_id).unwrap_or_else(|| env::panic_str("no such pending payment"));
        require!(p.amount.0 <= self.available(), "insufficient vault balance");
        self.spent_today += p.amount.0;
        let id = self.record("approved", Some(p.recipient.clone()), Some(p.amount), Some(p.memo), None, Some(pending_id));
        self.pay(p.recipient, p.amount.0, id);
        id
    }

    #[payable]
    pub fn reject(&mut self, pending_id: u64, reason: Option<String>) -> u64 {
        self.assert_owner();
        let p = self.pending.remove(&pending_id).unwrap_or_else(|| env::panic_str("no such pending payment"));
        self.record("rejected", Some(p.recipient), Some(p.amount), Some(p.memo), reason, Some(pending_id))
    }

    #[payable]
    pub fn set_policy(&mut self, per_tx_cap: U128, daily_cap: U128, approval_threshold: Option<U128>) {
        self.assert_owner();
        let policy = Policy { per_tx_cap, daily_cap, approval_threshold };
        Self::validate_policy(&policy);
        self.policy = policy;
        let desc = format!(
            "per_tx_cap={} daily_cap={} approval_threshold={}",
            per_tx_cap.0,
            daily_cap.0,
            approval_threshold.map(|t| t.0.to_string()).unwrap_or_else(|| "none".into())
        );
        self.record("policy_updated", None, None, None, Some(desc), None);
    }

    #[payable]
    pub fn add_recipient(&mut self, account_id: AccountId, label: String) {
        self.assert_owner();
        require!(account_id != env::current_account_id(), "cannot allow-list the vault itself");
        require!(account_id != self.agent, "cannot allow-list the agent itself");
        let label: String = label.chars().take(60).collect();
        self.recipients.insert(account_id.clone(), label.clone());
        self.record("recipient_added", Some(account_id), None, Some(label), None, None);
    }

    #[payable]
    pub fn remove_recipient(&mut self, account_id: AccountId) {
        self.assert_owner();
        require!(self.recipients.remove(&account_id).is_some(), "recipient not on allow-list");
        self.record("recipient_removed", Some(account_id), None, None, None, None);
    }

    /// Kill switch: every agent request is refused while paused.
    #[payable]
    pub fn pause(&mut self) {
        self.assert_owner();
        self.paused = true;
        self.record("paused", None, None, None, None, None);
    }

    #[payable]
    pub fn unpause(&mut self) {
        self.assert_owner();
        self.paused = false;
        self.record("unpaused", None, None, None, None, None);
    }

    /// Rotate the agent (e.g. a key was leaked). Pending payments are kept.
    #[payable]
    pub fn set_agent(&mut self, agent: AccountId) {
        self.assert_owner();
        require!(agent != self.owner, "owner and agent must be different accounts");
        self.agent = agent.clone();
        self.record("agent_changed", Some(agent), None, None, None, None);
    }

    #[payable]
    pub fn withdraw(&mut self, amount: U128) -> Promise {
        self.assert_owner();
        require!(amount.0 > 0 && amount.0 <= self.available(), "amount exceeds available balance");
        self.record("withdraw", Some(self.owner.clone()), Some(amount), None, None, None);
        Promise::new(self.owner.clone()).transfer(NearToken::from_yoctonear(amount.0))
    }

    // ------------------------------------------------------------------ callback

    #[private]
    pub fn on_transfer_complete(&mut self, log_id: u64, recipient: AccountId, amount: U128) -> bool {
        let ok = env::promise_result_checked(0, 64).is_ok();
        if !ok {
            // Funds came back to the vault: give the allowance back too.
            self.roll_day();
            self.spent_today = self.spent_today.saturating_sub(amount.0);
            self.record(
                "transfer_failed",
                Some(recipient),
                Some(amount),
                None,
                Some(format!("transfer for log #{log_id} failed; refunded to vault")),
                None,
            );
        }
        ok
    }

    // ------------------------------------------------------------------ views

    pub fn get_config(&self) -> Config {
        Config {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            policy: self.policy.clone(),
            paused: self.paused,
            pending_count: self.pending.len(),
            log_count: self.log_count,
        }
    }

    /// Remaining daily allowance, computed for the current block's UTC day.
    pub fn get_allowance(&self) -> Allowance {
        let today = env::block_timestamp() / DAY_NS;
        let spent = if today == self.day_index { self.spent_today } else { 0 };
        Allowance {
            daily_cap: self.policy.daily_cap,
            spent_today: U128(spent),
            remaining_today: U128(self.policy.daily_cap.0.saturating_sub(spent)),
            per_tx_cap: self.policy.per_tx_cap,
            resets_at: (today + 1) * DAY_NS,
        }
    }

    pub fn get_balance(&self) -> VaultBalance {
        let total = env::account_balance().as_yoctonear();
        let storage = env::storage_byte_cost().as_yoctonear() * env::storage_usage() as u128;
        VaultBalance {
            total: U128(total),
            storage_locked: U128(storage),
            reserve: U128(STORAGE_RESERVE.as_yoctonear()),
            available: U128(self.available()),
        }
    }

    pub fn get_recipients(&self) -> Vec<(AccountId, String)> {
        self.recipients.iter().map(|(a, l)| (a.clone(), l.clone())).collect()
    }

    pub fn is_allowed(&self, account_id: AccountId) -> bool {
        self.recipients.contains_key(&account_id)
    }

    pub fn get_pending(&self) -> Vec<PendingPayment> {
        self.pending.values().cloned().collect()
    }

    /// Newest first. `limit` defaults to 50, max 100.
    pub fn get_log(&self, limit: Option<u64>, before_id: Option<u64>) -> Vec<AuditEntry> {
        let limit = limit.unwrap_or(50).min(100);
        let oldest = self.log_count.saturating_sub(MAX_LOG);
        let mut id = before_id.unwrap_or(self.log_count).min(self.log_count);
        let mut out = Vec::new();
        while id > oldest && (out.len() as u64) < limit {
            id -= 1;
            if let Some(e) = self.log.get(&id) {
                out.push(e.clone());
            }
        }
        out
    }

    // ------------------------------------------------------------------ internal

    fn check_policy(&self, recipient: &AccountId, amt: u128) -> Option<String> {
        if self.paused {
            return Some("vault is paused by owner (kill switch)".into());
        }
        if amt == 0 {
            return Some("amount must be greater than zero".into());
        }
        if !self.recipients.contains_key(recipient) {
            return Some(format!("recipient {recipient} is not on the allow-list"));
        }
        if amt > self.policy.per_tx_cap.0 {
            return Some(format!("amount exceeds per-transaction cap of {} yoctoNEAR", self.policy.per_tx_cap.0));
        }
        let needs_approval = self.policy.approval_threshold.map(|t| amt > t.0).unwrap_or(false);
        if !needs_approval {
            if self.spent_today + amt > self.policy.daily_cap.0 {
                return Some(format!(
                    "daily cap exceeded: {} of {} yoctoNEAR left today",
                    self.policy.daily_cap.0.saturating_sub(self.spent_today),
                    self.policy.daily_cap.0
                ));
            }
            if amt > self.available() {
                return Some("insufficient vault balance".into());
            }
        }
        None
    }

    fn pay(&self, recipient: AccountId, amt: u128, log_id: u64) {
        Promise::new(recipient.clone()).transfer(NearToken::from_yoctonear(amt)).then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_FOR_CALLBACK)
                .on_transfer_complete(log_id, recipient, U128(amt)),
        ).detach();
    }

    fn available(&self) -> u128 {
        let total = env::account_balance().as_yoctonear();
        let storage = env::storage_byte_cost().as_yoctonear() * env::storage_usage() as u128;
        total.saturating_sub(storage).saturating_sub(STORAGE_RESERVE.as_yoctonear())
    }

    fn roll_day(&mut self) {
        let today = env::block_timestamp() / DAY_NS;
        if today != self.day_index {
            self.day_index = today;
            self.spent_today = 0;
        }
    }

    fn assert_owner(&self) {
        assert_one_yocto();
        require!(env::predecessor_account_id() == self.owner, "Unauthorized: owner only");
    }

    fn validate_policy(p: &Policy) {
        require!(p.per_tx_cap.0 > 0 && p.daily_cap.0 > 0, "caps must be greater than zero");
        require!(p.per_tx_cap.0 <= p.daily_cap.0, "per-transaction cap cannot exceed daily cap");
        if let Some(t) = p.approval_threshold {
            require!(t.0 < p.per_tx_cap.0, "approval threshold must be below the per-transaction cap");
        }
    }

    fn result(&self, status: &str, reason: Option<String>, log_id: u64, pending_id: Option<u64>) -> PaymentResult {
        PaymentResult {
            status: status.into(),
            reason,
            log_id,
            pending_id,
            remaining_today: U128(self.policy.daily_cap.0.saturating_sub(self.spent_today)),
        }
    }

    fn record(
        &mut self,
        kind: &str,
        recipient: Option<AccountId>,
        amount: Option<U128>,
        memo: Option<String>,
        reason: Option<String>,
        pending_id: Option<u64>,
    ) -> u64 {
        let id = self.log_count;
        let entry = AuditEntry {
            id,
            timestamp: env::block_timestamp(),
            block_height: env::block_height(),
            kind: kind.into(),
            actor: env::predecessor_account_id(),
            recipient,
            amount,
            memo,
            reason,
            pending_id,
        };
        // NEP-297 event so indexers / explorers can follow the audit trail.
        env::log_str(&format!(
            "EVENT_JSON:{}",
            near_sdk::serde_json::json!({
                "standard": "leashpay",
                "version": "1.0.0",
                "event": kind,
                "data": [entry]
            })
        ));
        self.log.insert(id, entry);
        if id >= MAX_LOG {
            self.log.remove(&(id - MAX_LOG));
        }
        self.log_count += 1;
        id
    }
}

#[cfg(test)]
mod tests;
