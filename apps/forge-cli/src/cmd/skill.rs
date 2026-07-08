use crate::bundle;
use anyhow::Result;
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Op {
    /// Package a skill directory into a signed .forgebundle.json file.
    Bundle {
        /// Directory holding the skill's `.md` + supporting files.
        dir: PathBuf,
        /// Output path. `.forgebundle.json` will be auto-appended when missing.
        #[arg(long)]
        out: PathBuf,
        /// Private key file (base64 ed25519 secret, one line). Use `forge keygen`.
        #[arg(long)]
        key: PathBuf,
    },
    /// Verify the signature on a bundle. Exit 0 = trusted, non-zero = bad.
    Verify {
        bundle: PathBuf,
        /// Optional: expected public key file. If given, we also check the
        /// bundle was signed by exactly this key.
        #[arg(long)]
        pubkey: Option<PathBuf>,
    },
    /// Verify then unpack a bundle under a destination directory.
    Install {
        bundle: PathBuf,
        /// Where to unpack — defaults to `./active/`.
        #[arg(long, default_value = "./active")]
        dest: PathBuf,
        /// Expected public key. If unset, any signed bundle is accepted.
        #[arg(long)]
        pubkey: Option<PathBuf>,
        /// Overwrite any pre-existing files.
        #[arg(long)]
        force: bool,
    },
}

pub async fn dispatch(json: bool, op: Op) -> Result<()> {
    let _ = json; // no JSON output path here; all outputs are short prose
    match op {
        Op::Bundle { dir, out, key } => bundle::sign_bundle(&dir, &out, &key),
        Op::Verify { bundle: b, pubkey } => {
            let _ = bundle::verify_bundle(&b, pubkey.as_deref())?;
            println!("signature OK");
            Ok(())
        }
        Op::Install { bundle: b, dest, pubkey, force } => {
            bundle::install_bundle(&b, &dest, pubkey.as_deref(), force)
        }
    }
}
