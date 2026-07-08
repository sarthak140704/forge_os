---
name: python-project
version: 1.0.0
description: Scaffold, run, and test a Python project (pip / poetry / uv / pytest).
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - python
    - pip
    - poetry
    - uv
    - pytest
    - django
    - flask
    - fastapi
    - pandas
  file_globs:
    - "**/pyproject.toml"
    - "**/requirements.txt"
    - "**/*.py"
---
# Python Project Playbook

Use this playbook whenever the mission touches a Python codebase.

## Detect the tooling (in this order)
1. `uv.lock` or `.python-version` + `uv` present → use `uv`.
2. `poetry.lock`   → use `poetry`.
3. `Pipfile.lock`  → use `pipenv`.
4. `requirements.txt` or bare `pyproject.toml` → use `pip`.

Do not switch tooling mid-mission.

## Virtual environment
- If no venv is active and the project has one at `.venv/`, activate it.
- If none exists, create one: `python -m venv .venv` and activate it. All
  subsequent `pip install` / `pytest` calls must run inside the venv.

## Edit loop
- Run the project's own commands first (see `pyproject.toml [tool.*]`,
  `Makefile`, or `README.md`).
- Formatter: `ruff format` if configured, else `black`.
- Linter: `ruff check .` if configured, else `flake8`.
- Type checker: `mypy` or `pyright` if a config exists.
- After each meaningful change: `pytest -q --no-header` on the affected
  package. For a fast smoke check, `pytest -q -x` (stop on first failure).

## Standalone scripts
- To *run* a script directly (no test harness), use
  `python -c "..."` for tiny snippets or `python path/to/script.py` for
  files. Always run inside the venv if one exists.

## Rollback
- `pip freeze > requirements.txt` before dependency edits; restore with
  `pip install -r requirements.txt` if needed.
