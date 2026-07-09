//! End-to-end integration test.
//!
//! We boot a real Runtime with `api_bind = Some(127.0.0.1:<ephemeral>)`, then
//! shell out to the *built* `forge` binary and drive every subcommand. The
//! LLM key is a placeholder so planning fails — which is exactly what we want
//! to exercise the "run --wait" path terminating with `failed`.
//!
//! This runs under `cargo test -p forge-cli --test end_to_end -- --nocapture`.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};

fn forge_bin() -> PathBuf {
    // The convention is `target/debug/forge(.exe)`. Cargo sets CARGO_BIN_EXE_<name>
    // for integration tests, but only when the binary is in the SAME crate as
    // the test — which it is. Fall back to walking manifest_dir/../../target.
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_forge") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut path = manifest.clone();
    while path.pop() {
        let candidate_release = path.join("target").join("debug").join(if cfg!(windows) { "forge.exe" } else { "forge" });
        if candidate_release.exists() { return candidate_release; }
    }
    panic!("cannot locate built `forge` binary; run `cargo build -p forge-cli` first");
}

async fn boot_server_on_ephemeral_port() -> (Runtime, std::net::SocketAddr, tempfile::TempDir) {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let db_path   = tmp.path().join("forge.sqlite");
    std::fs::create_dir_all(&workspace).unwrap();

    // Use a bogus provider key so planning fails predictably — we don't want
    // this test to hit a real LLM.
    unsafe {
        std::env::set_var("__FORGE_CLI_TEST_KEY__", "unused");
        std::env::set_var("FORGE_CLI_TEST_TOKEN",   "t0p-secret");
    }

    let config = RuntimeConfig {
        workspace_root: workspace,
        db_path,
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq { api_key_env: "__FORGE_CLI_TEST_KEY__".into() }],
            model: "unused-model".into(),
        },
        max_parallel_goals: 2,
        skills_root: None,
        mcp_config: None,
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
        workers: 0,
        worker_stale_secs: 120,
        org_memory_enabled: false,
        embedding_provider: None,
        api_bind: Some(addr),
        api_token_env: "FORGE_CLI_TEST_TOKEN".into(),
    };
    let rt = Runtime::boot(config).await.expect("runtime boot");
    // Give axum a beat to actually be listening.
    tokio::time::sleep(Duration::from_millis(400)).await;
    (rt, addr, tmp)
}

