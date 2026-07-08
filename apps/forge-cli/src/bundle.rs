//! Signed skill & plugin bundles.
//!
//! A **bundle** is a JSON file with these fields:
//! ```json
//! { "manifest": <yaml manifest dict>,
//!   "files":    { "<rel/path>": "<base64-encoded bytes>", … },
//!   "signature": "<base64 ed25519 signature over the canonical bytes>",
//!   "pubkey":    "<base64 ed25519 public key that signed it>",
//!   "kind":     "skill" | "plugin"        // optional; absent = skill }
//! ```
//!
//! The signed bytes are `serde_json::to_vec(&{manifest, files, kind?})` —
//! the signature covers manifest + all file contents + kind, no more, no
//! less. Two bundles built from the same directory produce byte-identical
//! signed bytes, so signatures are reproducible.
//!
//! `kind` is `#[serde(skip_serializing_if = "Option::is_none")]` on the
//! Signed payload, so bundles built before Phase 5f (no explicit kind)
//! still verify with the exact same canonical bytes.
//!
//! # Two supported source layouts
//!
//! * **Skill** (`--kind skill`, default): the manifest is the YAML frontmatter
//!   of the first `.md` file in the directory. Everything else is a
//!   supporting file. Convention set by Phase 5b.
//! * **Plugin** (`--kind plugin`): the manifest is the top-level `mcp.yaml`
//!   or `plugin.yaml` file. Additional files (helper scripts, prompts) are
//!   bundled alongside. Convention set by Phase 5f — mirrors the
//!   `forge-mcp` config shape.
//!
//! # Design decisions
//! * **No versioning inside the bundle** — the surrounding filename is
//!   sufficient (`my-skill-v3.forgebundle.json`).
//! * **No compression** — files are already small text; zipping just adds
//!   surface area.
//! * **Public-key-per-bundle** — each bundle carries the pubkey that signed
//!   it. `verify` checks the signature, and it's the caller's job to decide
//!   whether that key is trusted.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BUNDLE_SUFFIX: &str = ".forgebundle.json";

/// Which flavor of bundle we're packing / unpacking.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BundleKind {
    Skill,
    Plugin,
}

impl Default for BundleKind {
    fn default() -> Self { BundleKind::Skill }
}

