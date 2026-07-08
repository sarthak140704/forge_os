use crate::client::ApiClient;
use crate::render;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct Msg<'a> { role: &'a str, content: &'a str }

#[derive(Serialize)]
struct ChatRequest<'a> {
    model:    &'a str,
    messages: Vec<Msg<'a>>,
    stream:   bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage:   Option<Usage>,
}
#[derive(Deserialize)]
struct Choice {
    message:       Message,
    finish_reason: String,
}
#[derive(Deserialize)]
struct Message { content: String }
#[derive(Deserialize)]
struct Usage {
    prompt_tokens:     u32,
    completion_tokens: u32,
    total_tokens:      u32,
}

pub async fn run(
    client: &ApiClient,
    json: bool,
    model: String,
    system: Option<String>,
    prompt: String,
) -> Result<()> {
    let mut msgs: Vec<Msg> = Vec::new();
    if let Some(s) = &system { msgs.push(Msg { role: "system", content: s }); }
    msgs.push(Msg { role: "user", content: &prompt });

    // The completion path can take minutes — override the CLI timeout.
    let long_client = ApiClient::new(client.base(), client.token(), 60 * 30);
    let req = ChatRequest { model: &model, messages: msgs, stream: false };
    let resp: serde_json::Value = long_client.post_json("/v1/chat/completions", &req).await?;

    if json {
        render::json_pretty(&resp);
        return Ok(());
    }

    let parsed: ChatResponse = serde_json::from_value(resp.clone())?;
    let Some(choice) = parsed.choices.first() else {
        println!("(no choices returned)");
        return Ok(());
    };
    println!("{}", choice.message.content);
    println!();
    println!("--- finish_reason: {}", choice.finish_reason);
    if let Some(u) = parsed.usage {
        println!("--- usage: prompt={} completion={} total={}",
            u.prompt_tokens, u.completion_tokens, u.total_tokens);
    }
    Ok(())
}
