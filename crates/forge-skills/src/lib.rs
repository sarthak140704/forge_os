//! Skill Runtime.
//!
//! A **Skill** is executable procedural knowledge — the "how" behind a class
//! of missions. Concretely it's a Markdown file with a YAML front-matter head
//! (metadata + declared tools + triggers) and a Markdown body containing the
//! prose procedure the planner will lean on.
//!
//! The file format is intentionally aligned with the community
//! [agentskills.io](https://agentskills.io) `SKILL.md` convention so skills
//! authored for other agents can be reused with minimal reshaping.
//!
//! # Design
//!
//! - `Skill` — parsed representation (metadata + body).
//! - `SkillLoader` — trait; ships with `FilesystemSkillLoader` that walks a
//!   directory and loads every `*.md`, deduping by name (highest semver wins).
//! - `SkillRegistry` — in-memory catalog + `select_for_mission` that scores
//!   skills against a mission's title/description using their `triggers`.
//! - `ProposalWriter` — writes reflection-suggested skills to
//!   `skills_root/proposed/…` for human review. The active loader ignores
//!   `proposed/` and `archived/` by default, so proposals never take effect
//!   until approved.
//!
//! # Nothing here touches the LLM
//!
//! The registry is pure data + string matching. The planner injects selected
//! skills into its prompt; the reflector emits new proposals. All I/O is
//! synchronous, tiny, and fine to hold in memory.

pub mod format;
pub mod loader;
pub mod registry;
pub mod proposal;
pub mod validate;
pub mod versions;

pub use format::{
    parse_skill, Skill, SkillFrontMatter, SkillIo, SkillStatus, SkillTrigger,
};
pub use loader::{FilesystemSkillLoader, SkillLoader};
pub use proposal::{
    approve_proposal, approve_proposal_detail, list_proposals, reject_proposal,
    restore_from_bytes, retire_active_skill, ProposalWriter, SuggestedSkill,
};
pub use registry::{SkillMatch, SkillRegistry};
pub use validate::{
    validate_bytes, validate_file, validate_skill,
    ActiveSkillSummary, Severity, ValidationCheck, ValidationReport, ValidatorContext,
};
pub use versions::{hash_file, SkillVersionStore};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("skill `{name}` is malformed: {reason}")]
    Malformed { name: String, reason: String },
    #[error("skill file has no YAML front-matter (expected leading `---` fence)")]
    MissingFrontMatter,
    #[error("skill front-matter YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("skill missing required field `{0}`")]
    MissingField(&'static str),
}
