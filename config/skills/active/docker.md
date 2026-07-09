---
name: docker
version: 1.0.0
description: Build, run, and inspect containers and Compose stacks with the Docker CLI.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - docker
    - container
    - dockerfile
    - compose
    - image
  file_globs:
    - "**/Dockerfile"
    - "**/*.dockerfile"
    - "**/docker-compose*.yml"
    - "**/compose*.yaml"
---
# Docker Playbook

Use this playbook whenever the mission builds, runs, or debugs containers.

## Preflight
1. `docker version` and `docker info` to confirm the daemon is reachable.
2. Read the `Dockerfile` / `docker-compose.yml` before changing anything —
   note the base image, exposed ports, volumes, and multi-stage boundaries.

## Building
- Build with an explicit tag: `docker build -t <name>:<sha-or-tag> .`.
- Prefer multi-stage builds; keep the final stage minimal (distroless or
  `-slim`). Never bake secrets into layers — use build args or mounts.
- Pin base image digests for reproducibility once the image stabilises.

## Running
- Run detached with a name so you can find it: `docker run -d --name <n> ...`.
- Map only the ports the mission needs. Prefer `--rm` for throwaway runs.
- Inspect logs with `docker logs -f <name>`; exec in with
  `docker exec -it <name> sh` only for debugging.

## Compose
- `docker compose up -d --build` to bring a stack up; `docker compose ps` to
  check health; `docker compose logs -f <svc>` per service.

## Rollback
- Stop and remove throwaway containers: `docker rm -f <name>`.
- Never `docker system prune -a` on a shared host — it deletes other missions'
  images and caches. Remove only the specific image/tag you created.