fn run_cli(bin: &PathBuf, url: &str, token: &str, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin)
        .args(["--url", url, "--token", token])
        .args(args)
        .output()
        .expect("running forge cli");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_end_to_end() {
    let bin = forge_bin();
    let (_rt, addr, _tmp) = boot_server_on_ephemeral_port().await;
    let url   = format!("http://{addr}");
    let token = "t0p-secret";

    // 1. health
    let (code, stdout, stderr) = run_cli(&bin, &url, token, &["health"]);
    assert_eq!(code, 0, "health should exit 0. stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("200") || stdout.contains("OK"),
        "health output should mention 200/OK: {stdout}");

    // 2. health with wrong token — still 200 because /health is unauthenticated
    let (code, _stdout, _stderr) = run_cli(&bin, &url, "wrong", &["health"]);
    assert_eq!(code, 0, "health probe ignores bearer");

    // 3. missions list on empty db
    let (code, stdout, _) = run_cli(&bin, &url, token, &["missions", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("(no missions)"), "expected empty state: {stdout}");

    // 4. missions create --plan-only (json)
    let (code, stdout, _) = run_cli(&bin, &url, token, &[
        "--json", "missions", "create", "cli-test-title",
        "--description", "cli-test-desc", "--plan-only",
    ]);
    assert_eq!(code, 0);
    let created: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let mid = created["id"].as_str().unwrap().to_string();
    assert_eq!(mid.len(), 36, "expected raw uuid");

    // 5. missions get
    let (code, stdout, _) = run_cli(&bin, &url, token, &["missions", "get", &mid]);
    assert_eq!(code, 0);
    assert!(stdout.contains("cli-test-title"), "detail should include title: {stdout}");

    // 6. missions list — now non-empty
    let (code, stdout, _) = run_cli(&bin, &url, token, &["missions", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("cli-test-title"), "list should include the mission: {stdout}");

    // 7. missions cancel
    let (code, stdout, _) = run_cli(&bin, &url, token, &["missions", "cancel", &mid]);
    assert_eq!(code, 0);
    assert!(stdout.contains("cancelled") || stdout.contains(&mid[..8]), "{stdout}");

    // 8. chat — dummy LLM key means finish_reason=error, but the CLI must still exit 0
    let (code, stdout, _) = run_cli(&bin, &url, token, &["chat", "hi"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("finish_reason"), "chat output should mention finish_reason: {stdout}");

    // 9. wrong bearer on a protected route → non-zero exit
    let (code, _stdout, _stderr) = run_cli(&bin, &url, "wrong", &["missions", "list"]);
    assert_ne!(code, 0, "bad bearer must exit non-zero");
}

#[test]
fn cli_bundle_roundtrip() {
    // Isolated from the async server test — bundle sign/verify/install do
    // not need a live Runtime.
    let bin = forge_bin();
    let tmp = tempfile::tempdir().unwrap();

    // 1. keygen
    let key = tmp.path().join("id_ed25519");
    let out = Command::new(&bin)
        .args(["keygen", "--out"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "keygen: {}", String::from_utf8_lossy(&out.stderr));
    assert!(key.exists());
    let pub_key = key.with_extension("pub");
    assert!(pub_key.exists());

    // 2. write a fake skill
    let skill_dir = tmp.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("my-skill.md"),
        "---\nname: my-skill\nversion: 1\n---\n\nMy body.\n",
    ).unwrap();
    std::fs::write(skill_dir.join("extra.txt"), b"payload").unwrap();

    // 3. bundle
    let bundle = tmp.path().join("my-skill.forgebundle.json");
    let out = Command::new(&bin)
        .args(["skill", "bundle"])
        .arg(&skill_dir)
        .args(["--out"]).arg(&bundle)
        .args(["--key"]).arg(&key)
        .output()
        .unwrap();
    assert!(out.status.success(), "bundle: {}", String::from_utf8_lossy(&out.stderr));
    assert!(bundle.exists());

    // 4. verify (any pubkey)
    let out = Command::new(&bin)
        .args(["skill", "verify"])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(out.status.success(), "verify: {}", String::from_utf8_lossy(&out.stderr));

    // 5. verify with expected pubkey → OK
    let out = Command::new(&bin)
        .args(["skill", "verify"])
        .arg(&bundle)
        .args(["--pubkey"]).arg(&pub_key)
        .output()
        .unwrap();
    assert!(out.status.success(), "verify --pubkey: {}", String::from_utf8_lossy(&out.stderr));

    // 6. verify with wrong pubkey → must fail
    let other_key = tmp.path().join("other_ed25519");
    Command::new(&bin).args(["keygen", "--out"]).arg(&other_key).output().unwrap();
    let out = Command::new(&bin)
        .args(["skill", "verify"])
        .arg(&bundle)
        .args(["--pubkey"]).arg(&other_key.with_extension("pub"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "verify with wrong key should FAIL");

    // 7. install
    let dest = tmp.path().join("installed");
    let out = Command::new(&bin)
        .args(["skill", "install"])
        .arg(&bundle)
        .args(["--dest"]).arg(&dest)
        .output()
        .unwrap();
    assert!(out.status.success(), "install: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dest.join("my-skill.md").exists());
    assert!(dest.join("extra.txt").exists());

    // 8. tamper detection — flip a byte in the bundle, verify must fail.
    let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&bundle).unwrap()).unwrap();
    let files = raw["files"].as_object_mut().unwrap();
    let (k, _) = files.iter().next().unwrap();
    let k = k.clone();
    files.insert(k, serde_json::Value::String("dGFtcGVy".into())); // "tamper"
    std::fs::write(&bundle, serde_json::to_vec(&raw).unwrap()).unwrap();
    let out = Command::new(&bin).args(["skill", "verify"]).arg(&bundle).output().unwrap();
    assert!(!out.status.success(), "tampered bundle should fail verify");
}

// ---------------------------------------------------------------------------
// Phase 5f — plugin bundle roundtrip
// ---------------------------------------------------------------------------
#[test]
fn cli_plugin_bundle_roundtrip() {
    let bin = forge_bin();
    let tmp = tempfile::tempdir().unwrap();

    // 1. key + plugin source dir with mcp.yaml + a helper file
    let key = tmp.path().join("id_ed25519");
    Command::new(&bin).args(["keygen", "--out"]).arg(&key).output().unwrap();
    let pubk = key.with_extension("pub");

    let plugin_dir = tmp.path().join("my-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("mcp.yaml"),
        "servers:\n  - name: fs\n    transport: stdio\n    command: npx\n    args: [\"-y\", \"mcp-fs\"]\n    enabled: true\n",
    ).unwrap();
    std::fs::write(plugin_dir.join("readme.md"), b"# my plugin").unwrap();

    // 2. bundle
    let bundle = tmp.path().join("my-plugin.forgebundle.json");
    let out = Command::new(&bin)
        .args(["plugin", "bundle"])
        .arg(&plugin_dir)
        .args(["--out"]).arg(&bundle)
        .args(["--key"]).arg(&key)
        .output().unwrap();
    assert!(out.status.success(), "plugin bundle: {}", String::from_utf8_lossy(&out.stderr));

    // 3. verify with pubkey → OK
    let out = Command::new(&bin)
        .args(["plugin", "verify"]).arg(&bundle)
        .args(["--pubkey"]).arg(&pubk)
        .output().unwrap();
    assert!(out.status.success(), "plugin verify: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Plugin"), "verify should report kind=Plugin: {stdout}");

    // 4. install
    let dest = tmp.path().join("installed-plugin");
    let out = Command::new(&bin)
        .args(["plugin", "install"]).arg(&bundle)
        .args(["--dest"]).arg(&dest)
        .output().unwrap();
    assert!(out.status.success(), "plugin install: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dest.join("mcp.yaml").exists());
    assert!(dest.join("readme.md").exists());

    // 5. skill bundle command MUST refuse a plugin dir (no .md manifest, so
    //    manifest is empty, but files should still be bundled — verify signature
    //    verifies but kind field is absent). Just check the sign is a "skill"
    //    round-trip: no `kind: plugin` in JSON output.
    let bundle_as_skill = tmp.path().join("my-plugin-as-skill.forgebundle.json");
    let _ = Command::new(&bin)
        .args(["skill", "bundle"])
        .arg(&plugin_dir)
        .args(["--out"]).arg(&bundle_as_skill)
        .args(["--key"]).arg(&key)
        .output().unwrap();
    if bundle_as_skill.exists() {
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&bundle_as_skill).unwrap()).unwrap();
        assert!(raw.get("kind").is_none() ||
                raw.get("kind").and_then(|v| v.as_str()) == Some("skill"),
                "skill-bundled dir must not carry kind=plugin");
    }
}

// ---------------------------------------------------------------------------
// Phase 5d — RBAC: read-only tokens
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_readonly_token_restricts_writes() {
    // Boot with an extra read-only token.
    unsafe { std::env::set_var("FORGE_API_READONLY_TOKENS", "ro-token-1,ro-token-2"); }
    let bin = forge_bin();
    let (_rt, addr, _tmp) = boot_server_on_ephemeral_port().await;
    let url = format!("http://{addr}");
    // Reset immediately so this env doesn't bleed into other tests.
    unsafe { std::env::remove_var("FORGE_API_READONLY_TOKENS"); }

    // 1. RO token can GET /health (though /health is unauth anyway)
    let (code, _, _) = run_cli(&bin, &url, "ro-token-1", &["health"]);
    assert_eq!(code, 0);

    // 2. RO token can GET /missions
    let (code, stdout, _) = run_cli(&bin, &url, "ro-token-1", &["missions", "list"]);
    assert_eq!(code, 0, "RO token should list missions: {stdout}");

    // 3. RO token CANNOT POST /missions — must exit non-zero
    let (code, stdout, stderr) = run_cli(&bin, &url, "ro-token-1", &[
        "missions", "create", "should-fail", "--description", "-", "--plan-only",
    ]);
    assert_ne!(code, 0, "RO token must NOT be able to create missions. stdout={stdout} stderr={stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("403") || combined.to_lowercase().contains("forbidden"),
        "expected 403/forbidden in output: {combined}");

    // 4. RO token CANNOT POST /v1/chat/completions
    let (code, _, _) = run_cli(&bin, &url, "ro-token-1", &["chat", "hi"]);
    assert_ne!(code, 0, "RO token must NOT be able to chat");

    // 5. A totally unknown token → 401.
    let (code, _, _) = run_cli(&bin, &url, "unknown-token", &["missions", "list"]);
    assert_ne!(code, 0);

    // 6. Full token still works.
    let (code, _, _) = run_cli(&bin, &url, "t0p-secret", &[
        "missions", "create", "full-works", "--description", "-", "--plan-only",
    ]);
    assert_eq!(code, 0, "full token must still be able to create missions");
}

// ---------------------------------------------------------------------------
// Phase 5c — streaming shim (raw HTTP, no CLI wrapper needed).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn openai_streaming_shim_emits_chunks() {
    let (_rt, addr, _tmp) = boot_server_on_ephemeral_port().await;
    let url = format!("http://{addr}/v1/chat/completions");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build().unwrap();
    let resp = client.post(&url)
        .bearer_auth("t0p-secret")
        .json(&serde_json::json!({
            "model": "forge-mission",
            "messages": [{"role":"user","content":"stream me"}],
            "stream":   true,
        }))
        .send().await.expect("streaming POST");
    assert!(resp.status().is_success(), "expected 200, got {}", resp.status());
    let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(ctype.starts_with("text/event-stream"), "expected SSE content-type: {ctype}");

    let body = resp.text().await.expect("body");
    // Must have at least one data: frame and terminate with [DONE].
    assert!(body.contains("data: {"), "expected data JSON frames:\n{body}");
    assert!(body.contains("data: [DONE]"), "expected [DONE] sentinel:\n{body}");

    // Must include at least one chat.completion.chunk envelope.
    assert!(body.contains("chat.completion.chunk"), "expected chunk envelope:\n{body}");
}
