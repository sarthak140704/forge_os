use crate::client::ApiClient;
use crate::render;
use anyhow::{Context, Result};
use eventsource_stream::Eventsource;
use futures::StreamExt;

pub async fn run(
    client: &ApiClient,
    json: bool,
    since: u64,
    mission: Option<String>,
    follow: bool,
) -> Result<()> {
    // We deliberately DON'T use the default reqwest timeout for streams —
    // build a fresh client with `no_timeout`.
    let http = reqwest::Client::builder()
        .build()
        .expect("stream client");

    let mut url = format!("{}/events?since={since}", client.base());
    if let Some(m) = &mission { url.push_str(&format!("&mission={m}")); }

    let resp = http
        .get(&url)
        .bearer_auth(client.token())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("{url}: {}", resp.status());
    }

    let mut stream = resp.bytes_stream().eventsource();
    let mut got_any = false;

    while let Some(event) = stream.next().await {
        let e = match event {
            Ok(e)  => e,
            Err(e) => {
                eprintln!("!! stream error: {e}");
                if !follow { break; }
                continue;
            }
        };
        got_any = true;

        let val: serde_json::Value = match serde_json::from_str(&e.data) {
            Ok(v)  => v,
            Err(err) => {
                eprintln!("!! non-json frame: {err}");
                continue;
            }
        };
        if json {
            render::json_line(&val);
        } else {
            println!("{}", render::summarize_event(&val));
        }

        if !follow {
            // Grab a single burst — server-side there's no clean "end of buffered replay"
            // marker, so we consume until the stream idles briefly. For the CLI we just
            // stop after the first frame if the user asked for --once.
            break;
        }
    }

    if !got_any && !follow && !json {
        println!("(no events)");
    }
    Ok(())
}
