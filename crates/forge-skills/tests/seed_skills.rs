//! Regression gate: every skill shipped in `config/skills/active/` must pass
//! the hard validation checks against the runtime's built-in tool set.
//!
//! These skills are embedded verbatim into the desktop bootstrap
//! (`SEED_SKILLS` in `apps/forge-desktop/src-tauri/src/lib.rs`). If someone
//! adds a seed skill with an unresolvable tool, an empty body, or no trigger,
//! this test fails in the portable workspace run *before* it can ship.
//!
//! Paths are relative to this file: `crates/forge-skills/tests/` → repo root
//! is `../../../`.

use forge_skills::{validate_bytes, ValidatorContext};

/// The tool ids the local runtime registers today (see
/// `crates/forge-tools/src/builtins.rs::all`). Seed skills may only reference
/// these, or an `mcp:`-prefixed tool.
fn known_tools() -> Vec<String> {
    ["fs.read", "fs.write", "fs.mkdir", "fs.list", "code.search", "shell.run"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// (filename, embedded bytes) for every active seed skill. Keep this list in
/// sync with `SEED_SKILLS` in the desktop bootstrap.
const SEED_SKILLS: &[(&str, &str)] = &[
    ("rust-crate.md",         include_str!("../../../config/skills/active/rust-crate.md")),
    ("node-project.md",       include_str!("../../../config/skills/active/node-project.md")),
    ("python-project.md",     include_str!("../../../config/skills/active/python-project.md")),
    ("git-repo.md",           include_str!("../../../config/skills/active/git-repo.md")),
    ("docker.md",             include_str!("../../../config/skills/active/docker.md")),
    ("kubernetes.md",         include_str!("../../../config/skills/active/kubernetes.md")),
    ("terraform.md",          include_str!("../../../config/skills/active/terraform.md")),
    ("github-cli.md",         include_str!("../../../config/skills/active/github-cli.md")),
    ("aws.md",                include_str!("../../../config/skills/active/aws.md")),
    ("postgres.md",           include_str!("../../../config/skills/active/postgres.md")),
    ("redis.md",              include_str!("../../../config/skills/active/redis.md")),
    ("go-module.md",          include_str!("../../../config/skills/active/go-module.md")),
    ("react-app.md",          include_str!("../../../config/skills/active/react-app.md")),
    ("security-review.md",    include_str!("../../../config/skills/active/security-review.md")),
    ("code-review.md",        include_str!("../../../config/skills/active/code-review.md")),
    ("documentation.md",      include_str!("../../../config/skills/active/documentation.md")),
    ("refactoring.md",        include_str!("../../../config/skills/active/refactoring.md")),
    ("database-migration.md", include_str!("../../../config/skills/active/database-migration.md")),
    ("incident-response.md",  include_str!("../../../config/skills/active/incident-response.md")),
    ("release-management.md", include_str!("../../../config/skills/active/release-management.md")),
];

#[test]
fn every_seed_skill_passes_hard_validation() {
    let ctx = ValidatorContext { known_tools: known_tools(), active_skills: Vec::new() };
    let mut failures = Vec::new();
    for (name, body) in SEED_SKILLS {
        let report = validate_bytes(body, &ctx);
        if !report.ok {
            failures.push(format!("{name}: hard failures {:?}", report.hard_failures()));
        }
    }
    assert!(
        failures.is_empty(),
        "seed skills failed hard validation:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn seed_skill_names_are_unique() {
    let mut names: Vec<String> = SEED_SKILLS
        .iter()
        .map(|(_, body)| forge_skills::parse_skill(body).expect("seed skill parses").front.name)
        .collect();
    let total = names.len();
    names.sort();
    names.dedup();
    assert_eq!(total, names.len(), "duplicate skill `name` in seed set: {names:?}");
}

#[test]
fn seed_skill_count_is_stable() {
    // Guards against an accidental deletion. Bump intentionally when adding.
    assert_eq!(
        SEED_SKILLS.len(),
        20,
        "seed skill count changed — update SEED_SKILLS in both the desktop bootstrap and this test"
    );
}
