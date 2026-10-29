use super::*;
use near_sdk::PromiseResult;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{testing_env, RuntimeFeesConfig};
use near_sdk::test_vm_config;

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

fn owner() -> AccountId { "owner.testnet".parse().unwrap() }
fn agent() -> AccountId { "agent.testnet".parse().unwrap() }
fn api() -> AccountId { "weather-api.testnet".parse().unwrap() }
fn gpu() -> AccountId { "gpu-cloud.testnet".parse().unwrap() }
fn stranger() -> AccountId { "unknown.testnet".parse().unwrap() }
fn vault() -> AccountId { "leashpay.testnet".parse().unwrap() }

fn ctx(caller: AccountId, deposit: u128, ts: u64) -> VMContextBuilder {
    let mut b = VMContextBuilder::new();
    b.current_account_id(vault())
        .predecessor_account_id(caller.clone())
        .signer_account_id(caller)
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .account_balance(NearToken::from_near(20))
        .block_timestamp(ts);
    b
}

fn set(caller: AccountId, deposit: u128, ts: u64) {
    testing_env!(ctx(caller, deposit, ts).build());
}

const T0: u64 = 1_800_000_000_000_000_000; // some day, mid-day UTC

/// per-tx 2 NEAR, daily 3 NEAR, approval above 1 NEAR; weather-api + gpu-cloud allow-listed.
fn setup() -> LeashPay {
    set(owner(), 0, T0);
    let mut c = LeashPay::new(owner(), agent(), U128(2 * NEAR), U128(3 * NEAR), Some(U128(NEAR)));
    set(owner(), 1, T0);
    c.add_recipient(api(), "Weather Data API".into());
    c.add_recipient(gpu(), "GPU Cloud".into());
    c
}

fn pay(c: &mut LeashPay, to: AccountId, amt: u128) -> PaymentResult {
    c.request_payment(to, U128(amt), Some("test".into()))
}

#[test]
fn small_payment_to_allowlisted_recipient_is_paid() {
    let mut c = setup();
    set(agent(), 0, T0);
    let r = pay(&mut c, api(), NEAR / 2);
    assert_eq!(r.status, "paid");
    assert_eq!(r.remaining_today.0, 3 * NEAR - NEAR / 2);
    assert_eq!(c.get_allowance().spent_today.0, NEAR / 2);
    assert_eq!(c.get_log(Some(1), None)[0].kind, "paid");
    assert!(get_logs().iter().any(|l| l.starts_with("EVENT_JSON:") && l.contains("\"event\":\"paid\"")));
}

#[test]
fn recipient_not_on_allowlist_is_blocked() {
    let mut c = setup();
    set(agent(), 0, T0);
    let r = pay(&mut c, stranger(), NEAR / 10);
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("not on the allow-list"));
    assert_eq!(c.get_allowance().spent_today.0, 0);
    let last = &c.get_log(Some(1), None)[0];
    assert_eq!(last.kind, "blocked");
    assert_eq!(last.recipient, Some(stranger()));
}

#[test]
fn amount_over_per_tx_cap_is_blocked() {
    let mut c = setup();
    set(agent(), 0, T0);
    let r = pay(&mut c, api(), 50 * NEAR);
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("per-transaction cap"));
    assert!(c.get_pending().is_empty());
}

#[test]
fn daily_cap_blocks_once_exhausted() {
    let mut c = setup();
    set(agent(), 0, T0);
    assert_eq!(pay(&mut c, api(), NEAR).status, "paid");
    assert_eq!(pay(&mut c, gpu(), NEAR).status, "paid");
    assert_eq!(pay(&mut c, api(), NEAR / 2).status, "paid");
    let r = pay(&mut c, api(), NEAR); // 2.5 spent, 1 more would be 3.5 > 3
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("daily cap exceeded"));
    assert_eq!(c.get_allowance().remaining_today.0, NEAR / 2);
}

