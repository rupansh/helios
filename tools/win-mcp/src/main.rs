//! win-mcp — a local stdio MCP server that runs commands and cargo/cargo-make
//! builds on the Helios `win11` dev VM over SSH.
//!
//! Why this exists: raw `ssh win "..."` from the agent suffers from cmd.exe
//! quoting hell and stale-ControlMaster environments. This server wraps the VM
//! with two clean tools and ships commands as base64-UTF16LE PowerShell
//! (`powershell -EncodedCommand`), which is immune to shell quoting and
//! propagates the real exit code.
//!
//! Runs on Linux (the agent's host); the win11 project tree is shared at `Z:\`,
//! so no file tools are needed — the agent edits files directly on the Linux
//! side of the share.

use std::collections::HashMap;
use std::future::Future; // referenced by the #[tool] macro expansion
use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use base64::Engine as _;
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
    ServerHandler, ServiceExt,
};
use serde::Deserialize;
use tokio::process::Command;

/// Shared project root on the Windows side (the Z: drive maps the Linux tree).
const PROJECT_DRIVE: &str = "Z:\\";
/// Local build mirror. cargo/wdk build IO fails on the Z:\ 9p share (OS error 87,
/// see windows-drivers-rs#481), so win_cargo robocopy-syncs here and builds on
/// local disk. Edit sources on Linux/Z:\; the mirror is re-synced each build.
const MIRROR_ROOT: &str = "C:\\Users\\Rupansh\\helios-vgpu";
/// libclang location for bindgen (set as LIBCLANG_PATH for cargo builds).
const LIBCLANG_PATH: &str = "C:\\Program Files\\LLVM\\bin";
/// SSH host alias for the dev VM (from ~/.ssh/config).
const SSH_HOST: &str = "win";

#[derive(Clone)]
pub struct WinHost {
    tool_router: ToolRouter<WinHost>,
}

struct ExecOutput {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    timed_out: bool,
}

/// Encode a PowerShell script as base64 of its UTF-16LE bytes, the input format
/// for `powershell -EncodedCommand`. This avoids all shell quoting concerns.
fn encode_powershell(script: &str) -> String {
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16le)
}

/// Escape a string for a PowerShell single-quoted literal (double the quotes).
fn ps_single_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// Keep the last `max` bytes of `s` (on a char boundary), noting truncation.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…[{} earlier bytes truncated]…\n{}", start, &s[start..])
}

