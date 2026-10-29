//! Integration tests on a local NEAR sandbox node (near-workspaces).
//! Build the wasm first: `cargo near build non-reproducible-wasm`, then `cargo test --test sandbox`.
//! Set LEASHPAY_WASM to override the wasm path.
use near_workspaces::types::{AccessKey, NearToken, SecretKey};
use near_workspaces::{Account, Contract};
use serde_json::{json, Value};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

fn wasm() -> Vec<u8> {
    let p = std::env::var("LEASHPAY_WASM").unwrap_or_else(|_| {
        let t = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "../contract/target".into());
        format!("{t}/near/leashpay.wasm")
    });
    std::fs::read(&p).unwrap_or_else(|_| panic!("wasm not found at {p}; run `cargo near build non-reproducible-wasm`"))
}

async fn balance(a: &Account) -> u128 {
    a.view_account().await.unwrap().balance.as_yoctonear()
}

async fn pay(agent: &Account, c: &Contract, to: &str, amt: u128) -> Value {
    agent
        .call(c.id(), "request_payment")
        .args_json(json!({"recipient": to, "amount": amt.to_string(), "memo": "sandbox"}))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .json::<Value>()
        .unwrap()
}

#[tokio::test]
async fn end_to_end_on_sandbox() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let mk = |name: &'static str, near: u128| {
        let root = root.clone();
        async move {
            root.create_subaccount(name).initial_balance(NearToken::from_near(near)).transact().await.unwrap().into_result().unwrap()
        }
    };
    let owner = mk("owner", 50).await;
    let agent = mk("agent", 5).await;
    let api = mk("api", 1).await;
    let vault_acc = mk("vault", 20).await;
    let c = vault_acc.deploy(&wasm()).await?.into_result()?;

    owner
        .call(c.id(), "new")
        .args_json(json!({"owner": owner.id(), "agent": agent.id(),
            "per_tx_cap": (NEAR).to_string(), "daily_cap": (2 * NEAR).to_string(),
            "approval_threshold": (NEAR / 2).to_string()}))
        .transact()
        .await?
        .into_result()?;
    owner
        .call(c.id(), "add_recipient")
        .args_json(json!({"account_id": api.id(), "label": "API"}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    // paid: recipient actually receives the NEAR
    let before = balance(&api).await;
    let r = pay(&agent, &c, api.id().as_str(), NEAR / 4).await;
    assert_eq!(r["status"], "paid");
    assert_eq!(balance(&api).await - before, NEAR / 4);

    // blocked: unknown recipient, nothing moves
    let r = pay(&agent, &c, "stranger.test.near", NEAR / 4).await;
    assert_eq!(r["status"], "blocked");

    // queued then approved by owner
    let r = pay(&agent, &c, api.id().as_str(), 3 * NEAR / 4).await;
    assert_eq!(r["status"], "queued");
    let pid = r["pending_id"].as_u64().unwrap();
    let before = balance(&api).await;
    owner
        .call(c.id(), "approve")
        .args_json(json!({"pending_id": pid}))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    assert_eq!(balance(&api).await - before, 3 * NEAR / 4);

    // allowance view
    let a: Value = c.view("get_allowance").await?.json()?;
    assert_eq!(a["remaining_today"], (2 * NEAR - NEAR).to_string());

    // unauthorized caller panics
    let res = owner
        .call(c.id(), "request_payment")
        .args_json(json!({"recipient": api.id(), "amount": "1", "memo": null}))
        .transact()
        .await?;
    assert!(res.is_failure());

    // function-call key: can request payments, cannot call anything else
    let sk = SecretKey::from_random(near_workspaces::types::KeyType::ED25519);
    agent
        .batch(agent.id())
        .add_key(
            sk.public_key(),
            AccessKey::function_call_access(c.id(), &["request_payment"], Some(NearToken::from_near(1))),
        )
        .transact()
        .await?
        .into_result()?;
    let leashed = Account::from_secret_key(agent.id().clone(), sk, &worker);
    let r = pay(&leashed, &c, api.id().as_str(), NEAR / 10).await;
    assert_eq!(r["status"], "paid");
    let attempt = leashed
        .call(c.id(), "withdraw")
        .args_json(json!({"amount": NEAR.to_string()}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await;
    assert!(attempt.is_err() || attempt.unwrap().is_failure(), "function-call key must not withdraw");
    let attempt = leashed.call(c.id(), "pause").transact().await;
    assert!(attempt.is_err() || attempt.unwrap().is_failure(), "function-call key must not call pause");

    // kill switch
    owner.call(c.id(), "pause").deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    let r = pay(&agent, &c, api.id().as_str(), NEAR / 10).await;
    assert_eq!(r["status"], "blocked");

    let log: Vec<Value> = c.view("get_log").args_json(json!({"limit": 100})).await?.json()?;
    assert!(log.len() >= 8);
    Ok(())
}