#[test]
fn daily_cap_resets_next_utc_day() {
    let mut c = setup();
    set(agent(), 0, T0);
    pay(&mut c, api(), NEAR);
    pay(&mut c, api(), NEAR);
    pay(&mut c, api(), NEAR);
    assert_eq!(pay(&mut c, api(), NEAR / 10).status, "blocked");
    let tomorrow = (T0 / DAY_NS + 1) * DAY_NS + 1;
    set(agent(), 0, tomorrow);
    // the view already reflects the new day before any write
    assert_eq!(c.get_allowance().remaining_today.0, 3 * NEAR);
    assert_eq!(pay(&mut c, api(), NEAR).status, "paid");
    assert_eq!(c.get_allowance().spent_today.0, NEAR);
    assert_eq!(c.get_allowance().resets_at, (tomorrow / DAY_NS + 1) * DAY_NS);
}

#[test]
fn above_threshold_goes_to_queue_then_owner_approves() {
    let mut c = setup();
    set(agent(), 0, T0);
    let r = pay(&mut c, gpu(), 3 * NEAR / 2);
    assert_eq!(r.status, "queued");
    let pid = r.pending_id.unwrap();
    assert_eq!(c.get_pending().len(), 1);
    assert_eq!(c.get_allowance().spent_today.0, 0, "queued payments do not spend allowance");

    set(owner(), 1, T0);
    c.approve(pid);
    assert!(c.get_pending().is_empty());
    assert_eq!(c.get_allowance().spent_today.0, 3 * NEAR / 2);
    let last = &c.get_log(Some(1), None)[0];
    assert_eq!(last.kind, "approved");
    assert_eq!(last.pending_id, Some(pid));
}

#[test]
fn owner_can_reject_queued_payment() {
    let mut c = setup();
    set(agent(), 0, T0);
    let pid = pay(&mut c, gpu(), 3 * NEAR / 2).pending_id.unwrap();
    set(owner(), 1, T0);
    c.reject(pid, Some("not needed".into()));
    assert!(c.get_pending().is_empty());
    assert_eq!(c.get_allowance().spent_today.0, 0);
    assert_eq!(c.get_log(Some(1), None)[0].kind, "rejected");
}

#[test]
#[should_panic(expected = "Unauthorized: owner only")]
fn agent_cannot_approve_its_own_request() {
    let mut c = setup();
    set(agent(), 0, T0);
    let pid = pay(&mut c, gpu(), 3 * NEAR / 2).pending_id.unwrap();
    set(agent(), 1, T0);
    c.approve(pid);
}

#[test]
fn pause_blocks_everything_and_unpause_restores() {
    let mut c = setup();
    set(owner(), 1, T0);
    c.pause();
    assert!(c.get_config().paused);
    set(agent(), 0, T0);
    let r = pay(&mut c, api(), NEAR / 10);
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("paused"));
    set(owner(), 1, T0);
    c.unpause();
    set(agent(), 0, T0);
    assert_eq!(pay(&mut c, api(), NEAR / 10).status, "paid");
}

#[test]
#[should_panic(expected = "Unauthorized: only the registered agent")]
fn stranger_cannot_request_payment() {
    let mut c = setup();
    set(stranger(), 0, T0);
    pay(&mut c, api(), NEAR / 10);
}

#[test]
#[should_panic(expected = "Unauthorized: only the registered agent")]
fn owner_is_not_the_agent() {
    let mut c = setup();
    set(owner(), 0, T0);
    pay(&mut c, api(), NEAR / 10);
}

#[test]
#[should_panic(expected = "Unauthorized: owner only")]
fn agent_cannot_change_policy() {
    let mut c = setup();
    set(agent(), 1, T0);
    c.set_policy(U128(100 * NEAR), U128(100 * NEAR), None);
}

#[test]
#[should_panic(expected = "Unauthorized: owner only")]
fn agent_cannot_add_recipient() {
    let mut c = setup();
    set(agent(), 1, T0);
    c.add_recipient(stranger(), "me".into());
}

#[test]
#[should_panic(expected = "Unauthorized: owner only")]
fn agent_cannot_withdraw() {
    let mut c = setup();
    set(agent(), 1, T0);
    let _ = c.withdraw(U128(NEAR));
}