/// Drop SSH-client banners (the post-quantum warning) and any stray CLIXML
/// artifacts so build errors aren't buried in noise.
fn clean_stderr(s: &str) -> String {
    s.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("** WARNING")
                || t.contains("post-quantum")
                || t.contains("store now, decrypt later")
                || t.contains("openssh.com/pq")
                || t.contains("may need to be upgraded")
                || t.starts_with("#< CLIXML")
                || t.starts_with("<Objs"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_output(o: &ExecOutput) -> String {
    const CAP: usize = 60_000;
    let code = if o.timed_out {
        "TIMEOUT".to_string()
    } else {
        o.code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
    };
    format!(
        "exit_code: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        tail(&o.stdout, CAP),
        tail(&o.stderr, CAP),
    )
}

/// Run a PowerShell command on the VM and capture stdout/stderr/exit code.
async fn run_ssh(
    command: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    timeout_secs: u64,
) -> Result<ExecOutput> {
    // Build a PowerShell script: set env, cd, run, then propagate the exit code.
    // SilentlyContinue on progress stops PowerShell from CLIXML-serializing its
    // "Preparing modules…" progress records into our captured stderr.
    let mut script =
        String::from("$ProgressPreference = 'SilentlyContinue';\n$ErrorActionPreference = 'Continue';\n");
    for (k, v) in env {
        script.push_str(&format!("$env:{k} = '{}';\n", ps_single_quote(v)));
    }
    let dir = cwd.unwrap_or(PROJECT_DRIVE);
    script.push_str(&format!("Set-Location -LiteralPath '{}';\n", ps_single_quote(dir)));
    script.push_str(command);
    // For native programs $LASTEXITCODE holds the code; cmdlets leave it null.
    script.push_str("\n$c = $LASTEXITCODE; if ($null -eq $c) { $c = 0 }; exit $c\n");

    let encoded = encode_powershell(&script);

    let mut cmd = Command::new("ssh");
    cmd.arg(SSH_HOST)
        .arg("powershell")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-EncodedCommand")
        .arg(&encoded)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn()?;
    match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(out) => {
            let out = out?;
            Ok(ExecOutput {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: clean_stderr(&String::from_utf8_lossy(&out.stderr)),
                code: out.status.code(),
                timed_out: false,
            })
        }
        // Timed out: the wait_with_output future is dropped, and kill_on_drop
        // terminates the ssh child (and thus the remote powershell).
        Err(_) => Ok(ExecOutput {
            stdout: String::new(),
            stderr: format!("command exceeded timeout of {timeout_secs}s and was killed"),
            code: None,
            timed_out: true,
        }),
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WinExecArgs {
    /// PowerShell command/script to run on the Windows 11 dev VM (win11).
    command: String,
    /// Working directory on Windows. Defaults to Z:\ (the shared project root).
    #[serde(default)]
    cwd: Option<String>,
    /// Extra environment variables to set before running the command.
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    /// Timeout in seconds. Defaults to 600.
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WinCargoArgs {
    /// Crate directory relative to the project root, e.g. "kmd" or "icd".
    /// The working directory becomes Z:\<crate_dir>.
    crate_dir: String,
    /// Arguments passed to cargo, e.g. ["make","--makefile","Cargo.make.toml"]
    /// or ["build","--release"].
    args: Vec<String>,
    /// Timeout in seconds. Defaults to 1800 (driver builds are slow).
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[tool_router]
impl WinHost {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Run a PowerShell command on the Windows 11 dev VM (win11) over SSH and return exit_code, stdout, and stderr. The Helios project tree is shared at Z:\\ (same files as the Linux side). Prefer this over raw ssh: it avoids cmd.exe quoting issues and uses a fresh environment."
    )]
    async fn win_exec(&self, Parameters(a): Parameters<WinExecArgs>) -> String {
        let env = a.env.unwrap_or_default();
        match run_ssh(&a.command, a.cwd.as_deref(), &env, a.timeout_secs.unwrap_or(600)).await {
            Ok(o) => format_output(&o),
            Err(e) => format!("error launching ssh: {e}"),
        }
    }

    #[tool(
        description = "Sync the project to the local build mirror and run cargo (or cargo make) there. The Z:\\ share cannot host cargo/wdk build IO (OS error 87), so this robocopy-mirrors Z:\\ -> C:\\Users\\Rupansh\\helios-vgpu (excluding target and .git) and builds inside the mirror with LIBCLANG_PATH set for bindgen. Edit sources on the Linux/Z:\\ side — the mirror is re-synced on every call. crate_dir is relative to the project root (e.g. \"kmd\"); args is the cargo argv (e.g. [\"make\",\"--makefile\",\"Cargo.make.toml\"] or [\"build\"])."
    )]
    async fn win_cargo(&self, Parameters(a): Parameters<WinCargoArgs>) -> String {
        let mut env = HashMap::new();
        env.insert("LIBCLANG_PATH".to_string(), LIBCLANG_PATH.to_string());
        // 1) mirror the tree to local disk (cargo/wdk IO fails on the share),
        // 2) cd into the crate in the mirror, 3) build with the local default target.
        let command = format!(
            "robocopy {PROJECT_DRIVE} {MIRROR_ROOT} /MIR /XD target .git /NFL /NDL /NJH /NJS /NP /R:1 /W:1 | Out-Null\n\
             if ($LASTEXITCODE -ge 8) {{ \"win_cargo: robocopy mirror sync failed (exit $LASTEXITCODE)\"; exit $LASTEXITCODE }}\n\
             Set-Location -LiteralPath '{MIRROR_ROOT}\\{}'\n\
             cargo {}",
            a.crate_dir,
            a.args.join(" "),
        );
        match run_ssh(&command, None, &env, a.timeout_secs.unwrap_or(1800)).await {
            Ok(o) => format_output(&o),
            Err(e) => format!("error launching ssh: {e}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for WinHost {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Runs commands and cargo/cargo-make builds on the Helios win11 dev VM. \
                 The project source is shared at Z:\\ (identical to the Linux tree), so \
                 edit files on Linux and build here."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Drop any stale SSH ControlMaster so the first build picks up the current
    // machine environment (PATH/vars updated by recent toolchain installs).
    let _ = Command::new("ssh").args(["-O", "exit", SSH_HOST]).output().await;

    let service = WinHost::new()
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;
    service.waiting().await?;
    Ok(())
}
