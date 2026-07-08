//! In-memory catalog + `select_for_mission` scoring.
//!
//! Scoring is deliberately simple: count keyword hits in the mission's title
//! + description, weighted 2x for the title. Later a semantic reranker can
//! slot in without changing the surface. What matters for phase-2 is that the
//! planner gets a *ranked list* of relevant playbooks, not a random pile.

use crate::{Skill, SkillTrigger};

pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new(skills: Vec<Skill>) -> Self { Self { skills } }
    pub fn empty() -> Self { Self { skills: Vec::new() } }
    pub fn len(&self) -> usize { self.skills.len() }
    pub fn is_empty(&self) -> bool { self.skills.is_empty() }
    pub fn all(&self) -> &[Skill] { &self.skills }

    /// Set of active skill names — used for dedup by the reflector/learning loop.
    pub fn names(&self) -> std::collections::HashSet<String> {
        self.skills.iter().map(|s| s.front.name.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.front.name == name)
    }

    /// Rank skills against a mission. Returns matches with `score > 0`,
    /// sorted descending. The caller decides how many to inject.
    pub fn select_for_mission(&self, title: &str, description: &str) -> Vec<SkillMatch<'_>> {
        let title_l = title.to_lowercase();
        let desc_l  = description.to_lowercase();

        let mut out: Vec<SkillMatch<'_>> = self.skills.iter()
            .filter_map(|s| {
                let score = trigger_score(&s.front.triggers, &title_l, &desc_l);
                if score > 0 { Some(SkillMatch { skill: s, score }) } else { None }
            })
            .collect();
        out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.skill.front.name.cmp(&b.skill.front.name)));
        out
    }
}

pub struct SkillMatch<'a> {
    pub skill: &'a Skill,
    pub score: u32,
}

fn trigger_score(trg: &SkillTrigger, title_l: &str, desc_l: &str) -> u32 {
    let mut score = 0u32;
    for kw in &trg.keywords {
        let kw_l = kw.to_lowercase();
        if kw_l.is_empty() { continue; }
        if word_present(title_l, &kw_l) { score += 2; }
        if word_present(desc_l,  &kw_l) { score += 1; }
    }
    score
}

/// Whole-word-ish match: keyword surrounded by non-alphanumeric on both sides
/// (or start/end of string). Guards against `rust` matching `trust`.
fn word_present(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(idx) = haystack[start..].find(needle) {
        let abs = start + idx;
        let before_ok = abs == 0 || !haystack.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let after_end = abs + needle.len();
        let after_ok = after_end == haystack.len()
            || !haystack.as_bytes()[after_end].is_ascii_alphanumeric();
        if before_ok && after_ok { return true; }
        start = abs + needle.len();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_skill};

    fn skill(name: &str, keywords: &[&str]) -> Skill {
        let kw = keywords.iter().map(|s| format!("      - {s}\n")).collect::<String>();
        let src = format!(
            "---\n\
             name: {name}\n\
             version: 1.0.0\n\
             description: d\n\
             triggers:\n  keywords:\n{kw}\
             ---\nbody"
        );
        parse_skill(&src).unwrap()
    }

    #[test]
    fn selects_matching_skill() {
        let reg = SkillRegistry::new(vec![
            skill("rust-crate", &["rust", "cargo"]),
            skill("node-project", &["node", "npm"]),
        ]);
        let hits = reg.select_for_mission("Build a Rust CLI", "Use cargo to build");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].skill.front.name, "rust-crate");
        // title=2 (rust) + desc=1 (cargo) = 3
        assert_eq!(hits[0].score, 3);
    }

    #[test]
    fn ranks_by_score() {
        let reg = SkillRegistry::new(vec![
            skill("a", &["deploy"]),                // 1 in title
            skill("b", &["deploy", "kubernetes"]),  // 1 in title + 1 in desc
        ]);
        let hits = reg.select_for_mission("Deploy to prod", "Run kubernetes apply");
        assert_eq!(hits[0].skill.front.name, "b");
        assert_eq!(hits[1].skill.front.name, "a");
    }

    #[test]
    fn word_boundary_avoids_false_positives() {
        assert!(word_present("build with rust", "rust"));
        assert!(!word_present("i trust the compiler", "rust"));
        assert!(word_present("cargo-based project", "cargo"));
        assert!(!word_present("supercargo container", "cargo"));
    }

    #[test]
    fn returns_empty_when_nothing_matches() {
        let reg = SkillRegistry::new(vec![ skill("k8s", &["kubernetes"]) ]);
        assert!(reg.select_for_mission("Write a poem", "About the sea").is_empty());
    }

    #[test]
    fn missing_triggers_never_match() {
        let src = "---\nname: silent\nversion: 1.0.0\ndescription: d\n---\nbody";
        let reg = SkillRegistry::new(vec![parse_skill(src).unwrap()]);
        assert!(reg.select_for_mission("anything", "anything at all").is_empty());
    }
}
