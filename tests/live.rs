//! Explicit, priced, spending-limited real-provider smoke test.
use anyhow::{Context, Result};
use forge_harness::{
    config::Config,
    credentials,
    model::{Message, Role},
    providers::{HttpProvider, Provider},
    runtime::estimate_input,
    storage::Store,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "paid API request; set FORGE_LIVE_CONFIG and FORGE_LIVE_MODEL explicitly"]
async fn live_provider_smoke() -> Result<()> {
    let path = std::env::var("FORGE_LIVE_CONFIG")
        .context("set FORGE_LIVE_CONFIG to a user config TOML")?;
    let alias = std::env::var("FORGE_LIVE_MODEL")
        .context("set FORGE_LIVE_MODEL to an explicitly priced model alias")?;
    let config = Config::load(std::path::Path::new(&path))?;
    let (_, mut model) = {
        let (a, m) = config.model(Some(&alias))?;
        (a.to_string(), m.clone())
    };
    model.max_output_tokens = model.max_output_tokens.min(128);
    let price = model
        .pricing
        .as_ref()
        .context("live test requires explicit pricing")?;
    let key = credentials::resolve(&alias, &model)?;
    let d = tempfile::tempdir()?;
    let store = Store::open(&d.path().join("test.db"))?;
    let session = store.create_session("live", &alias)?;
    store.set_budget("session", &session, Some(10_000_000))?; // $0.01 local maximum reservation
    let messages = vec![Message::text(
        Role::User,
        "Reply with only OK. Do not use tools.",
    )];
    let input = estimate_input(&messages, false)?;
    let reservation = store.reserve(
        &session,
        "live",
        input,
        Some(price.reserve(input, model.max_output_tokens)?),
        Some(price),
    )?;
    let p: Arc<dyn Provider> = Arc::new(HttpProvider::new(model.clone(), key)?);
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = p
        .complete(&messages, false, tx, &CancellationToken::new())
        .await;
    drain.await?;
    match result {
        Ok(c) => {
            store.settle(&reservation.id, c.usage.as_ref(), model.pricing.as_ref())?;
            assert!(!c.text.is_empty());
            assert!(c.usage.is_some());
        }
        Err(e) => {
            store.uncertain(&reservation.id)?;
            return Err(e);
        }
    }
    Ok(())
}
