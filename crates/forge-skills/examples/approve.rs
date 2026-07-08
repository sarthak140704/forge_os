//! Approve or reject a pending skill proposal.
//!
//! Usage:
//!   cargo run -p forge-skills --example approve -- <filename>
//!   cargo run -p forge-skills --example approve -- --reject <filename>

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (reject, filename) = match args.as_slice() {
        [f]                             => (false, f.clone()),
        [flag, f] if flag == "--reject" => (true,  f.clone()),
        _ => {
            eprintln!("usage: approve [--reject] <filename>");
            std::process::exit(2);
        }
    };

    let appdata = std::env::var("APPDATA")?;
    let skills_root: PathBuf = [&appdata, "com.sarthak.forgeos", "skills"].iter().collect();

    if reject {
        forge_skills::proposal::reject_proposal(&skills_root, &filename)?;
        println!("rejected: {filename}");
    } else {
        let dst = forge_skills::proposal::approve_proposal(&skills_root, &filename)?;
        println!("approved -> {}", dst.display());
    }
    Ok(())
}
