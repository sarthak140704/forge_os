//! Stdio transport for MCP servers.
//!
//! Spawns a child process, treats its stdin/stdout as a line-delimited
//! JSON-RPC channel, and drains stderr into `tracing` so a misbehaving
//! server's error output shows up in the same log stream as the rest of
//! Forge.
//!
//! Every transport owns its child process — dropping the transport kills
//! the child. This matches the MCP spec's "server lifetime == transport
//! lifetime" model and avoids orphaned processes when the runtime shuts
//! down.

use crate::protocol::McpError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// A framed line off the MCP server's stdout. `Line` is the raw JSON payload
/// (without a trailing newline). `Closed` means the server exited or its
/// stdout was closed.
#[derive(Debug)]
pub enum Frame {
    Line(String),
    Closed,
}

/// A live stdio connection to an MCP server.
///
/// * `writer` — send JSON-RPC frames.
/// * `rx`     — receive JSON-RPC frames.
/// * `_child` — held to keep the child alive; drops → SIGKILL.
pub struct StdioTransport {
    pub writer: FrameWriter,
    pub rx: mpsc::Receiver<Frame>,
    pub child: ChildGuard,
}

impl StdioTransport {
    /// Spawn `command args...` with the given env additions and start the
    /// two background tasks that shuttle bytes between the child and the
    /// mpsc channel.
    ///
    /// **Windows note:** if `command` is a bare name without a path separator
    /// or extension (e.g. `npx`, `python`), we route through `cmd.exe /c` so
    /// the shell's `PATHEXT` handling picks up `.cmd`/`.bat` shims like
    /// npm's `npx.cmd`. Otherwise `Command::new("npx")` would fail with
    /// "program not found" even though `npx.cmd` sits right on `PATH`.
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&PathBuf>,
    ) -> Result<Self, McpError> {
        let mut cmd = build_command(command, args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Env: pass through parent env, then apply overrides.
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }

        let mut child = cmd.spawn().map_err(McpError::Io)?;
        let stdin = child.stdin.take().ok_or_else(|| McpError::ServerClosed { phase: "spawn:stdin" })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::ServerClosed { phase: "spawn:stdout" })?;
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<Frame>(128);

        // Reader: line-delimited JSON off stdout.
        let name_for_reader = command.to_string();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        if tx.send(Frame::Line(line)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(Frame::Closed).await;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(mcp = %name_for_reader, error = %e, "stdout read error");
                        let _ = tx.send(Frame::Closed).await;
                        break;
                    }
                }
            }
        });

        // Stderr drain (best-effort).
        if let Some(stderr) = stderr {
            let name_for_stderr = command.to_string();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::debug!(mcp = %name_for_stderr, "stderr: {line}");
                }
            });
        }

        Ok(Self {
            writer: FrameWriter { stdin },
            rx,
            child: ChildGuard(child),
        })
    }
}

/// Writes JSON frames to the child's stdin.
pub struct FrameWriter {
    stdin: ChildStdin,
}

impl FrameWriter {
    /// Send a single JSON value as a line-delimited frame.
    pub async fn write_json(&mut self, v: &serde_json::Value) -> Result<(), McpError> {
        let mut bytes = serde_json::to_vec(v)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

/// RAII wrapper: dropping this guard SIGKILLs the child.
///
/// `Command::kill_on_drop(true)` already does this on Tokio, but we keep the
/// child in a named field to make the ownership obvious to readers.
pub struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // start_kill returns immediately; the wait would block on drop.
        let _ = self.0.start_kill();
    }
}

/// Build a `Command` for the given command + args, applying Windows-specific
/// shell routing when needed. See `StdioTransport::spawn` for the rationale.
fn build_command(command: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let has_sep = command.contains('\\') || command.contains('/');
        let has_ext = std::path::Path::new(command).extension().is_some();
        if !has_sep && !has_ext {
            // Route bare-name commands through cmd.exe so PATHEXT (.cmd/.bat/
            // .exe) resolution matches what a user would get in a shell.
            //
            // We use `/S /C "..."` — /S tells cmd.exe to strip *only* the
            // outer pair of quotes, so nested quoting in the assembled line
            // is preserved verbatim. We build the command line with
            // `raw_arg` to bypass std's argv escaping entirely.
            let mut line = String::from("\""); // outer /S quote pair
            line.push_str(&quote_for_cmd(command));
            for a in args {
                line.push(' ');
                line.push_str(&quote_for_cmd(a));
            }
            line.push('"');
            let mut c = Command::new("cmd.exe");
            c.raw_arg("/S").raw_arg("/C").raw_arg(&line);
            return c;
        }
    }
    let mut c = Command::new(command);
    c.args(args);
    c
}

#[cfg(windows)]
fn quote_for_cmd(s: &str) -> String {
    // cmd.exe quoting: wrap in double quotes if it contains whitespace or
    // any of the shell metacharacters. Existing double quotes are escaped by
    // doubling them (cmd.exe convention).
    let needs_quote = s.is_empty()
        || s.chars().any(|c| c.is_whitespace() || matches!(c, '&' | '|' | '<' | '>' | '^' | '(' | ')' | ',' | ';' | '=' | '"'));
    if needs_quote {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            if ch == '"' {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        s.to_string()
    }
}
