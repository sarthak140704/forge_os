---
name: aws
version: 1.0.0
description: Operate AWS resources through the aws CLI with least-privilege guardrails.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - aws
    - s3
    - ec2
    - lambda
    - iam
    - cloudwatch
    - dynamodb
---
# AWS Playbook

Use this playbook whenever the mission operates AWS resources.

## Preflight — confirm identity and account
1. `aws sts get-caller-identity` — **always** verify the account id and role
   before any mutation. Refuse destructive actions if the account looks like
   production without explicit mission approval.
2. Set the region explicitly with `--region <r>` on every command; never rely
   on an ambient default.
3. Prefer `--profile <name>` over long-lived root keys.

## Read-first patterns
- S3: `aws s3 ls s3://<bucket>/<prefix>` before any `cp`/`rm`.
- EC2: `aws ec2 describe-instances --filters ...` before stop/terminate.
- Logs: `aws logs tail <group> --follow` to observe Lambda / service output.

## Mutations
- Use `--dry-run` where the API supports it (EC2 actions do).
- For anything that deletes data (`s3 rm --recursive`, `dynamodb delete-table`,
  `ec2 terminate-instances`), require an explicit approval and prefer a
  reversible alternative (versioning, snapshots, soft-delete) first.

## Rollback
- Restore S3 objects from a prior version if bucket versioning is on.
- Recreate EC2/RDS from the latest snapshot rather than trying to "undo".
- Keep infra declarative (see the `terraform` skill) so recovery is a
  re-apply, not manual clicking.
