---
name: incident-response
version: 1.0.0
description: Triage, diagnose, and mitigate a production incident methodically.
tools:
  - fs.read
  - code.search
  - shell.run
triggers:
  keywords:
    - incident
    - outage
    - on-call
    - triage
    - postmortem
    - sev
---
# Incident Response Playbook

Use this playbook when the mission is to respond to a live incident. Priority
order is always: **mitigate → diagnose → fix → learn.** Restore service first;
root-cause after.

## 1. Stabilise and scope
- Confirm the symptom with data, not assumptions: check dashboards, `tail`
  logs, error rates, and recent deploys (`git log`, deploy history).
- Establish blast radius: which users/regions/services? Is it getting worse?

## 2. Mitigate fast (reversible actions first)
- The fastest safe mitigation is usually **roll back the last deploy** or
  disable the offending feature flag — not a forward hotfix under pressure.
- Scale up, fail over, or shed load if the cause is saturation.
- Every mitigation must itself be reversible and logged.

## 3. Diagnose
- Correlate the incident start with the change timeline. `code.search` the
  error signature to find the responsible code path.
- Reproduce in a safe environment before shipping a forward fix.

## 4. Fix and verify
- Ship the smallest correct fix. Confirm error rates return to baseline and
  watch for a full recovery cycle before declaring resolved.

## 5. Learn
- Capture a blameless timeline: what happened, detection time, mitigation,
  root cause, and concrete action items to prevent recurrence.

## Guardrails
- Do not make large, irreversible changes to production while diagnosing under
  pressure. Communicate status; escalate if blast radius grows.
