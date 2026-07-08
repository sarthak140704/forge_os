use crate::{DomainError, GoalId, MissionId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl GoalStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, GoalStatus::Completed | GoalStatus::Failed | GoalStatus::Skipped)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub mission_id: MissionId,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    pub depends_on: Vec<GoalId>,
    pub confidence: f32,
    pub priority: i32,
    pub retries_remaining: u8,
    pub tasks: Vec<TaskId>,
}

impl Goal {
    pub fn new(
        mission_id: MissionId,
        title: impl Into<String>,
        description: impl Into<String>,
        depends_on: Vec<GoalId>,
    ) -> Self {
        Self {
            id: GoalId::new(),
            mission_id,
            title: title.into(),
            description: description.into(),
            status: GoalStatus::Pending,
            depends_on,
            confidence: 0.5,
            priority: 0,
            retries_remaining: 2,
            tasks: Vec::new(),
        }
    }
}

/// A node in the mission's goal DAG, used by the execution engine.
#[derive(Clone, Debug)]
pub struct GoalNode {
    pub goal: Goal,
    pub blocks: Vec<GoalId>,   // reverse edges — filled in during DAG build
}

/// Validate that the goal set forms a DAG (no cycles, no dangling deps) and
/// return a topologically usable structure keyed by id.
pub fn build_dag(
    mission_id: MissionId,
    goals: Vec<Goal>,
) -> Result<HashMap<GoalId, GoalNode>, DomainError> {
    let ids: HashSet<GoalId> = goals.iter().map(|g| g.id).collect();
    for g in &goals {
        for dep in &g.depends_on {
            if !ids.contains(dep) {
                return Err(DomainError::UnknownGoal(*dep));
            }
        }
    }

    // Kahn's algorithm to detect cycles.
    let mut indegree: HashMap<GoalId, usize> = goals.iter().map(|g| (g.id, g.depends_on.len())).collect();
    let mut queue: Vec<GoalId> = indegree.iter().filter(|(_, n)| **n == 0).map(|(k, _)| *k).collect();
    let mut visited = 0usize;
    let mut reverse: HashMap<GoalId, Vec<GoalId>> = HashMap::new();
    for g in &goals {
        for dep in &g.depends_on {
            reverse.entry(*dep).or_default().push(g.id);
        }
    }
    while let Some(id) = queue.pop() {
        visited += 1;
        if let Some(children) = reverse.get(&id) {
            for c in children {
                let entry = indegree.get_mut(c).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    queue.push(*c);
                }
            }
        }
    }
    if visited != goals.len() {
        return Err(DomainError::DependencyCycle { mission_id });
    }

    let mut nodes: HashMap<GoalId, GoalNode> = goals
        .into_iter()
        .map(|g| (g.id, GoalNode { goal: g, blocks: Vec::new() }))
        .collect();
    for (parent, children) in reverse {
        if let Some(node) = nodes.get_mut(&parent) {
            node.blocks = children;
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cycle() {
        let mid = MissionId::new();
        let a = Goal::new(mid, "a", "", vec![]);
        let mut b = Goal::new(mid, "b", "", vec![a.id]);
        let mut c = Goal::new(mid, "c", "", vec![b.id]);
        // introduce cycle a -> depends on c
        let mut a2 = a.clone();
        a2.depends_on = vec![c.id];
        b.depends_on = vec![a2.id];
        c.depends_on = vec![b.id];
        let err = build_dag(mid, vec![a2, b, c]).unwrap_err();
        matches!(err, DomainError::DependencyCycle { .. });
    }

    #[test]
    fn accepts_valid_dag() {
        let mid = MissionId::new();
        let a = Goal::new(mid, "a", "", vec![]);
        let b = Goal::new(mid, "b", "", vec![a.id]);
        let c = Goal::new(mid, "c", "", vec![a.id, b.id]);
        let dag = build_dag(mid, vec![a.clone(), b.clone(), c.clone()]).unwrap();
        assert_eq!(dag.len(), 3);
        assert_eq!(dag.get(&a.id).unwrap().blocks.len(), 2);
    }
}
