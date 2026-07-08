//! Built-in tools shipped with Forge OS Phase 1.

use crate::{Tool, ToolCtx, ToolError};
use async_trait::async_trait;
use forge_domain::{Permission, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resolve a caller-provided path against the workspace root. Rejects any
/// path that escapes the root (defence in depth — the policy engine also
/// enforces this).
///
/// Uses `dunce::canonicalize` so Windows `\\?\`-prefixed paths don't cause
/// spurious mismatches. For paths that don't exist yet (e.g. `fs.write`
/// creating a new file), we canonicalize the deepest existing ancestor and
/// splice back the remainder.
fn safe_resolve(root: &Path, given: &str) -> Result<PathBuf, ToolError> {
    let joined = if Path::new(given).is_absolute() {
        PathBuf::from(given)
    } else {
        root.join(given)
    };
    let canon_root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canon_target = canonicalize_deepest_existing(&joined);
    if !canon_target.starts_with(&canon_root) {
        return Err(ToolError::InvalidInput {
            tool: "path".to_string(),
            reason: format!(
                "path {} escapes workspace root {}",
                canon_target.display(),
                canon_root.display()
            ),
        });
    }
    Ok(canon_target)
}

/// Canonicalize the deepest existing ancestor and re-attach the missing
/// suffix. Works for both existing and yet-to-be-created files.
fn canonicalize_deepest_existing(target: &Path) -> PathBuf {
    let mut probe = target.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = dunce::canonicalize(&probe) {
            let mut out = canon;
            for seg in tail.iter().rev() {
                out.push(seg);
            }
            return out;
        }
        match probe.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                if !probe.pop() {
                    return target.to_path_buf();
                }
            }
            None => return target.to_path_buf(),
        }
    }
}

pub fn all(workspace_root: PathBuf) -> Vec<Arc<dyn Tool>> {
    let _ = workspace_root; // tools receive it via ctx; kept for future defaults
    vec![
        Arc::new(FsRead) as Arc<dyn Tool>,
        Arc::new(FsWrite),
        Arc::new(FsMkdir),
        Arc::new(FsList),
        Arc::new(CodeSearch),
        Arc::new(ShellRun),
    ]
}

// ---------------------------------------------------------------------------
// fs.read
// ---------------------------------------------------------------------------

pub struct FsRead;

#[derive(Deserialize)]
struct FsReadInput { path: String }

#[async_trait]
impl Tool for FsRead {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs.read".to_string(),
            description: "Read a UTF-8 text file. Path must be workspace-relative (e.g. `README.md`, `src/lib.rs`). Absolute paths escaping the workspace root will error.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }),
            permissions: vec![Permission::FsRead],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: FsReadInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "fs.read".to_string(), reason: e.to_string(),
        })?;
        let resolved = safe_resolve(&ctx.workspace_root, &args.path)?;
        let content = tokio::fs::read_to_string(&resolved).await?;
        Ok(json!({ "path": resolved.display().to_string(), "content": content, "bytes": content.len() }))
    }
}

// ---------------------------------------------------------------------------
// fs.write
// ---------------------------------------------------------------------------

pub struct FsWrite;

#[derive(Deserialize)]
struct FsWriteInput { path: String, content: String }

#[async_trait]
impl Tool for FsWrite {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs.write".to_string(),
            description: "Write a UTF-8 text file to the workspace, creating parent directories as needed. To create an EMPTY directory (no file), use fs.mkdir instead — passing empty content here writes a 0-byte file, which will block later writes under the same name.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path":    { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            permissions: vec![Permission::FsWrite],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: FsWriteInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "fs.write".to_string(), reason: e.to_string(),
        })?;
        let resolved = safe_resolve(&ctx.workspace_root, &args.path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&resolved, args.content.as_bytes()).await?;
        Ok(json!({ "path": resolved.display().to_string(), "bytes": args.content.len() }))
    }
}

// ---------------------------------------------------------------------------
// fs.mkdir
// ---------------------------------------------------------------------------

pub struct FsMkdir;

#[derive(Deserialize)]
struct FsMkdirInput { path: String }

#[async_trait]
impl Tool for FsMkdir {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs.mkdir".to_string(),
            description: "Create a directory (recursively). Use this to make an EMPTY folder. To create a file, use fs.write — its parent dirs are auto-created.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }),
            permissions: vec![Permission::FsWrite],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: FsMkdirInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "fs.mkdir".to_string(), reason: e.to_string(),
        })?;
        let resolved = safe_resolve(&ctx.workspace_root, &args.path)?;
        if resolved.is_file() {
            return Err(ToolError::InvalidInput {
                tool: "fs.mkdir".to_string(),
                reason: format!("path {} exists as a file — cannot create directory", resolved.display()),
            });
        }
        tokio::fs::create_dir_all(&resolved).await?;
        Ok(json!({ "path": resolved.display().to_string(), "created": true }))
    }
}

