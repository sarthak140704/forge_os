# Forge OS — Acceptance Test Suite (UI + Backend)

Every test case verifies **both** what the UI shows **and** what the backend actually did:
- **UI:** what to click, what event/badge/node to look for.
- **Backend:** SQL/file/log checks that prove the state change is real — not just a visual hint.
- **Runnable:** headless equivalent that you can run instead of clicking through the UI.

> **Pass** = every "Backend proof" bullet resolves as described. UI-only pass is not enough.

## Prerequisites

- Windows + PowerShell.
- Python 3.10+ (for the inspector below — needed for the `match` statement).
- Groq (or OpenAI/Ollama) key: set as `$env:GROQ_API_KEY` or in the Settings → Secrets keyring.
- App started: `cd apps\forge-desktop; npx @tauri-apps/cli@2 dev` (leave it running in a separate terminal).
- Data dir: `%APPDATA%\com.sarthak.forgeos\`
  - DB:        `%APPDATA%\com.sarthak.forgeos\forge.sqlite`
  - Workspace: `%APPDATA%\com.sarthak.forgeos\workspace\`
  - Skills:    `%APPDATA%\com.sarthak.forgeos\skills\` (`active/`, `proposed/`, `archived/`, `history/`)
  - Checkpoints: `%APPDATA%\com.sarthak.forgeos\checkpoints\` (bare git repo)

## Backend inspector

`scripts/forge-inspect.py` reads the live SQLite DB read-only (safe to run while the app is up).

```powershell
cd C:\Users\t-sarverma\Projects\forge-os

# One-line row counts across all tables
python scripts\forge-inspect.py counts

# Missions dashboard (status + goal counts)
python scripts\forge-inspect.py missions

# Full detail for one mission (goals + tasks + reflections + event summary)
python scripts\forge-inspect.py mission <prefix-or-full-mission-id>

# Last N events, or events of a given type, or all events for one mission
python scripts\forge-inspect.py events -n 30
python scripts\forge-inspect.py events-type skill_promoted
python scripts\forge-inspect.py events-for <mission_id>

# Live tail (Ctrl-C to stop) — great for watching a mission run in real time
python scripts\forge-inspect.py tail -n 5

# Skills (active + full history)
python scripts\forge-inspect.py skills
python scripts\forge-inspect.py skills-history [name]

# Reflections
python scripts\forge-inspect.py reflections [mission_id]

# Escape hatch — arbitrary read-only SQL
python scripts\forge-inspect.py query "SELECT event_type, COUNT(*) FROM events GROUP BY event_type ORDER BY 2 DESC"
```

Every test case below cites the exact inspector command that proves the backend state change.

---

# Phase 1 — Core orchestration

## TC-P1-01 · Mission → planning → DAG → completion (full happy path)

**Prompt (paste into Create Mission)**
- Title: `Print workspace files`
- Description: `List every file in the workspace root and echo the list to the terminal.`

**UI**
- Mission auto-selected; DAG renders immediately (no need to click the list).
- DAG has ≥1 goal, each goal has tool-tagged tasks.
- Mission ends with a green "Completed" badge.

**Backend proof** — capture the mission id from the URL / DAG header, call it `$MID`:
```powershell
python scripts\forge-inspect.py mission $MID
```
The output must include:
- `status: completed`
- ≥1 goals, all with `[completed]`
- ≥1 tasks, all with `[Completed]` and non-zero `attempts`
- Event summary containing every one of:
  - `mission_created`  ×1
  - `mission_planning_started`  ×1
  - `skills_selected`  ×1
  - `mission_planning_completed`  ×1
  - `goal_created`  ×N (where N = goals)
  - `task_created`  ×M
  - `tool_invoked` ≥1
  - `llm_requested` ≥1  and `llm_responded` ≥1  (planner call at minimum)
  - `mission_status_changed`  ≥3 (Draft→Planning→Ready→Running→Completed)
  - `mission_cost_summary`  ×1  (fires at terminal transition)

Also confirm event durability by dumping the raw store:
```powershell
python scripts\forge-inspect.py events-for $MID | Select-Object -First 20
```

**Runnable alternative** — none (this is the manual sanity check). But if you don't want to click, run TC-X-10 (full unit + integration regression) to prove the core loop compiles + passes tests.

---

## TC-P1-02 · Cancel a running mission (cooperative cancellation)

**Prompt**
- Title: `Slow shell`
- Description: `Run "cmd /c timeout 25" and then echo done.`

**UI**
- Within 3 s of clicking Run, hit **Cancel** in the DAG header.
- Mission badge flips to grey **Cancelled** (not red "Failed").
- Any in-flight task node turns grey.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-for $MID
```
Must contain, in order:
- `mission_cancel_requested`
- Zero or more `task_failed` **with `error: "cancelled"`** — no other error strings.
- `mission_status_changed { to: cancelled }`

And:
```powershell
python scripts\forge-inspect.py query "SELECT status FROM missions WHERE id = '$MID'"
# expect: {'status': 'cancelled'}

python scripts\forge-inspect.py query "SELECT status, error FROM tasks WHERE goal_id IN (SELECT id FROM goals WHERE mission_id = '$MID')"
# every non-completed task must be status=cancelled with no error, or status=pending
```

**Runnable alternative**
```powershell
cargo run -p forge-runtime --example checkpoints_smoke  # exercises cancellation path
```

---

## TC-P1-03 · Policy approval gate blocks destructive shell

**Prompt**
- Title: `Delete build folder`
- Description: `Run "rmdir /S /Q build" in the workspace.`

**UI**
- A task appears with yellow "PendingApproval" badge.
- Mission stays Running; DAG doesn't advance past that task.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-for $MID | Select-String "policy_approval_requested"
```
- Exactly one `policy_approval_requested` with a non-empty `rule` field.
- The event's `reason` must reference `rmdir` (matched by `deny_if_input_string_matches` in `config/policy.default.yaml`).

```powershell
python scripts\forge-inspect.py query "SELECT status FROM tasks WHERE tool = 'shell.run' AND goal_id IN (SELECT id FROM goals WHERE mission_id = '$MID')"
# expect at least one row with status='pending_approval'
```

Clean up: click **Cancel** on the mission.

---

## TC-P1-04 · Workspace escape denial (path traversal defence)

**Prompt**
- Title: `Write outside workspace`
- Description: `Create the file C:\forge-escape-test.txt with content "hi".`

**UI**
- Task ends `Failed` (red) **or** `Denied` (yellow with lock icon).

**Backend proof**
```powershell
Test-Path C:\forge-escape-test.txt
# MUST be False — no file may have been created

python scripts\forge-inspect.py events-for $MID | Select-String "policy_denied|task_failed"
```
At least one of:
- `policy_denied { reason: "...workspace_root..." }` — blocked by policy rule, OR
- `task_failed { error: "...escapes workspace root..." }` — blocked by `safe_resolve` in the tool itself (defence in depth).

---

## TC-P1-05 · Event stream is complete and ordered

**Prompt** — none (uses the mission from TC-P1-01).

**Backend proof**
```powershell
# Global monotonic seq — must have no gaps
python scripts\forge-inspect.py query "SELECT MIN(seq), MAX(seq), COUNT(*) FROM events"
# MIN=1, COUNT = MAX (autoincrement is contiguous unless deletes happened; deletes never happen)

# Every event stored is JSON-parseable and matches its type discriminator
python scripts\forge-inspect.py query "SELECT event_type, payload FROM events ORDER BY seq DESC LIMIT 5"
# each payload must have "type": "<event_type>" as the tag field
```

---

# Phase 2 — Skills + reflection + continuous re-planning

## TC-P2-01 · Seed skills load on boot

**Setup** — first boot only; already applies on your system.

**UI** — ⚙ Settings → Skills → active list shows ≥4 rows with `origin=handcrafted`.

**Backend proof**
```powershell
python scripts\forge-inspect.py skills
# expect ≥4 rows, all origin=handcrafted

Get-ChildItem "$env:APPDATA\com.sarthak.forgeos\skills\active" -Filter *.md
# same count of files on disk

