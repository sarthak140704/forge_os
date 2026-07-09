---
name: kubernetes
version: 1.0.0
description: Inspect, apply, and debug Kubernetes workloads with kubectl and Helm.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - kubernetes
    - kubectl
    - helm
    - k8s
    - pod
    - deployment
    - namespace
  file_globs:
    - "**/*.k8s.yaml"
    - "**/kustomization.yaml"
    - "**/charts/**/*.yaml"
    - "**/templates/**/*.yaml"
---
# Kubernetes Playbook

Use this playbook whenever the mission touches a Kubernetes cluster.

## Preflight — confirm the target
1. `kubectl config current-context` — **always** verify you are not pointed at
   production. Refuse mutating actions if the context looks like prod unless the
   mission explicitly authorises it.
2. `kubectl get ns` and set the namespace explicitly with `-n <ns>` on every
   command. Never rely on the default namespace.

## Inspecting
- `kubectl get pods -n <ns> -o wide` to see scheduling + node placement.
- `kubectl describe pod <p> -n <ns>` for events (image pull errors, OOMKills).
- `kubectl logs <p> -n <ns> [-c <container>] --tail=200` for app output.

## Applying changes
- Dry-run first: `kubectl apply -f <file> --dry-run=server -n <ns>`.
- Apply declaratively (`kubectl apply -f`), never `kubectl edit` for anything
  you want reproducible. Keep manifests in the repo as the source of truth.
- Helm: `helm diff upgrade` (plugin) before `helm upgrade --install`.

## Safety rails
- Never `kubectl delete namespace` or scale prod to zero without an approval.
- Prefer `kubectl rollout restart deploy/<d>` over deleting pods manually.

## Rollback
- `kubectl rollout undo deploy/<d> -n <ns>` reverts to the prior ReplicaSet.
- `helm rollback <release> <revision>` for Helm-managed workloads.