#[test]
#[should_panic(expected = "Unauthorized: owner only")]
fn agent_cannot_unpause() {
    let mut c = setup();
    set(owner(), 1, T0);
    c.pause();
    set(agent(), 1, T0);
    c.unpause();
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn owner_actions_require_one_yocto_full_access_key() {
    let mut c = setup();
    set(owner(), 0, T0);
    c.pause();
}

#[test]
fn removed_recipient_is_blocked() {
    let mut c = setup();
    set(owner(), 1, T0);
    c.remove_recipient(api());
    set(agent(), 0, T0);
    assert_eq!(pay(&mut c, api(), NEAR / 10).status, "blocked");
}

#[test]
fn rotating_agent_locks_out_old_agent() {
    let mut c = setup();
    let new_agent: AccountId = "agent2.testnet".parse().unwrap();
    set(owner(), 1, T0);
    c.set_agent(new_agent.clone());
    set(new_agent, 0, T0);
    assert_eq!(pay(&mut c, api(), NEAR / 10).status, "paid");
    set(agent(), 0, T0);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pay(&mut c, api(), NEAR / 10)));
    assert!(r.is_err());
}

#[test]
fn policy_update_takes_effect() {
    let mut c = setup();
    set(owner(), 1, T0);
    c.set_policy(U128(NEAR / 10), U128(NEAR), None);
    set(agent(), 0, T0);
    assert_eq!(pay(&mut c, api(), NEAR / 2).status, "blocked");
    // threshold removed: nothing queues now
    assert_eq!(pay(&mut c, api(), NEAR / 10).status, "paid");
}

#[test]
#[should_panic(expected = "per-transaction cap cannot exceed daily cap")]
fn invalid_policy_rejected() {
    let mut c = setup();
    set(owner(), 1, T0);
    c.set_policy(U128(5 * NEAR), U128(NEAR), None);
}

#[test]
fn insufficient_balance_is_blocked() {
    set(owner(), 0, T0);
    let mut c = LeashPay::new(owner(), agent(), U128(50 * NEAR), U128(100 * NEAR), None);
    set(owner(), 1, T0);
    c.add_recipient(api(), "api".into());
    set(agent(), 0, T0); // vault balance is 20 NEAR
    let r = pay(&mut c, api(), 30 * NEAR);
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("insufficient vault balance"));
}

#[test]
fn failed_transfer_refunds_allowance() {
    let mut c = setup();
    set(agent(), 0, T0);
    pay(&mut c, api(), NEAR);
    assert_eq!(c.get_allowance().spent_today.0, NEAR);
    testing_env!(
        ctx(vault(), 0, T0).build(),
        test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Failed]
    );
    assert!(!c.on_transfer_complete(0, api(), U128(NEAR)));
    assert_eq!(c.get_allowance().spent_today.0, 0);
    assert_eq!(c.get_log(Some(1), None)[0].kind, "transfer_failed");
}

#[test]
fn audit_log_is_bounded_and_newest_first() {
    let mut c = setup(); // 2 entries already
    set(agent(), 0, T0);
    for _ in 0..(MAX_LOG + 10) {
        set(agent(), 0, T0); // fresh context per tx (per-receipt log limit)
        pay(&mut c, stranger(), 1);
    }
    let cfg = c.get_config();
    assert_eq!(cfg.log_count, MAX_LOG + 12);
    let page = c.get_log(Some(100), None);
    assert_eq!(page.len(), 100);
    assert!(page[0].id > page[1].id);
    // entries older than the window are gone
    let mut all = Vec::new();
    let mut before = None;
    loop {
        let p = c.get_log(Some(100), before);
        if p.is_empty() { break; }
        before = Some(p.last().unwrap().id);
        all.extend(p);
    }
    assert_eq!(all.len() as u64, MAX_LOG);
}

#[test]
fn queue_is_bounded() {
    let mut c = setup();
    set(agent(), 0, T0);
    for _ in 0..MAX_PENDING {
        assert_eq!(pay(&mut c, gpu(), 3 * NEAR / 2).status, "queued");
    }
    let r = pay(&mut c, gpu(), 3 * NEAR / 2);
    assert_eq!(r.status, "blocked");
    assert!(r.reason.unwrap().contains("queue full"));
}