# Every active row in the DB has a matching sha in the content store
python scripts\forge-inspect.py query "SELECT sha FROM skills_history WHERE retired_at IS NULL"
# for each sha, check the corresponding file exists:
Get-ChildItem "$env:APPDATA\com.sarthak.forgeos\skills\history" -Recurse -Filter *.md | Measure-Object
```

Your live system currently shows: 5 handcrafted skills (`create-rust-cli`, `git-repo`, `node-project`, `python-project`, `rust-crate`) all promoted at boot time on 2026-07-08.

---

## TC-P2-02 · Skills actually get selected and injected into the planner

**Prompt**
- Title: `Bootstrap a Rust CLI`
- Description: `Create a new Rust CLI project named "hello-cli" that prints "hello world".`

**UI** — Timeline shows a `skills_selected` chip before planning finishes.

**Backend proof — this is the key test that skills are *invoked*, not just listed**:
```powershell
# 1. skills_selected fired for this mission, with a non-empty list
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='skills_selected' AND aggregate_id='$MID'"
# payload must contain "skill_names": [...] with at least one entry
# For this prompt, expect: "create-rust-cli" and/or "rust-crate" to appear

# 2. The prompts the planner sent to the LLM include the skill body.
# We don't log full prompts (privacy), but every skills_selected event
# is followed (within seconds) by an llm_requested event for the planner.
python scripts\forge-inspect.py query "SELECT event_type, created_at FROM events WHERE aggregate_id='$MID' AND event_type IN ('skills_selected','llm_requested','mission_planning_completed') ORDER BY seq"
# ORDER MUST BE: skills_selected → llm_requested → ... → mission_planning_completed
# If skills_selected didn't precede planning, injection was bypassed.
```

Compare against a control prompt that shouldn't match any skill (e.g. `Title: "Print pi"`, `Desc: "Print the digits of pi"`) — `skill_names` should be empty (or contain only very generic skills).

---

## TC-P2-03 · Continuous re-planning kicks in

**Prompt**
- Title: `Write and verify Python`
- Description: `Create scripts/fizzbuzz.py that prints FizzBuzz for 1..15, then run it and confirm the output has 15 lines.`

**UI** — Look for `replan_requested` chips in the timeline.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-for $MID | Select-String "replan_requested|plan_revised|replan_cap_exceeded"
```
Must contain:
- ≥1 `replan_requested { iteration: N }` where 1 ≤ N ≤ 5.
- For each `replan_requested`, a paired `plan_revised { added_goals: X }` (X ≥ 0).
- **Must NOT** contain `replan_cap_exceeded` on a well-behaved run (that's the runaway-loop safety valve — `REPLAN_CAP = 5`).

```powershell
# Confirm the goal count grew as replanning added goals
python scripts\forge-inspect.py query "SELECT COUNT(*) FROM goals WHERE mission_id='$MID'"
# should equal initial_goals + sum(added_goals)
```

---

## TC-P2-04 · Reflection produces proposals + persists to disk

**Prompt** — reuse TC-P2-03 (or any mission that ran ≥2 goals successfully).

**Backend proof**
```powershell
# 1. Reflection row persisted
python scripts\forge-inspect.py reflections $MID
# must show ≥1 row with outcome in {completed, extended, failed, cancelled}

# 2. mission_reflection_completed event fired
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='mission_reflection_completed' AND aggregate_id='$MID'"
# payload has: insights_count (int), suggested_skills (list of names)

# 3. If suggested_skills was non-empty, proposal files exist on disk
Get-ChildItem "$env:APPDATA\com.sarthak.forgeos\skills\proposed" -Filter *.md | Sort-Object LastWriteTime -Descending | Select-Object -First 5
# most recent files should include ones with a name matching a suggested_skills entry

# 4. skill_proposal_written events fired, one per proposal file
python scripts\forge-inspect.py query "SELECT COUNT(*) FROM events WHERE event_type='skill_proposal_written' AND aggregate_id='$MID'"
# must equal len(suggested_skills)
```

---

## TC-P2-05 · Extend a completed mission (follow-up)

**Setup** — pick any completed mission.

**UI** — click **+ Follow-up** → type `Also list the goal count.` → **Extend**.

**Backend proof**
```powershell
python scripts\forge-inspect.py mission $MID
# STATUS must be one of {planning, ready, running, completed} — NOT the terminal it was before extend
# GOAL COUNT must be > original count (extend adds new goals, never overwrites)

python scripts\forge-inspect.py events-for $MID | Select-String "mission_status_changed"
# Must include: {from: completed|failed|cancelled, to: draft} then draft → planning → …
# This confirms the re-open transition allowed by MissionStatus::can_transition
```

---

# Phase 3 — Governance features

## TC-P3-01 · Feature-flag toggles change runtime behaviour

**Setup**
```powershell
notepad "$env:APPDATA\com.sarthak.forgeos\workspace\feature-flags.toml"
# set: episodic_recall = false
```
Restart the app.

**UI + Backend proof**
- Boot log line: `feature flags loaded materializer=... episodic_recall=false cost_summary=...`
- Run any mission. Now:
```powershell
python scripts\forge-inspect.py query "SELECT COUNT(*) FROM events WHERE event_type='episodic_recall_surfaced' AND aggregate_id='$NEW_MID'"
# must be 0 while the flag is off
```
- Set back to `true`, restart. Same query on a new mission must be ≥0 (fires if any prior mission matches keywords).

---

## TC-P3-02 · Secrets manager

**UI** — ⚙ Settings → Secrets → for `GROQ_API_KEY` click **Set/Replace** → paste key → save.

**Backend proof**
```powershell
# Keyring holds it (OS credential store)
cmdkey /list | Select-String "forge"
# expect an entry containing "forgeos" and "GROQ_API_KEY"

# Env var wins if set — verify precedence:
$env:GROQ_API_KEY = "envval"
# Restart the app; Settings → Secrets should show badge "env", and the runtime
# uses "envval", NOT the keyring value.

# On delete: cmdkey /list no longer shows the entry; Settings badge → "unset".
```

---

## TC-P3-03 · Checkpoints are real git snapshots and revert works

**Prompt**
- Title: `Small file write`
- Description: `Create scripts/hello.py with a single print("hello from forge") line.`

**UI**
- Timeline shows `checkpoint_created { sha, task_id }` after the fs.write.
- ⚙ Settings → Checkpoints shows ≥1 row.

**Backend proof — file exists**
```powershell
Get-Content "$env:APPDATA\com.sarthak.forgeos\workspace\scripts\hello.py"
# must contain: print("hello from forge")
```

**Backend proof — checkpoint is a real git snapshot**
```powershell
cd "$env:APPDATA\com.sarthak.forgeos\checkpoints"
git --git-dir="$env:APPDATA\com.sarthak.forgeos\checkpoints" --work-tree="$env:APPDATA\com.sarthak.forgeos\workspace" log --oneline -5
# expect ≥1 commit; the SHA in the checkpoint_created event must appear here

python "C:\Users\t-sarverma\Projects\forge-os\scripts\forge-inspect.py" query "SELECT payload FROM events WHERE event_type='checkpoint_created' ORDER BY seq DESC LIMIT 1"
# grab the sha; then:
git --git-dir="$env:APPDATA\com.sarthak.forgeos\checkpoints" show --stat <sha>
# must show scripts/hello.py as added
```

**Backend proof — revert actually restores workspace**
- ⚙ Settings → Checkpoints → click **Revert** on that checkpoint → confirm.
```powershell
Test-Path "$env:APPDATA\com.sarthak.forgeos\workspace\scripts\hello.py"
# expect False — reverting to the pre-write checkpoint removed the file
```

---

## TC-P3-04 · Audit-bundle export is a complete JSON dump

**UI** — ⚙ Settings → Audit → pick `C:\Users\t-sarverma\Downloads\forge-audit.json` → **Export**.

**Backend proof**
```powershell
$audit = Get-Content C:\Users\t-sarverma\Downloads\forge-audit.json | ConvertFrom-Json
$audit.PSObject.Properties.Name
# must include: missions, goals, tasks, events, reflections, skills_history

# Row counts match the live DB
python scripts\forge-inspect.py counts
# compare with:
$audit.missions.Count; $audit.goals.Count; $audit.tasks.Count; $audit.events.Count
```

---

## TC-P3-05 · Episodic recall injects prior attempts

**Setup** — first run TC-P2-03 (`fizzbuzz.py`) and let it complete. Then:

**Prompt**
- Title: `Rewrite fizzbuzz`
- Description: `Rewrite scripts/fizzbuzz.py using a match statement instead of if/elif.`

**Backend proof**
```powershell
# The recall event fires BEFORE planning, referencing the earlier mission:
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='episodic_recall_surfaced' AND aggregate_id='$NEW_MID'"
# payload has: matched_count ≥ 1, matched_mission_ids: [ <old mission id> ]

# Ordering: episodic_recall_surfaced MUST come before mission_planning_started
python scripts\forge-inspect.py query "SELECT event_type, seq FROM events WHERE aggregate_id='$NEW_MID' AND event_type IN ('episodic_recall_surfaced','mission_planning_started') ORDER BY seq"
```

**Runnable alternative**
```powershell
cargo run -p forge-runtime --example episodic_recall_smoke
# expect: PASS banner at tail
```

---

## TC-P3-06 · Per-mission cost tracking

**Prompt** — any mission that involves the LLM (e.g. TC-P1-01).

**Backend proof**
```powershell
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='mission_cost_summary' AND aggregate_id='$MID'"
# payload: {calls: >0, prompt_tokens: >0, completion_tokens: >0, total_latency_ms: >0}

# Fires exactly once, at the terminal transition (not per goal):
python scripts\forge-inspect.py query "SELECT COUNT(*) FROM events WHERE event_type='mission_cost_summary' AND aggregate_id='$MID'"
# expect: 1

# Independent per mission — buckets don't leak
python scripts\forge-inspect.py query "SELECT aggregate_id, json_extract(payload,'$.calls') FROM events WHERE event_type='mission_cost_summary' ORDER BY seq DESC LIMIT 5"
# each row is a separate mission
```

**Runnable alternative**
```powershell
cargo run -p forge-runtime --example cost_summary_smoke
```

---

# Phase 4a — Version-controlled skills

## TC-P4A-01 · New handcrafted skill picked up on next boot

**Setup**
```powershell
$path = "$env:APPDATA\com.sarthak.forgeos\skills\active\hello-skill.md"
@'
---
name: hello-skill
version: 1.0.0
keywords: [hello, greet]
tools: [fs.read]
---
# hello-skill
A handcrafted skill used purely to verify that on-disk files are picked up at boot, snapshotted into the content-addressed store, and appended as a handcrafted-origin row in skills_history.
'@ | Out-File -Encoding utf8 $path
```
Restart the app.

**Backend proof**
```powershell
python scripts\forge-inspect.py skills-history hello-skill
# expect exactly 1 row: origin=handcrafted, retired=no, version=1.0.0

# sha in the DB matches sha256 of the file
$sha_disk = [BitConverter]::ToString((New-Object System.Security.Cryptography.SHA256Managed).ComputeHash([IO.File]::ReadAllBytes($path))) -replace '-',''
python scripts\forge-inspect.py query "SELECT sha FROM skills_history WHERE name='hello-skill'"
# hex of the sha column must equal $sha_disk.ToLower()

# Content store has a copy at the sharded path
$sha = <sha from query>.ToLower()
$shard = $sha.Substring(0,3)
Test-Path "$env:APPDATA\com.sarthak.forgeos\skills\history\$shard\$sha.md"
# must be True
```

---

## TC-P4A-02 · Edit-in-place = new version row (never overwrites)

**Setup** — edit `hello-skill.md` above: bump `version: 1.1.0` and add a line to the body. Save. Restart.

**Backend proof**
```powershell
python scripts\forge-inspect.py skills-history hello-skill
# expect 2 rows now: newest (1.1.0, retired=no) and prior (1.0.0, retired=yes)

# The retired row's sha still exists in the content store — nothing was overwritten
python scripts\forge-inspect.py query "SELECT sha FROM skills_history WHERE name='hello-skill' AND retired_at IS NOT NULL"
# check the file at history/<shard>/<sha>.md still exists (byte-for-byte v1.0.0)
```

---

## TC-P4A-03 · Rollback restores prior bytes exactly

**UI** — ⚙ Settings → Skills → `hello-skill` → expand history → click **Rollback** on the 1.0.0 row → reason: `test rollback`.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_rolled_back -n 1
# payload: {name: hello-skill, from_sha: <1.1.0 sha>, to_sha: <1.0.0 sha>, reason: test rollback}

python scripts\forge-inspect.py skills-history hello-skill
# expect 3 rows now: newest origin=rollback with same sha as the original 1.0.0 row

# The active file on disk is byte-for-byte v1.0.0 again:
$active_bytes = [IO.File]::ReadAllBytes("$env:APPDATA\com.sarthak.forgeos\skills\active\hello-skill.md")
$stored_bytes = [IO.File]::ReadAllBytes("$env:APPDATA\com.sarthak.forgeos\skills\history\<shard>\<v1.0.0-sha>.md")
[System.Linq.Enumerable]::SequenceEqual($active_bytes, $stored_bytes)
# must be True
```

---

## TC-P4A-04 · Retire moves file to archived/ and marks the row

**UI** — ⚙ Settings → Skills → `hello-skill` → **Retire** → reason: `eol`.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_retired -n 1
# payload: {name: hello-skill, sha: ..., reason: eol}

python scripts\forge-inspect.py query "SELECT * FROM skills_history WHERE name='hello-skill' AND retired_at IS NULL"
# expect 0 rows — no active row remains

Test-Path "$env:APPDATA\com.sarthak.forgeos\skills\active\hello-skill.md"
# must be False

Get-ChildItem "$env:APPDATA\com.sarthak.forgeos\skills\archived" | Where-Object { $_.Name -like "*hello-skill*" }
# must show ≥1 file (all prior copies are archived, name-prefixed to avoid collisions)
```

---

## TC-P4A-05 · Curator finds near-duplicates

**Setup** — have two skills active whose names have Jaro-Winkler similarity ≥ 0.90 (e.g. `hello-skill` and `hello-skil`).

**UI** — ⚙ Settings → Skills → **Run curator**. Suggestions panel populates.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_curation_suggested -n 5
# ≥1 event with kind=Duplicate and evidence mentioning both names + similarity ≥ 0.90
```

---

## TC-P4A-06 · Runnable · full 4a smoke (no UI)

```powershell
cd C:\Users\t-sarverma\Projects\forge-os
cargo run -p forge-runtime --example skill_versioning_smoke
# expect: PASS: skill versioning + curator verified end-to-end
```
6 scenarios covered: promote v1 → promote v2 → rollback to v1 → history rows correct → retire → curator finds duplicate.

---

# Phase 4b — Validation gate + AutoPromoter

## TC-P4B-01 · Validator blocks a bad proposal (hard-fail)

**Setup**
```powershell
$path = "$env:APPDATA\com.sarthak.forgeos\skills\proposed\bad-proposal.md"
@'
---
name: bad-skill
version: 0.1.0
keywords: []
tools: [nonexistent.tool]
---
short body
'@ | Out-File -Encoding utf8 $path
```

**UI** — ⚙ Settings → Skills → Proposals → find `bad-proposal.md` → click **Validate**.
- Red badges for `body_length`, `has_trigger`, `tools_resolvable`.
- **Approve** button is disabled.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_validation_failed -n 1
# payload: {filename: bad-proposal.md, name: bad-skill, failed_checks: ["body_length","has_trigger","tools_resolvable"]}

# The file must NOT have moved to active/ or archived/
Test-Path $path
# must be True — still in proposed/

python scripts\forge-inspect.py query "SELECT * FROM skills_history WHERE name='bad-skill'"
# expect 0 rows — no promotion happened
```

---

## TC-P4B-02 · Validator lets a good proposal through

**Setup**
```powershell
$path = "$env:APPDATA\com.sarthak.forgeos\skills\proposed\good-proposal.md"
@'
---
name: good-skill
version: 0.1.0
keywords: [fmt, format]
tools: [fs.read, fs.write]
---
# good-skill
A well-formed proposal used purely to verify that the validation gate greenlights it and the approve button becomes clickable in the UI panel.
'@ | Out-File -Encoding utf8 $path
```

**UI** — ⚙ Settings → Skills → Proposals → click **Validate** (all green) → **Approve**.

**Backend proof**
```powershell
python scripts\forge-inspect.py query "SELECT event_type FROM events WHERE aggregate_id='skill_good-skill' ORDER BY seq"
# expect the sequence: skill_validation_passed → skill_promoted

python scripts\forge-inspect.py skills-history good-skill
# expect 1 row: origin=proposal, retired=no, version=0.1.0

# File moved from proposed/ to active/
Test-Path "$env:APPDATA\com.sarthak.forgeos\skills\active\good-skill.md"   # True
Test-Path "$env:APPDATA\com.sarthak.forgeos\skills\proposed\good-proposal.md"  # False
```

---

## TC-P4B-03 · MCP-prefixed tools bypass the whitelist

**Setup**
```powershell
$path = "$env:APPDATA\com.sarthak.forgeos\skills\proposed\mcp-proposal.md"
@'
---
name: mcp-skill
version: 0.1.0
keywords: [mcp]
tools: [mcp:whatever.does.not.exist]
---
# mcp-skill
A proposal that uses an mcp: prefixed tool name that does not exist in the local ToolRegistry, to verify that MCP tools bypass the tools_resolvable whitelist and are gated only at run time by policy.
'@ | Out-File -Encoding utf8 $path
```

**UI** — Validate. All 5 hard checks pass (soft warnings ok).

**Backend proof**
```powershell
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='skill_validation_passed' ORDER BY seq DESC LIMIT 1"
# payload.name = mcp-skill; payload.soft_failures may be non-empty but hard checks pass
```

---

## TC-P4B-04 · AutoPromoter background sweep

**Setup — enable the autopromoter**
- In `apps\forge-desktop\src-tauri\src\lib.rs`, change `auto_promote_skills: false` to `true` and `autopromote_interval_secs: 300` to `30`. Rebuild the app.
- Alternative: run the headless test in the next case.

**UI** — Drop `good-proposal-2.md` (same shape as TC-P4B-02, different name) into `skills\proposed\`. Wait ≤ 60 s. Do NOT click anything.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_auto_promoted -n 5
# expect ≥1 event with name=good-skill-2, sha=..., version=0.1.0

python scripts\forge-inspect.py skills-history good-skill-2
# expect 1 row: origin=proposal (the autopromoter uses the same promote path)

Test-Path "$env:APPDATA\com.sarthak.forgeos\skills\active\good-skill-2.md"   # True
```

---

## TC-P4B-05 · Runnable · full 4b smoke (no UI, no config change)

```powershell
cd C:\Users\t-sarverma\Projects\forge-os
cargo run -p forge-runtime --example skill_validation_smoke
# expect: PASS: validation gate + autopromoter verified end-to-end
```
3 scenarios: good→promote, bad→ValidationFailed(body_length,has_trigger,tools_resolvable), AutoPromoter.sweep picks up a fresh good one.

---

## TC-P4D-01 · Mission queue enqueue emits MissionQueued

**Prompt** — open Settings > Mission Queue. Confirm the workers badge shows either "N workers" (green) or "inline mode" (blue).

**Backend proof**
```powershell
cargo run -p forge-runtime --example worker_pool_smoke
# expect: PASS: worker pool + queue verified end-to-end
# The first scenario prints "✓ saw 4 MissionQueued events"
```

## TC-P4D-02 · Worker pool claims and finishes in parallel

**Backend proof**
```powershell
cargo run -p forge-runtime --example worker_pool_smoke
# expect scenario 1 to print "✓ queue drained: queued=0, claimed=0"
# within 10s (2 workers × 4 missions)
```
Also inspect a live DB after enqueueing via the smoke:
```powershell
python scripts\forge-inspect.py query "SELECT status, COUNT(*) FROM mission_queue GROUP BY status"
```

## TC-P4D-03 · Crash recovery requeues stale claims

**Backend proof**
Scenario 3 of the smoke manually backdates `heartbeat_at` on a Claimed row, then calls `requeue_stale(6)`, verifying it moves the row back to Queued.
```powershell
cargo run -p forge-runtime --example worker_pool_smoke
# expect: "✓ requeue_stale rescued 1 row(s)"
```

## TC-P4D-04 · Terminal missions can be re-enqueued

By design, `enqueue` is idempotent only against ACTIVE (Queued|Claimed) rows; a mission in a terminal state (Done|Failed) can be re-enqueued as a fresh retry row. Scenario 2 of the smoke asserts this behavior:
```powershell
cargo run -p forge-runtime --example worker_pool_smoke
# expect: "✓ retry row appended: 4 → 5"
```

## TC-P4D-05 · Runnable · queue + memory persistence unit tests

```powershell
cargo test -p forge-persistence
# expect: 9 passed; 0 failed
# covers: SqliteMissionQueueRepository (idempotency, claim/finish, stale requeue)
#         SqliteOrgMemoryRepository (insert/search/retire)
#         postgres::connect NotYetImplemented shape
```

---

## TC-P4E-01 · PersistenceHandles URL dispatch

```powershell
cargo run -p forge-runtime --example postgres_dispatch_smoke
# expect: PASS: PersistenceHandles URL dispatch verified
# Confirms:
#   sqlite://…?mode=rwc      → real SQLite bundle
#   postgres://forge@…       → NotYetImplemented("postgres persistence backend")
#   mysql://nope             → rejected downstream
```

## TC-P4E-02 · Postgres stub message points to the roadmap

The `NotYetImplemented` payload must reference `crates/forge-persistence/src/postgres.rs`:
```powershell
cargo run -p forge-runtime --example postgres_dispatch_smoke 2>&1 | Select-String "postgres.rs"
# expect a line ending with "…see crates/forge-persistence/src/postgres.rs head comment for the Phase 5 rollout plan"
```

---

## TC-P4F-01 · Org memory extractor persists reflection insights

**Setup** — run any completed mission (Groq or Ollama) whose reflection produces insights.

**Backend proof**
```powershell
python scripts\forge-inspect.py query "SELECT id, key, tags, source_mission_id FROM org_memory ORDER BY id DESC LIMIT 5"
# expect one row per insight from the most recent reflection
```

Also check the event stream:
```powershell
python scripts\forge-inspect.py events-type org_memory_learned -n 10
```

## TC-P4F-02 · Planner recalls org memory on the next mission

**Setup** — after TC-P4F-01, run a second mission whose title shares keywords with a memory row's tags.

**UI proof** — EventTimeline for the second mission shows an `org_memory_recalled` line with a preview of the injected block.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type org_memory_recalled -n 5
# for the newer mission, expect block_preview to include text from the memory's `value`
```

## TC-P4F-03 · UI retire hides a memory row

**Prompt** — open Settings > Organizational Memory. Click Retire on one row. Confirm the row disappears.

**Backend proof**
```powershell
python scripts\forge-inspect.py query "SELECT id, retired_at FROM org_memory WHERE retired_at IS NOT NULL ORDER BY id DESC LIMIT 5"
# retired row appears with retired_at timestamp
```

## TC-P4F-04 · Retire is idempotent

Click Retire on a memory row, then via `sqlite3` inspect that a second retire call would be a no-op:
```powershell
python scripts\forge-inspect.py query "SELECT retire_id, COUNT(*) FROM org_memory WHERE id = <ID> AND retired_at IS NOT NULL"
```
Or run the smoke:
```powershell
cargo run -p forge-runtime --example org_memory_smoke
# expect scenario 4: first retire returns true, second returns false
```

## TC-P4F-05 · Runnable · org memory storage smoke

```powershell
cargo run -p forge-runtime --example org_memory_smoke
# expect: PASS: org memory storage verified end-to-end
# 4 scenarios: insert × 3, list_active ordering, search by tag, idempotent retire
```

---

# Phase 5 — HTTP API server

## TC-P5-01 · Health probe is unauthenticated, everything else requires bearer

**Setup** — desktop is running; `Settings > HTTP API` shows `running` at `127.0.0.1:7823`. Set `$env:FORGE_API_TOKEN = "s3cr3t"` **before launching** the desktop so `token_set` reads true.

**Prompts** (PowerShell):

```powershell
curl.exe -s -o NUL -w "%{http_code}`n" http://127.0.0.1:7823/health         # expect: 200
curl.exe -s -o NUL -w "%{http_code}`n" http://127.0.0.1:7823/missions       # expect: 401
curl.exe -s -o NUL -w "%{http_code}`n" -H "Authorization: Bearer wrong" `
    http://127.0.0.1:7823/missions                                          # expect: 401
curl.exe -s -o NUL -w "%{http_code}`n" -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
    http://127.0.0.1:7823/missions                                          # expect: 200
```

---

## TC-P5-02 · POST /missions round-trips with `GET /missions/:id`

**Prompt**:

```powershell
$body = @{ title = "api test"; description = "check that the shim boots a plan_and_run" } | ConvertTo-Json
$resp = Invoke-RestMethod -Method Post -Uri http://127.0.0.1:7823/missions `
    -Headers @{Authorization="Bearer $env:FORGE_API_TOKEN"} `
    -ContentType "application/json" -Body $body
$id = $resp.id
Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:7823/missions/$id" `
    -Headers @{Authorization="Bearer $env:FORGE_API_TOKEN"} |
    Select-Object -ExpandProperty mission
```

**Expected**: `mission` object with `id == $id`, `status ∈ { draft, planning, running, completed }` depending on how quickly you fetched. In the desktop UI, `Settings > Mission Queue` should show this mission listed; the DAG for this id shows real events as they land.

---

## TC-P5-03 · SSE `/events` streams live events with `?since` skip

**Prompt** — open two PowerShell windows.

Window 1:
```powershell
curl.exe -N -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
    "http://127.0.0.1:7823/events?since=0"
```

Window 2 (submit any mission via the UI or the curl above).

**Expected in window 1**: an unending stream of `id: <seq>\nevent: forge.event\ndata: {...}\n\n` frames beginning with the desktop's boot events (seq > 0). Re-run with `?since=999999` and confirm no historical replay — only fresh events after the moment of subscription. Add `?mission=<uuid>` and confirm only that mission's cascade streams.

---

## TC-P5-04 · OpenAI-compat shim answers `POST /v1/chat/completions`

**Prompt** (Python one-liner or any OpenAI SDK):

```powershell
curl.exe -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
    -H "Content-Type: application/json" `
    -d '{"model":"forge","messages":[{"role":"user","content":"echo hi"}]}' `
    http://127.0.0.1:7823/v1/chat/completions
```

**Expected**: a JSON body with `object: "chat.completion"`, one entry in `choices`, `finish_reason ∈ { "stop" | "error" | "length" | "cancelled" }`. If your LLM key is valid, `choices[0].message.content` contains a mission summary ("Mission Completed." + bullets per goal). `stream: true` returns 400 with a hint pointing at `/events`.

---

## TC-P5-05 · Runnable · end-to-end API smoke over real TCP

```powershell
cargo run -p forge-server --example api_smoke
# expect: ✅ Phase 5 API smoke complete.
# 7 assertions: /health, wrong-bearer 401, POST /missions, GET /missions/:id,
# cancel, /v1/chat/completions with finish_reason="error" (dummy LLM key),
# /events SSE stream. LLM-free.
```

---

# Phase 5b — CLI + VS Code + skill bundles

## TC-P5B-01 · `forge health` behaves like a real Unix probe

**Setup** — desktop is running (or spawn any Forge Runtime with `api_bind`).

**Prompt** (PowerShell):

```powershell
$env:FORGE_API_URL   = "http://127.0.0.1:7823"
$env:FORGE_API_TOKEN = "s3cr3t"   # match your desktop's FORGE_API_TOKEN

.\target\debug\forge.exe health           # → exit 0, "status: 200 OK"
$env:FORGE_API_URL   = "http://127.0.0.1:1"
.\target\debug\forge.exe health           # → exit non-zero, "operation timed out" or "connection refused"
```

Also verify: `forge health` exits 0 even with a wrong bearer, because `/health` is unauthenticated.

---

## TC-P5B-02 · `forge missions` round-trip drives the API cleanly

```powershell
$env:FORGE_API_TOKEN = "s3cr3t"
.\target\debug\forge.exe missions list                                    # (no missions) or table
$id = (.\target\debug\forge.exe --json missions create "cli test" --plan-only |
       ConvertFrom-Json).id
.\target\debug\forge.exe missions get $id                                 # detail table
.\target\debug\forge.exe missions list --limit 5                          # includes $id
.\target\debug\forge.exe missions cancel $id                              # "cancelled: <uuid>"
```

`--json` on any subcommand switches to newline-delimited JSON output ready to pipe into `jq`.

---

## TC-P5B-03 · `forge run --wait --stream` blocks until terminal

**Prompt**:

```powershell
$env:FORGE_API_TOKEN = "s3cr3t"
.\target\debug\forge.exe run "print hello then exit" --wait --stream
```

Expected: live event summaries stream to stdout (`[#123 2026-…] mission_created …`), the process exits when the mission reaches `completed | failed | cancelled`, and the last stdout block is the goal-by-goal summary. Exit code 0 as long as the mission itself finished (even if `failed` — that's the mission's outcome, not a CLI error).

---

## TC-P5B-04 · Signed skill bundle round-trip + tamper detection

**Prompt**:

```powershell
.\target\debug\forge.exe keygen --out $env:TEMP\forge_test_key
# writes forge_test_key + forge_test_key.pub

New-Item -ItemType Directory -Force $env:TEMP\my-skill | Out-Null
Set-Content $env:TEMP\my-skill\my-skill.md "---`nname: my-skill`nversion: 1`n---`n`nBody."

.\target\debug\forge.exe skill bundle $env:TEMP\my-skill `
    --out $env:TEMP\my-skill.forgebundle.json `
    --key $env:TEMP\forge_test_key

.\target\debug\forge.exe skill verify $env:TEMP\my-skill.forgebundle.json `
    --pubkey $env:TEMP\forge_test_key.pub                                # → "signature OK"

.\target\debug\forge.exe skill install $env:TEMP\my-skill.forgebundle.json `
    --dest $env:TEMP\installed --pubkey $env:TEMP\forge_test_key.pub     # → "installed 1 files"

# Tamper — flip a byte in the base64 file contents:
$b = Get-Content $env:TEMP\my-skill.forgebundle.json -Raw | ConvertFrom-Json
$b.files.PSObject.Properties | ForEach-Object { $_.Value = "dGFtcGVy" }
$b | ConvertTo-Json -Depth 10 | Set-Content $env:TEMP\my-skill.forgebundle.json

.\target\debug\forge.exe skill verify $env:TEMP\my-skill.forgebundle.json # → non-zero exit
```

---

## TC-P5B-05 · Runnable · full CLI integration suite

```powershell
cargo test -p forge-cli --test end_to_end -- --nocapture
# expect: 5 passed; 0 failed
#   cli_end_to_end       — spins a real Runtime, drives every route via `Command::spawn`
#   cli_bundle_roundtrip — keygen → bundle → verify → install → tamper → verify fails
```

---

## TC-P5B-06 · VS Code extension one-shot

**Setup** — desktop running with API up; `apps/forge-vscode/` opened as a folder in VS Code with `npm install && npm run compile` already done.

**Prompt** — press **F5** to launch an Extension Development Host. In the new window:

1. Command Palette → `Forge: Check server health` → notification `Forge server OK at http://127.0.0.1:7823`.
2. Command Palette → `Forge: Run mission (prompt)` → type a title, hit Enter. Notification `Forge: mission xxxxxxxx… created`.
3. Select any text in an editor → Command Palette → `Forge: Send selection as chat` → a scratch markdown doc opens, then updates in place with the response body + `finish_reason`.

**Failure modes to sanity-check**:
- Wrong `forgeOs.apiUrl` → notification `Forge server unreachable at <url>: <reason>`.
- Empty token when server requires one → the chat command surfaces the 401 body as an error notification.

---

## TC-BUG-01 · Mission-filter after create shows the new mission's events

**Regression check for the `msn_<uuid>` vs raw-UUID mismatch.**

**Prompt** — create a mission via the New Mission form. Confirm the EventTimeline for the newly-selected mission is NOT empty (should show `mission_created`, `mission_planning_started`, then goal/task events as they arrive).

Prior to the fix, `create_mission` returned `msn_<uuid>` (Display form) while events serialize as raw UUID, so `filterEvents` with `missionId === selected` never matched and the timeline stayed empty for freshly-created missions.

---

# Cross-cutting

## TC-X-01 · LLM router failover + circuit breaker

**Setup**
- Configure both providers so Groq is tried first. Set an invalid Groq key:
```powershell
$env:GROQ_API_KEY = "INVALID_KEY"
# ensure OPENAI_API_KEY or OPENROUTER_API_KEY is set correctly
```
Restart the app.

**Prompt** — any small mission, e.g. `Title: Print pi`, `Desc: Print the first 5 digits of pi.`

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type llm_failed -n 5
# ≥1 event with provider=groq, error mentioning auth/unauthorized

python scripts\forge-inspect.py events-type llm_requested -n 10
# for the SAME request_id, expect a subsequent llm_requested with provider != groq (failover)

# After 3 consecutive failures the breaker opens for 30 s. Verify:
python scripts\forge-inspect.py query "SELECT DISTINCT json_extract(payload,'$.provider') FROM events WHERE event_type='llm_requested' AND created_at > datetime('now','-30 seconds')"
# groq should NOT appear in this window after the breaker trips
```

---

## TC-X-02 · MCP wire protocol end-to-end (mock server)

```powershell
cd C:\Users\t-sarverma\Projects\forge-os
cargo test -p forge-mcp --test stdio_roundtrip
# expect: test result: ok. 3 passed
```
Exercises the actual stdio transport: real cargo child process, real JSON-RPC frames, real initialize/list-tools/call-tool handshake.

If you already have MCP servers configured in `mcp.yaml`, verify on real boot:
```powershell
python scripts\forge-inspect.py events-type mcp_server_started -n 5
# expect one event per configured server with tools=[...]
python scripts\forge-inspect.py events-type mcp_server_failed -n 5
# any failure entries include a meaningful error string
```

Your live system currently shows: `mcp_server_started { name: "filesystem", tools: [mcp.filesystem.read_file, ...] }` from your last session.

---

## TC-X-03 · Just-in-time task-input materialization

**Runnable** (requires a valid Groq key — the materializer calls the LLM to rewrite inputs)
```powershell
$env:GROQ_API_KEY = "<your-groq-key>"   # or export in the shell before boot
cargo run -p forge-runtime --example materialize_smoke
# expect: PASS banner
```
- Verifies: two-goal mission, downstream goal's `[insert directories]` placeholder is rewritten with the upstream `fs.list` result before execution.
- Emits `task_input_refreshed` events.

Live UI verification: any mission whose planner emits placeholder args in a downstream task. Confirm:
```powershell
python scripts\forge-inspect.py events-type task_input_refreshed -n 10
# each event has: task_id, old_input, new_input; old should contain a placeholder pattern
```

---

## TC-X-04 · User memory reaches the planner prompt

**Setup**
```powershell
"Always prefix filenames with usr_." | Out-File -Encoding utf8 "$env:APPDATA\com.sarthak.forgeos\workspace\USER_MEMORY.md"
```
Restart the app.

**Prompt** — `Title: Save a note`, `Desc: Save a text file "note.txt" with the content "hello".`

**Backend proof**
```powershell
# The planner produced a task that references the memory directive.
python scripts\forge-inspect.py mission $MID
# expect at least one task with tool=fs.write whose input path is scripts/usr_note.txt (or similar prefix)
```

**Runnable alternative**
```powershell
cargo run -p forge-runtime --example user_memory_smoke
# expect: PASS; user memory string appears in the composed prompt
```

---

## TC-X-05 · Runnable · checkpoints round-trip

```powershell
cargo run -p forge-runtime --example checkpoints_headless_smoke
# expect: PASS: CheckpointCreated + CheckpointSkipped verified end-to-end
```

## TC-X-06 · Full unit + integration test regression

```powershell
cargo test -p forge-domain -p forge-planner -p forge-policy -p forge-persistence -p forge-skills -p forge-runtime -p forge-mcp
# expect: 95+ tests passed, 0 failed
```

Your current baseline is 95 tests across those crates.

---

# Phase 4c — Actionable Skill Curator

The Curator scans `skills/active/` and can (a) surface advisory suggestions, (b) auto-archive near-duplicates, and (c) drop merge proposals into `skills/proposed/` for the validator to gate.

Three thresholds (in `CuratorPolicy`, all overridable):
- **Auto-archive** if `name_sim ≥ 0.92` OR `body_sim ≥ 0.85` OR `subset_ratio ≥ 0.95`.
- **Merge candidate** if body_sim ∈ `[merge_similarity_low=0.35, body_similarity_threshold=0.85)`.
- **Unused** if a skill never appears in `SkillsSelected` across the last N terminal missions (fallback: all missions if there are fewer than N terminals yet).

## TC-P4C-01 · Curator auto-archives a near-duplicate

**Setup**
```powershell
$active = "$env:APPDATA\com.sarthak.forgeos\skills\active"
@'
---
name: dedupe-alpha
version: 0.1.0
keywords: [dedupe, alpha]
tools: [fs.write]
---
Run `pytest -q` in the project root. Interpret the exit code as pass or fail. On failure, print the last 20 lines of stdout so the planner can inspect the traceback and choose the next repair task.
'@ | Out-File -Encoding utf8 (Join-Path $active "dedupe-alpha.md")
@'
---
name: dedupe-beta
version: 0.1.0
keywords: [dedupe, beta]
tools: [fs.write]
---
Run `pytest -q` in the project root. Interpret the exit code as pass or fail. On failure, print the last 20 lines of stdout so the planner can inspect the traceback and choose the next repair task. Prefer verbose mode when diagnosing.
'@ | Out-File -Encoding utf8 (Join-Path $active "dedupe-beta.md")
```

**UI** — ⚙ Settings → Skills → **Scan & apply**.
- Toast: `Applied — archived 1 skill`.
- Actions-taken panel lists `dedupe-beta` under Archived.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_auto_archived -n 1
# payload: {name:"dedupe-beta", reason:"duplicate_of dedupe-alpha", similarity: 1.0 or 0.867+, kept:"dedupe-alpha"}

Test-Path (Join-Path "$env:APPDATA\com.sarthak.forgeos\skills\active" "dedupe-beta.md")   # False
Test-Path (Join-Path "$env:APPDATA\com.sarthak.forgeos\skills\archived" "dedupe-beta.md") # True
Test-Path (Join-Path "$env:APPDATA\com.sarthak.forgeos\skills\active" "dedupe-alpha.md")  # True
```

## TC-P4C-02 · Curator generates a merge proposal (mid similarity)

**Setup** — two skills that overlap partially (body_sim ≈ 0.4–0.7). Keep both distinct enough to fall UNDER the 0.85 auto-archive threshold.

**UI** — Settings → Skills → **Dry-run scan** → row with kind `merge_candidate` appears listing both names + similarity. Then **Scan & apply** creates a proposal.

**Backend proof**
```powershell
python scripts\forge-inspect.py events-type skill_merge_proposed -n 1
# payload: {filename:"merged-<a>-<b>.md", merged_name:"...", sources:[a,b], similarity:0.4..0.85}

Get-ChildItem "$env:APPDATA\com.sarthak.forgeos\skills\proposed\merged-*.md"
# expect: at least one file — this is now gated by the validator (Phase 4b), so `curator_scan` does NOT auto-promote it.

python scripts\forge-inspect.py query "SELECT id, event_type FROM events WHERE event_type='skill_merge_proposed' ORDER BY id DESC LIMIT 3"
```

**Idempotency check** — click **Scan & apply** a second time immediately.
- `pending_proposal_names()` sees the proposal already exists in `proposed/`, so no new event fires and no duplicate file is created.

## TC-P4C-03 · Curator flags unused skills, protects recently-used ones

**Setup** — leave `dedupe-alpha` in `active/`, then run one mission that selects it (any short prompt; the planner picks skills from active/).

**UI** — Settings → Skills → **Advisory scan**.
- Unused skills appear as `kind: unused`.
- `dedupe-alpha` should NOT appear as unused because it was selected recently.

**Backend proof**
```powershell
# Confirm dedupe-alpha was selected in a recent terminal mission
python scripts\forge-inspect.py query "SELECT payload FROM events WHERE event_type='skills_selected' ORDER BY id DESC LIMIT 5"

# Run curator via the example (equivalent to Advisory scan):
cargo run -p forge-runtime --example skill_curator_smoke
# expect: PASS — dedupe-alpha never shows as unused in the scenarios where it was 'used' via SkillsSelected
```

## TC-P4C-04 · Runnable · full curator scenario smoke

```powershell
cargo run -p forge-runtime --example skill_curator_smoke
# expect: PASS: Phase 4c curator verified end-to-end
# Scenario 1: dry-run classifies dedupe / merge_candidate / unused correctly
# Scenario 2: apply archives the losing duplicate, writes a merge proposal, validator OK
# Scenario 3: 2nd pass is a no-op (idempotent)
```

## TC-P4C-05 · Runnable · similarity math unit tests

```powershell
cargo test -p forge-skills --lib similarity
# expect: 9 passed; 0 failed
```

---

# Smoke Run — 10 minutes end-to-end

Run in this order. Any FAIL → don't ship.

| # | Case | Type | Est. |
|---|------|------|------|
| 1 | TC-X-06 (full test regression) | Runnable | 2 min |
| 2 | TC-P4A-06 (skill versioning smoke) | Runnable | 30 s |
| 3 | TC-P4B-05 (validation gate smoke) | Runnable | 30 s |
| 4 | TC-P4C-04 (curator smoke) | Runnable | 30 s |
| 5 | TC-P4D-05 (queue + memory unit tests) | Runnable | 20 s |
| 6 | TC-P4D-02/03/04 (worker pool smoke) | Runnable | 30 s |
| 7 | TC-P4E-01 (postgres dispatch smoke) | Runnable | 10 s |
| 8 | TC-P4F-05 (org memory storage smoke) | Runnable | 15 s |
| 8b | TC-P5-05 (API server end-to-end smoke) | Runnable | 25 s |
| 8c | TC-P5B-05 (CLI integration suite) | Runnable | 45 s |
| 9 | TC-X-05 (checkpoints smoke) | Runnable | 30 s |
| 10 | TC-X-03 (materialize smoke) — needs GROQ_API_KEY | Runnable | 30 s |
| 11 | TC-X-02 (MCP round-trip) | Runnable | 30 s |
| 12 | TC-P1-01 (mission happy path) — verify with `python scripts/forge-inspect.py mission $MID` | UI+backend | 2 min |
| 13 | TC-BUG-01 (mission-filter shows events after create) | UI+backend | 30 s |
| 14 | TC-P1-02 (cancel) — verify events | UI+backend | 1 min |
| 15 | TC-P4B-01 + TC-P4B-02 (bad + good proposal) | UI+backend | 2 min |
| 16 | TC-P4C-01 (curator auto-archives duplicate) | UI+backend | 1 min |
| 17 | TC-P3-03 (checkpoint file gone after revert) | UI+backend | 1 min |
| 18 | TC-P4F-01/02 (memory learned then recalled on next mission) | UI+backend | 3 min |
| 19 | TC-P5-01/02/03 (HTTP API + SSE end-to-end from host tools) | UI+backend | 2 min |
| 20 | TC-P5B-02/04 (forge CLI missions + skill bundles) | UI+backend | 2 min |
| 21 | TC-P5B-06 (VS Code extension smoke) | UI+backend | 2 min |
| 22 | TC-P5C-01 (streaming SSE from `/v1/chat/completions`) | Runnable | 30 s |
| 23 | TC-P5D-01/02/03 (RBAC: RO can list, cannot create/chat; unknown token 401) | Runnable | 1 min |
| 24 | TC-P5E-01/02/03 (gateway health + signed Slack + generic webhook) | Runnable | 2 min |
| 25 | TC-P5F-01/02 (plugin bundle sign + verify + install) | Runnable | 30 s |

If all 10 pass, the system is verifiably working end-to-end: UI, IPC, event bus, persistence, LLM router, planner, executor, tools, policy, skills (load/select/validate/promote/rollback/retire/autopromote), reflection, cost tracking, checkpoints, MCP, materializer.

---

# Phase 5c/5d/5e/5f — Streaming shim + RBAC + Gateway + Plugin bundles

## TC-P5C-01 · `/v1/chat/completions` honours `stream: true`

**Setup** — desktop running with `FORGE_API_TOKEN=s3cr3t`.

**Prompt** (PowerShell):

```powershell
curl.exe -N --http1.1 `
  -H "Authorization: Bearer s3cr3t" `
  -H "Content-Type: application/json" `
  -H "Accept: text/event-stream" `
  --data-raw '{"model":"forge","stream":true,"messages":[{"role":"user","content":"tiny test"}]}' `
  http://127.0.0.1:7823/v1/chat/completions
```

**Pass criteria:**
- Response headers include `content-type: text/event-stream`.
- Body contains at least one `data: {"id":"...","object":"chat.completion.chunk"...}` frame.
- Stream ends with `data: [DONE]` on its own line.
- Automated equivalent: `cargo test -p forge-cli --test end_to_end openai_streaming_shim_emits_chunks -- --exact`.

## TC-P5D-01 · ReadOnly token can list missions

```powershell
$env:FORGE_API_TOKEN            = "s3cr3t"
$env:FORGE_API_READONLY_TOKENS  = "ro-token-1,ro-token-2"
# restart desktop so the env is picked up

# Then, from another shell:
curl.exe -sS -H "Authorization: Bearer ro-token-1" http://127.0.0.1:7823/missions
# → 200 OK, JSON array
```

## TC-P5D-02 · ReadOnly token cannot create missions

```powershell
curl.exe -sS -o - -w "`nHTTP %{http_code}`n" `
  -X POST `
  -H "Authorization: Bearer ro-token-1" `
  -H "Content-Type: application/json" `
  --data-raw '{"objective":"nope","approval_mode":"auto","budget":{"llm_calls_max":1,"tool_calls_max":1,"wallclock_seconds_max":1}}' `
  http://127.0.0.1:7823/missions
# → HTTP 403, body contains "forbidden" or "requires a full-access token"
```

Same behaviour for `POST /missions/{id}/cancel`, `/missions/{id}/extend`, and `POST /v1/chat/completions`.

## TC-P5D-03 · Unknown token still 401s

```powershell
curl.exe -sS -o - -w "`nHTTP %{http_code}`n" `
  -H "Authorization: Bearer nope" http://127.0.0.1:7823/missions
# → HTTP 401
```

Automated equivalent for all three: `cargo test -p forge-cli --test end_to_end cli_readonly_token_restricts_writes -- --exact`.

## TC-P5E-01 · Gateway `/health`

```powershell
cargo run -p forge-gateway
# in another shell:
curl.exe -sS http://127.0.0.1:7824/health
# → {"status":"ok"}
```

## TC-P5E-02 · Gateway `/webhook` requires shared secret

```powershell
$env:GATEWAY_SHARED_SECRET = "shh"
$env:FORGE_API_URL         = "http://127.0.0.1:7823"
$env:FORGE_API_TOKEN       = "s3cr3t"
cargo run -p forge-gateway

# in another shell:
curl.exe -sS -o - -w "`nHTTP %{http_code}`n" `
  -X POST -H "Authorization: Bearer wrong" `
  -H "Content-Type: application/json" `
  --data-raw '{"prompt":"hello"}' `
  http://127.0.0.1:7824/webhook
# → HTTP 401

curl.exe -sS -X POST -H "Authorization: Bearer shh" `
  -H "Content-Type: application/json" `
  --data-raw '{"prompt":"hello"}' `
  http://127.0.0.1:7824/webhook
# → 200 OK, JSON with a `response` field
```

## TC-P5E-03 · Slack signing-secret verifier

Unit-tested; run:

```powershell
cargo test -p forge-gateway
```

Should print `4 passed; 0 failed`. Covers a valid signature, a tampered body (rejected), a stale timestamp (rejected), and the router-builds smoke.

## TC-P5F-01 · Plugin bundle sign + verify

```powershell
$dir = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_.FullName }
Set-Content "$dir\mcp.yaml" "servers: {}"
Set-Content "$dir\readme.md" "# demo plugin"

.\target\debug\forge.exe keygen $dir\priv.key
.\target\debug\forge.exe plugin bundle $dir $dir\plugin.json $dir\priv.key
.\target\debug\forge.exe plugin verify $dir\plugin.json --pubkey $dir\priv.key.pub
# → "signature OK  (kind: Plugin)"
```

## TC-P5F-02 · Plugin install writes to `plugins/`

```powershell
$dest = New-TemporaryFile | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_.FullName }
.\target\debug\forge.exe plugin install $dir\plugin.json --dest $dest
Get-ChildItem -Recurse $dest
# → should list the plugin subdirectory containing mcp.yaml + readme.md
```

Automated equivalent: `cargo test -p forge-cli --test end_to_end cli_plugin_bundle_roundtrip -- --exact`.

---

# Environmental caveats

- Live-LLM cases (TC-P2-*, TC-X-01, TC-X-04) depend on your provider key being valid and quota not being exhausted (Groq TPD cap on free tier).
- Behaviour of the planner is probabilistic — accept minor phrasing differences in task descriptions; the backend-proof steps are deterministic.
- TC-P3-01 (feature flags) and TC-P4B-04 (autopromoter interval) require a restart; skip in fast smoke runs.
- Your live DB currently has 23 missions, 61 goals, 67 tasks, 868 events, 18 reflections, 5 active skills — the inspector confirms it's already accumulating real state.


---

# Phase 6 tests (added this session)

## TC-P6C-01 — Anthropic + Gemini response parsing (unit)
**What:** Both adapters round-trip typical + error payloads without panicking; Anthropic hoists `role: system` into the top-level `system` field.
**Run:** `cargo test -p forge-llm`
**Expect:** 13 passed (2 embed, 3 anthropic, 2 gemini, 2 openai naming, 4 azure — see TC-P6G).

## TC-P6C-02 — Anthropic via failover chain (manual, needs `ANTHROPIC_API_KEY`)
**What:** With no other keys set, mission planning uses Claude and produces a valid plan.
**Prep:** `[System.Environment]::SetEnvironmentVariable('ANTHROPIC_API_KEY','sk-ant-...','User')` — restart Tauri.
**Run:** Create a mission "Summarise the file `docs/ARCHITECTURE.md` in three bullets."
**Expect:** Plan appears, `LlmResponded` event mentions `claude-3-5-haiku-latest`, mission completes.

## TC-P6C-03 — Gemini via failover chain (manual, needs `GEMINI_API_KEY`)
**What:** Same as -02 but Gemini.
**Prep:** Set `GEMINI_API_KEY`; unset all higher-priority keys.
**Run:** Same mission.
**Expect:** `LlmResponded` mentions `gemini-1.5-flash`.

## TC-P6B-01 — OTLP export smoke (manual, needs a local collector)
**What:** With `FORGE_OTLP_ENDPOINT=http://localhost:4318` set, boot the runtime and confirm the collector logs incoming spans.
**Prep:** `docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest --config=/etc/otel-collector-config.yaml` (or your existing collector).
**Run:** `[System.Environment]::SetEnvironmentVariable('FORGE_OTLP_ENDPOINT','http://localhost:4318','User')`, launch Tauri, kick off any mission.
**Expect:** Collector logs show `service.name=forge-runtime` spans (mission, planner, LLM, task).

## TC-P6B-02 — OTLP is opt-in (unit)
**What:** With no `FORGE_OTLP_ENDPOINT`, the runtime boots the fmt-only subscriber and no OTLP calls happen.
**Run:** `cargo test -p forge-runtime`.
**Expect:** existing tests unchanged (they never set the env var).

## TC-P6E-01 — Discord signature verification (unit)
**What:** ed25519 over `timestamp || body` accepts correct sigs, rejects tampered bodies.
**Run:** `cargo test -p forge-gateway discord_signature`
**Expect:** `discord_signature_verifies_when_correct` and `discord_signature_rejects_tampered_body` pass.

## TC-P6E-02 — Discord command text extraction (unit)
**What:** First STRING option (type=3) wins; falls back to command name when absent.
**Run:** `cargo test -p forge-gateway discord_extracts`
**Expect:** both `discord_extracts_string_option` and `discord_falls_back_to_command_name` pass.

## TC-P6E-03 — Discord live PING (manual, needs a real bot)
**What:** Register the endpoint URL in Discord Developer Portal → app → Interactions Endpoint URL. Discord issues a PING; the endpoint responds with PONG or the URL is rejected.
**Prep:** Set `DISCORD_APPLICATION_PUBLIC_KEY` on the running gateway. Deploy the gateway behind a public HTTPS URL (ngrok fine).
**Expect:** Discord accepts the URL. Save-time PING appears in gateway logs.

## TC-P6E-04 — Telegram webhook echo (manual, needs a real bot token)
**What:** Bot receives a message and calls the gateway; gateway forwards to Forge; reply is `sendMessage`d back to the chat.
**Prep:** `TELEGRAM_BOT_TOKEN=...`, optional `TELEGRAM_SECRET_TOKEN=...`. Set webhook: `curl -X POST "https://api.telegram.org/bot<TOKEN>/setWebhook?url=<PUBLIC_URL>/telegram/webhook&secret_token=<SECRET>"`.
**Run:** DM the bot "list files in the workspace".
**Expect:** Bot replies with the Forge mission output.

## TC-P6A-01 — Semantic memory ranks by cosine (unit)
**What:** Three memory rows with hand-picked 4-dim vectors; a query along x correctly ranks the pure-x row first.
**Run:** `cargo test -p forge-persistence memory_semantic_search_ranks_by_cosine`
**Expect:** pass.

## TC-P6A-02 — Semantic search skips mismatched dims (unit)
**What:** A dim-4 row and a dim-8 row live in the same table; a dim-4 query only sees the dim-4 row.
**Run:** `cargo test -p forge-persistence memory_semantic_search_ignores_mismatched_dims`
**Expect:** pass.

## TC-P6A-03 — Lazy backfill via `set_embedding` (unit)
**What:** Insert an embedding-less row, backfill via `set_embedding`, semantic search now finds it; retiring the row hides it from semantic search.
**Run:** `cargo test -p forge-persistence memory_set_embedding_backfills_existing_row`
**Expect:** pass.

## TC-P6A-04 — Semantic recall beats keyword on nuance (manual, needs `OPENAI_API_KEY` or a local ollama with `nomic-embed-text`)
**What:** Prove semantic recall surfaces a memory the keyword recall would miss.
**Prep:** Add to `forge.toml`:
```toml
[embedding_provider]
kind = "openai"      # or "ollama"
api_key_env = "OPENAI_API_KEY"
```
Then, with the desktop app, run mission A: "This project uses pytest." — wait for reflection to persist an org-memory row and let the spawned embedder task backfill it.
**Run:** Create mission B: "How should I check that a Python function raises the right exception?"
**Expect:** Planner prompt includes the "pytest" org-memory row under `## Prior learnings (semantic recall)` even though "pytest" and "raises" don't share keywords. Without an embedding provider the keyword search would silently miss it.

---

# Phase 6g tests — provider expansion (Azure OpenAI + LM Studio + vLLM)

## TC-P6G-01 — Azure OpenAI adapter (unit)
**What:** URL is built as `{endpoint}/openai/deployments/{deployment}/chat/completions?api-version=...` (trailing slash trimmed), `api-version` override honored, typical + error payloads parse without panicking.
**Run:** `cargo test -p forge-llm azure`
**Expect:** 4 passed (`chat_url_*`, `api_version_override`, `parse_typical`, `parse_error`).

## TC-P6G-02 — OpenAI adapter is name-configurable (unit)
**What:** `OpenAiProvider` defaults `name()` to `openai`; `with_name(...)` overrides it so LM Studio / vLLM (OpenAI-compatible) reuse the same adapter with a distinct provider label in `CompletionResponse.provider`.
**Run:** `cargo test -p forge-llm openai::tests`
**Expect:** 7 passed, including `default_name_is_openai` and `with_name_overrides_for_compatible_backends`.

## TC-P6G-03 — Azure via failover chain (manual, needs an Azure deployment)
**What:** With Azure env vars set (and higher-priority keys unset), mission planning routes to the Azure deployment.
**Prep:** Set `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_ENDPOINT` (e.g. `https://my-res.openai.azure.com`), `AZURE_OPENAI_DEPLOYMENT` (e.g. `gpt-4o`); optional `AZURE_OPENAI_API_VERSION` (default `2024-06-01`). Restart the app.
**Run:** Create a mission "Summarise `docs/ARCHITECTURE.md` in three bullets."
**Expect:** Plan appears; `LlmResponded` attributes cost to the deployment name; mission completes. Provider only registers when key + endpoint + deployment are all present (else a specific warn is logged).

## TC-P6G-04 — LM Studio / vLLM local backends (manual, opt-in via base env)
**What:** Local OpenAI-compatible servers are wired only when their base env var is set, so idle machines don't get dead providers.
**Prep (LM Studio):** start LM Studio's local server, then `LMSTUDIO_BASE_URL=http://127.0.0.1:1234/v1` (no key needed).
**Prep (vLLM):** run vLLM's OpenAI server, then `VLLM_BASE_URL=http://127.0.0.1:8000/v1` (optional `VLLM_API_KEY`).
**Run:** Restart the app with all cloud keys unset; create any mission.
**Expect:** Planning succeeds against the local server; `CompletionResponse.provider` reads `lm_studio` / `vllm` respectively.