impl BundleKind {
    /// Directory name used when the CLI has to pick a default `install`
    /// destination — `active/skills/` vs `active/plugins/`. Reserved for
    /// future daemon-side dispatch.
    #[allow(dead_code)]
    pub fn install_subdir(self) -> &'static str {
        match self {
            BundleKind::Skill  => "skills",
            BundleKind::Plugin => "plugins",
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Bundle {
    /// Frontmatter of the skill file OR content of `mcp.yaml` — arbitrary
    /// YAML dict.
    pub manifest: serde_yaml::Value,
    /// Every file under the source dir, keyed by forward-slash relative path.
    pub files: BTreeMap<String, String>, // base64-encoded bytes
    /// base64 ed25519 signature over the canonical `Signed` bytes.
    pub signature: String,
    /// base64 ed25519 public key that produced the signature above.
    pub pubkey: String,
    /// Skill or plugin. Absent for Phase-5b bundles (interpret as Skill).
    /// Added in Phase 5f.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<BundleKind>,
}

/// The subset that gets signed. Never serialize any other struct with the
/// same field names — that would be signature-malleable.
///
/// `kind` is serialized only when it's `Some(_)`, so Phase-5b bundles that
/// didn't carry a kind still verify byte-identically post-upgrade.
#[derive(Serialize)]
struct Signed<'a> {
    manifest: &'a serde_yaml::Value,
    files:    &'a BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind:     Option<BundleKind>,
}

pub fn keygen(out: &Path) -> Result<()> {
    use rand_core::OsRng;
    let sk = SigningKey::generate(&mut OsRng);
    let pk = sk.verifying_key();

    let sk_b64 = base64::engine::general_purpose::STANDARD.encode(sk.to_bytes());
    let pk_b64 = base64::engine::general_purpose::STANDARD.encode(pk.to_bytes());

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    std::fs::write(out, format!("{sk_b64}\n"))
        .with_context(|| format!("writing private key to {}", out.display()))?;

    let pub_path = out.with_extension("pub");
    std::fs::write(&pub_path, format!("{pk_b64}\n"))
        .with_context(|| format!("writing public key to {}", pub_path.display()))?;

    println!("wrote private key: {}", out.display());
    println!("wrote public  key: {}", pub_path.display());
    println!();
    println!("Distribute the .pub half; keep the private key secret.");
    Ok(())
}

pub fn sign_bundle(skill_dir: &Path, out: &Path, priv_key_path: &Path, kind: BundleKind) -> Result<()> {
    if !skill_dir.is_dir() {
        bail!("source directory not found: {}", skill_dir.display());
    }
    let sk = load_signing_key(priv_key_path)?;
    let pk = sk.verifying_key();

    let (manifest, files) = match kind {
        BundleKind::Skill  => collect_skill_files(skill_dir)?,
        BundleKind::Plugin => collect_plugin_files(skill_dir)?,
    };

    // For plugin bundles we always embed `kind` in the signed payload.
    // Skill bundles omit `kind` so pre-Phase-5f bundles remain byte-
    // identical to newly-signed skills (preserving reproducibility).
    let signed_kind = match kind {
        BundleKind::Skill  => None,
        BundleKind::Plugin => Some(BundleKind::Plugin),
    };

    let payload = Signed { manifest: &manifest, files: &files, kind: signed_kind };
    let bytes = serde_json::to_vec(&payload).context("canonicalizing bundle for signature")?;
    let sig = sk.sign(&bytes);

    let bundle = Bundle {
        manifest,
        files,
        signature: base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        pubkey:    base64::engine::general_purpose::STANDARD.encode(pk.to_bytes()),
        kind:      signed_kind,
    };

    let json = serde_json::to_vec_pretty(&bundle).context("serializing bundle")?;
    let out = if out.extension().is_none() && !out.to_string_lossy().ends_with(BUNDLE_SUFFIX) {
        // Auto-append the canonical extension when the caller omits one.
        let mut p = out.as_os_str().to_owned();
        p.push(BUNDLE_SUFFIX);
        PathBuf::from(p)
    } else {
        out.to_path_buf()
    };
    std::fs::write(&out, json).with_context(|| format!("writing bundle {}", out.display()))?;
    let kind_str = match kind { BundleKind::Skill => "skill", BundleKind::Plugin => "plugin" };
    println!("wrote signed {kind_str} bundle: {}  ({} files, pubkey {})",
        out.display(),
        bundle.files.len(),
        &bundle.pubkey[..12.min(bundle.pubkey.len())]);
    Ok(())
}

/// Verify signature. If `expected_pubkey` is provided we also assert the
/// bundle was signed by exactly that key — protecting against a valid
/// signature from a wrong signer.
pub fn verify_bundle(bundle_path: &Path, expected_pubkey_path: Option<&Path>) -> Result<Bundle> {
    let raw = std::fs::read(bundle_path)
        .with_context(|| format!("reading bundle {}", bundle_path.display()))?;
    let bundle: Bundle = serde_json::from_slice(&raw)
        .context("bundle is not a valid JSON Bundle")?;

    // Signature check.
    let pk_bytes = base64::engine::general_purpose::STANDARD.decode(&bundle.pubkey)
        .context("bundle pubkey is not valid base64")?;
    let pk_arr: [u8; 32] = pk_bytes.as_slice().try_into()
        .map_err(|_| anyhow!("pubkey must be 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pk_arr).context("pubkey not a valid ed25519 point")?;

    let sig_bytes = base64::engine::general_purpose::STANDARD.decode(&bundle.signature)
        .context("signature is not valid base64")?;
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into()
        .map_err(|_| anyhow!("signature must be 64 bytes"))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    let payload = Signed {
        manifest: &bundle.manifest,
        files:    &bundle.files,
        kind:     bundle.kind,
    };
    let bytes = serde_json::to_vec(&payload).context("canonicalising bundle for verify")?;
    vk.verify(&bytes, &sig).context("SIGNATURE MISMATCH — bundle has been tampered with")?;

    if let Some(expected) = expected_pubkey_path {
        let expected_b64 = std::fs::read_to_string(expected)
            .with_context(|| format!("reading trusted pubkey {}", expected.display()))?;
        let expected_b64 = expected_b64.trim();
        if expected_b64 != bundle.pubkey {
            bail!(
                "bundle was signed by {} but you expected {}",
                &bundle.pubkey[..12.min(bundle.pubkey.len())],
                &expected_b64[..12.min(expected_b64.len())],
            );
        }
    }
    Ok(bundle)
}

/// Verify then materialise the bundle's files under `dest/`. Existing files
/// with the same relative path are refused unless `force = true`.
pub fn install_bundle(
    bundle_path: &Path,
    dest: &Path,
    expected_pubkey_path: Option<&Path>,
    force: bool,
) -> Result<()> {
    let bundle = verify_bundle(bundle_path, expected_pubkey_path)?;
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating dest {}", dest.display()))?;
    let mut written = 0usize;
    for (rel, b64) in &bundle.files {
        // Reject path traversal.
        if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') {
            bail!("bundle contains suspicious path: {rel}");
        }
        let out = dest.join(rel);
        if let Some(p) = out.parent() { std::fs::create_dir_all(p).ok(); }
        if out.exists() && !force {
            bail!("refusing to overwrite {} — pass --force", out.display());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64)
            .with_context(|| format!("decoding {rel}"))?;
        std::fs::write(&out, &bytes)
            .with_context(|| format!("writing {}", out.display()))?;
        written += 1;
    }
    println!("installed {} files under {}", written, dest.display());
    Ok(())
}

// ---------- helpers ----------

fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let b64 = std::fs::read_to_string(path)
        .with_context(|| format!("reading private key {}", path.display()))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())
        .context("private key is not valid base64")?;
    let arr: [u8; 32] = bytes.as_slice().try_into()
        .map_err(|_| anyhow!("private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Walk `dir` and return (manifest, files):
///   * `manifest` = YAML frontmatter of the first `.md` file whose header
///     starts with `---`. Empty map if none found.
///   * `files`    = { "rel/path" → base64(bytes) } for every regular file.
fn collect_skill_files(dir: &Path) -> Result<(serde_yaml::Value, BTreeMap<String, String>)> {
    let mut files = BTreeMap::new();
    let mut manifest = serde_yaml::Value::Mapping(Default::default());
    let mut found_manifest = false;

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        if !entry.file_type().is_file() { continue; }

        let rel = entry.path().strip_prefix(dir)
            .with_context(|| format!("stripping prefix {}", dir.display()))?
            .to_string_lossy()
            .replace('\\', "/");

        let bytes = std::fs::read(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;

        if !found_manifest
            && rel.ends_with(".md")
            && bytes.starts_with(b"---")
        {
            if let Some(m) = parse_frontmatter(&bytes) {
                manifest = m;
                found_manifest = true;
            }
        }
        files.insert(rel, base64::engine::general_purpose::STANDARD.encode(&bytes));
    }

    if files.is_empty() {
        bail!("no files in skill directory {}", dir.display());
    }
    Ok((manifest, files))
}

/// Walk `dir` and return (manifest, files) for a **plugin** bundle:
///   * `manifest` = full YAML contents of `mcp.yaml` OR `plugin.yaml` at the
///     root. It's an error if neither exists — plugins must ship a manifest.
///   * `files` = every regular file under `dir` (including the manifest
///     file itself), keyed by forward-slash relative path.
fn collect_plugin_files(dir: &Path) -> Result<(serde_yaml::Value, BTreeMap<String, String>)> {
    let mut files = BTreeMap::new();
    let mut manifest: Option<serde_yaml::Value> = None;

    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        if !entry.file_type().is_file() { continue; }

        let rel = entry.path().strip_prefix(dir)
            .with_context(|| format!("stripping prefix {}", dir.display()))?
            .to_string_lossy()
            .replace('\\', "/");

        let bytes = std::fs::read(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;

        // Manifest = the *first* mcp.yaml or plugin.yaml we hit at the root.
        // We prefer mcp.yaml when both exist since that's the forge-mcp
        // convention (crates/forge-mcp/src/config.rs).
        if manifest.is_none() && (rel == "mcp.yaml" || rel == "plugin.yaml") {
            let parsed: serde_yaml::Value = serde_yaml::from_slice(&bytes)
                .with_context(|| format!("parsing manifest {rel} as YAML"))?;
            manifest = Some(parsed);
        }

        files.insert(rel, base64::engine::general_purpose::STANDARD.encode(&bytes));
    }

    let manifest = manifest.ok_or_else(|| anyhow!(
        "plugin bundle requires a top-level mcp.yaml or plugin.yaml in {}",
        dir.display()
    ))?;
    if files.is_empty() {
        bail!("no files in plugin directory {}", dir.display());
    }
    Ok((manifest, files))
}

fn parse_frontmatter(bytes: &[u8]) -> Option<serde_yaml::Value> {
    let s = std::str::from_utf8(bytes).ok()?;
    let rest = s.strip_prefix("---")?.trim_start_matches('\n');
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];
    serde_yaml::from_str(yaml).ok()
}
