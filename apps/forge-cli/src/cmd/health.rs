use crate::client::ApiClient;
use crate::render;
use anyhow::{bail, Result};
use serde::Serialize;

#[derive(Serialize)]
struct HealthReport {
    url:      String,
    status:   u16,
    ok:       bool,
    token:    &'static str,
}

pub async fn run(client: &ApiClient, json: bool) -> Result<()> {
    let status = client.health().await?;
    let ok = (200..300).contains(&status);
    let token_state = if client.token().is_empty() { "unset" } else { "set" };

    if json {
        render::json_line(&HealthReport {
            url:    client.base().to_string(),
            status,
            ok,
            token:  token_state,
        });
    } else {
        println!("Forge API server @ {}", client.base());
        render::kv("status:", &format!("{} {}", status, if ok { "OK" } else { "DOWN" }));
        render::kv("token:",  token_state);
    }

    if !ok { bail!("health check failed"); }
    Ok(())
}
