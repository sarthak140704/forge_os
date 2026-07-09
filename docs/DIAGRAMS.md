# Forge OS — Architecture Diagrams

Visual companion to `ARCHITECTURE.md`, closing agent.txt *Deliverables* items
21–27 (state machines, sequence diagrams, data-flow diagrams, dependency graph,
plugin/skill/mission lifecycles). Every diagram is grounded in the actual crate
graph and event/state enums shipped in this repo — not aspirational.

All diagrams use [Mermaid](https://mermaid.js.org/), which GitHub renders
natively in Markdown.

---

## 1. Crate dependency graph (Deliverable 24)

The workspace follows Hexagonal / Clean Architecture: `forge-domain` is the
dependency-free core; every arrow points **inward** toward it. `forge-runtime`
is the composition root that wires everything together; the three binaries
(`forge-cli`, `forge-gateway`, `forge-desktop`) sit at the outermost ring.

> Edges are the real `[dependencies]` from each `Cargo.toml`. `forge-server`'s
> reference to `forge-runtime` is a **dev-dependency** (its API smoke example)
> and is intentionally omitted so the library graph stays acyclic.

```mermaid
flowchart TD
    domain[forge-domain]:::core

    llm[forge-llm]:::core
    skills[forge-skills]:::core

    persistence[forge-persistence] --> domain
    events[forge-events] --> domain
    events --> persistence
    tools[forge-tools] --> domain
    policy[forge-policy] --> domain
    mcp[forge-mcp] --> domain
    mcp --> tools

    planner[forge-planner] --> domain
    planner --> llm
    planner --> skills

    execution[forge-execution] --> domain
    execution --> events
    execution --> persistence
    execution --> tools
    execution --> policy

    mission[forge-mission] --> domain
    mission --> events
    mission --> llm
    mission --> persistence
    mission --> planner
    mission --> execution
    mission --> skills
    mission --> tools
    mission --> policy

    server[forge-server] --> domain
    server --> events
    server --> mission
    server --> persistence
    server --> llm

    runtime[forge-runtime]:::root --> domain
    runtime --> events
    runtime --> persistence
    runtime --> llm
    runtime --> tools
    runtime --> policy
    runtime --> planner
    runtime --> execution
    runtime --> mission
    runtime --> skills
    runtime --> mcp
    runtime --> server

    cli[[forge-cli]]:::app --> runtime
    cli --> server
    cli --> domain
    desktop[[forge-desktop]]:::app --> runtime
    desktop --> mission
    desktop --> skills
    gateway[[forge-gateway]]:::app

    classDef core fill:#1f2a4d,stroke:#7c8cf8,color:#fff;
    classDef root fill:#3a2a4d,stroke:#a78bfa,color:#fff;
    classDef app fill:#123,stroke:#34d399,color:#fff;
```

`forge-gateway` has no internal crate deps by design — it is a thin webhook
receiver (Slack/Discord/Telegram) that forwards to the API over HTTP, keeping
the messaging surface decoupled from the runtime.

---

## 2. Mission lifecycle (Deliverable 27, state machine)

`MissionStatus` transitions as the engine plans, executes, and finalises a
mission. Terminal states are `completed`, `failed`, and `cancelled`; a
completed/failed mission can be **extended** with a follow-up prompt, which
re-enters planning.

```mermaid
stateDiagram-v2
    [*] --> planning : mission_created
    planning --> ready : mission_planning_completed
    planning --> failed : mission_planning_failed
    ready --> running : first goal dispatched
    running --> paused : pause requested
    paused --> running : resume
    running --> completed : all goals completed/skipped
    running --> failed : unrecoverable goal failure
    running --> cancelled : mission_cancel_requested
    ready --> cancelled : mission_cancel_requested
    paused --> cancelled : mission_cancel_requested
    completed --> planning : extend_mission (follow-up)
    failed --> planning : extend_mission (follow-up)
    cancelled --> planning : extend_mission (follow-up)
    completed --> [*]
    failed --> [*]
    cancelled --> [*]
```

## 3. Goal lifecycle (Deliverable 26, state machine)

Each goal in the DAG advances independently. A goal becomes `ready` only once
all its `depends_on` predecessors are terminal. Replanning can inject new
`pending` goals mid-flight.

```mermaid
stateDiagram-v2
    [*] --> pending : goal_created
    pending --> ready : dependencies satisfied
    ready --> running : task dispatched
    running --> completed : goal_status_changed(completed)
    running --> failed : goal_status_changed(failed)
    pending --> skipped : upstream failed / pruned by replan
    ready --> skipped : upstream failed / pruned by replan
    failed --> ready : retry (within retry budget)
    completed --> [*]
    skipped --> [*]
    failed --> [*]
```

---

## 4. Plan → execute → reflect (Deliverable 22, sequence)

The end-to-end path of a single mission through the runtime. The LLM appears
only as a bounded reasoning component behind the planner — every other box is
deterministic runtime code (agent.txt *Core Philosophy*).

```mermaid
sequenceDiagram
    actor User
    participant UI as Desktop / API / CLI
    participant ME as Mission Engine
    participant PL as Planner
    participant LLM as LLM Router
    participant PE as Policy Engine
    participant EX as Execution Engine
    participant TOOL as Tool Runtime
    participant BUS as Event Bus
    participant DB as Persistence

    User->>UI: create mission(title, description)
    UI->>ME: submit
    ME->>BUS: MissionCreated
    ME->>PL: plan(mission)
    PL->>LLM: decompose into goal DAG
    LLM-->>PL: goals + tasks (JSON)
    PL->>BUS: GoalCreated * N, MissionPlanningCompleted
    loop each ready goal
        ME->>EX: execute(goal.tasks)
        EX->>PE: evaluate(tool, input)
        alt denied
            PE-->>EX: Denied(reason)
            EX->>BUS: PolicyDenied
        else allowed / approval granted
            PE-->>EX: Allow
            EX->>TOOL: invoke(tool, input)
            TOOL-->>EX: result
            EX->>BUS: TaskCompleted (+ CheckpointCreated on mutation)
        end
    end
    ME->>PL: reflect(outcomes)
    PL->>LLM: extract lessons + skill proposals
    LLM-->>PL: reflection
    PL->>BUS: MissionReflectionCompleted, SkillProposalWritten
    ME->>BUS: MissionCostSummary
    ME->>DB: persist final state
    BUS-->>UI: live event stream (replayable)
```

---

## 5. Event-sourcing data flow (Deliverable 23)

Every state change is an **append-only event**. The persisted event log is the
source of truth; read models (mission views, DAG, cost) are projections that can
be rebuilt by replaying the log. This is what makes the runtime resumable,
auditable, and replayable (agent.txt *Event Bus*, *Persistence*).

```mermaid
flowchart LR
    subgraph Producers
        ME[Mission Engine]
        EX[Execution Engine]
        PL[Planner / Reflector]
        POL[Policy Engine]
        MCP[MCP / Plugins]
    end

    ME & EX & PL & POL & MCP -->|typed events| BUS{{Event Bus<br/>broadcast}}

    BUS --> STORE[(Event Store<br/>SQLite → Postgres)]
    BUS --> LIVE[Live subscribers<br/>Desktop SSE / API stream]

    STORE -->|replay| PROJ[Projections]
    PROJ --> MV[Mission / Goal views]
    PROJ --> DAG[React Flow DAG]
    PROJ --> COST[Cost & token analytics]
    PROJ --> AUDIT[Audit / SOC2 export]

    STORE -->|on restart| REHYDRATE[Rehydrate in-flight missions]
    REHYDRATE --> ME
```

---

## 6. Skill lifecycle (Deliverable 26)

Skills are executable procedural knowledge that **evolve** through the learning
loop. Nothing is overwritten: promotion is content-addressed and every version
is retained, so any change is reversible (agent.txt *Self-Improvement*).

```mermaid
stateDiagram-v2
    [*] --> proposed : reflection emits SkillProposalWritten
    proposed --> validating : review / auto-curate
    validating --> proposed : hard-check failure (stays in proposed/)
    validating --> active : validation ok + approve → SkillPromoted
    active --> active : new version promoted (content-addressed history)
    active --> archived : Curator dedupe/merge/retire → SkillRetired
    archived --> active : restore prior version → SkillRolledBack
    active --> [*]
```

Hard validation checks (deterministic, no LLM): `parses`, `body_length ≥ 40`,
`has_trigger`, `tools_declared`, `tools_resolvable`. See
`crates/forge-skills/src/validate.rs`; the seed library is gated by
`crates/forge-skills/tests/seed_skills.rs`.

---

## 7. Plugin / MCP lifecycle (Deliverable 25)

Plugins are first-class runtime modules. The preferred surface is MCP: each
configured server is spawned, health-checked, and its tools are registered into
the Tool Runtime under an `mcp:<server>` namespace. Native plugins follow the
same lifecycle.

```mermaid
stateDiagram-v2
    [*] --> configured : mcp.yaml entry
    configured --> starting : runtime boot / hot-load
    starting --> failed : spawn/handshake error → McpServerFailed
    starting --> ready : tools listed → McpServerStarted
    ready --> serving : tool invoked → McpToolInvoked (via Policy Engine)
    serving --> ready : result returned
    ready --> failed : health check fails
    failed --> starting : retry / hot-reload
    ready --> stopped : shutdown
    stopped --> [*]
```

Every plugin tool call still passes through the Policy Engine before execution,
so sandboxing and least-privilege apply uniformly to native and MCP tools.

---

## Regenerating / validating

These diagrams are plain Mermaid in Markdown — no build step. Paste any block
into the [Mermaid Live Editor](https://mermaid.live) to validate syntax, or rely
on GitHub's native rendering. When the crate graph or an event/state enum
changes, update the corresponding block here and in `ARCHITECTURE.md`.