// ---------------------------------------------------------------------------
// fs.list
// ---------------------------------------------------------------------------

pub struct FsList;

#[derive(Deserialize)]
struct FsListInput { #[serde(default)] path: Option<String> }

#[derive(Serialize)]
struct DirEntry { name: String, kind: &'static str, bytes: Option<u64> }

#[async_trait]
impl Tool for FsList {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fs.list".to_string(),
            description: "List files and directories at a workspace-relative path. Pass `.` (or omit `path`) for the workspace root. Do NOT pass absolute paths like `C:\\` or `/` — they escape the sandbox and will error. For paths outside the workspace, use an MCP filesystem tool if configured.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "additionalProperties": false
            }),
            permissions: vec![Permission::FsRead],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: FsListInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "fs.list".to_string(), reason: e.to_string(),
        })?;
        let target = safe_resolve(&ctx.workspace_root, args.path.as_deref().unwrap_or("."))?;
        let mut rd = tokio::fs::read_dir(&target).await?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            let ft = entry.file_type().await?;
            let meta = entry.metadata().await.ok();
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                kind: if ft.is_dir() { "dir" } else if ft.is_symlink() { "symlink" } else { "file" },
                bytes: meta.map(|m| m.len()),
            });
        }
        Ok(json!({ "path": target.display().to_string(), "entries": out }))
    }
}

// ---------------------------------------------------------------------------
// code.search  (naive substring across .rs/.ts/.tsx/.js/.py — Phase-2 swaps in ripgrep)
// ---------------------------------------------------------------------------

pub struct CodeSearch;

#[derive(Deserialize)]
struct CodeSearchInput { pattern: String, #[serde(default)] path: Option<String>, #[serde(default = "default_limit")] limit: usize }
fn default_limit() -> usize { 100 }

#[derive(Serialize)]
struct Hit { path: String, line: usize, text: String }

#[async_trait]
impl Tool for CodeSearch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "code.search".to_string(),
            description: "Substring search across workspace files (Rust/TS/JS/Python by default).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path":    { "type": "string" },
                    "limit":   { "type": "integer", "minimum": 1, "maximum": 1000 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            permissions: vec![Permission::FsRead],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: CodeSearchInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "code.search".to_string(), reason: e.to_string(),
        })?;
        let root = safe_resolve(&ctx.workspace_root, args.path.as_deref().unwrap_or("."))?;
        let pattern = args.pattern;
        let limit = args.limit;
        let hits = tokio::task::spawn_blocking(move || walk_and_search(&root, &pattern, limit))
            .await
            .map_err(|e| ToolError::Exec(format!("join error: {e}")))??;
        Ok(json!({ "hits": hits, "count": hits.len() }))
    }
}

fn walk_and_search(root: &std::path::Path, pattern: &str, limit: usize) -> Result<Vec<Hit>, ToolError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.starts_with('.') || name == "node_modules" || name == "target" || name == "dist" { continue; }
            }
            let ft = match entry.file_type() { Ok(t) => t, Err(_) => continue };
            if ft.is_dir() { stack.push(path); continue; }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "md" | "toml" | "yaml" | "yml") { continue; }
            let content = match std::fs::read_to_string(&path) { Ok(c) => c, Err(_) => continue };
            for (i, line) in content.lines().enumerate() {
                if line.contains(pattern) {
                    out.push(Hit { path: path.display().to_string(), line: i + 1, text: line.to_string() });
                    if out.len() >= limit { return Ok(out); }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// shell.run
// ---------------------------------------------------------------------------

pub struct ShellRun;

#[derive(Deserialize)]
struct ShellInput { command: String, #[serde(default)] cwd: Option<String> }

#[async_trait]
impl Tool for ShellRun {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "shell.run".to_string(),
            description: "Run a shell command in a workspace-relative directory. Captures stdout/stderr/exit.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "cwd":     { "type": "string" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            permissions: vec![Permission::Shell],
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value, ToolError> {
        let args: ShellInput = serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
            tool: "shell.run".to_string(), reason: e.to_string(),
        })?;
        let cwd = safe_resolve(&ctx.workspace_root, args.cwd.as_deref().unwrap_or("."))?;
        // Windows uses cmd.exe /C; other platforms use sh -c.
        #[cfg(target_os = "windows")]
        let output = tokio::process::Command::new("cmd")
            .arg("/C").arg(&args.command).current_dir(&cwd).output().await?;
        #[cfg(not(target_os = "windows"))]
        let output = tokio::process::Command::new("sh")
            .arg("-c").arg(&args.command).current_dir(&cwd).output().await?;
        Ok(json!({
            "exit":   output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "cwd":    cwd.display().to_string(),
        }))
    }
}
