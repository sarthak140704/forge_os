---
name: terraform
version: 1.0.0
description: Plan and apply infrastructure-as-code changes with Terraform, safely.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - terraform
    - infrastructure
    - iac
    - hcl
    - provisioning
    - tfstate
  file_globs:
    - "**/*.tf"
    - "**/*.tfvars"
    - "**/.terraform.lock.hcl"
---
# Terraform Playbook

Use this playbook whenever the mission provisions or changes infrastructure.

## Preflight
1. `terraform version` and `terraform init` (idempotent) to sync providers.
2. Read `*.tf` and any `backend` block — know where state lives. Never touch a
   remote state file directly.
3. `terraform workspace show` — confirm you are in the intended workspace
   (e.g. `staging`, not `prod`).

## The plan gate
- **Always** run `terraform plan -out=tfplan` and read the summary before
  applying. Count creates/updates/**destroys**. Any destroy of a stateful
  resource (database, bucket, disk) requires explicit mission approval.
- Apply only the reviewed plan file: `terraform apply tfplan`. Never
  `terraform apply -auto-approve` against shared or production state.

## Formatting & validation
- `terraform fmt -recursive` to normalise, then `terraform validate` to catch
  type/reference errors before planning.

## Rollback
- Terraform has no generic "undo": recovery is a forward apply that restores
  the previous configuration. Keep the prior `.tf` under version control so a
  `git revert` + `plan` + `apply` returns to the known-good state.
- For accidental destroys, restore from the provider's backup/snapshot — never
  hand-edit `terraform.tfstate`.
