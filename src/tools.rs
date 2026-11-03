//! Workspace-scoped repository tools and explicitly approved mutations.
use crate::model::ToolCall;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const MAX_FILE: u64 = 256 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;

#[derive(Clone)]
pub struct Tools {
    root: PathBuf,
    pub shell: String,
    excluded_env: Vec<String>,
}

pub struct Prepared {
    pub title: String,
    pub preview: String,
    action: Action,
}
enum Action {
    Write {
        path: String,
        expected_target: PathBuf,
        original: Option<String>,
        content: String,
    },
    Shell {
        command: String,
        timeout: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    120
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"list_files","description":"List non-ignored workspace files. No arguments.","parameters":{"type":"object","properties":{},"additionalProperties":false}}),
        json!({"name":"read_file","description":"Read a UTF-8 workspace file, up to 256 KiB.","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}),
        json!({"name":"search","description":"Search literal text in non-ignored workspace files.","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}}),
        json!({"name":"write_file","description":"Propose creating or replacing a UTF-8 file; user reviews a diff before approval. Parent directory must exist.","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}}),
        json!({"name":"shell","description":"Propose a shell command in the workspace. Requires approval. No interactive commands.","parameters":{"type":"object","properties":{"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}},"required":["command"],"additionalProperties":false}}),
    ]
}

impl Tools {
    pub fn new(root: &Path, shell: Option<String>) -> Result<Self> {
        let root = root.canonicalize().context("workspace does not exist")?;
        if !root.is_dir() {
            bail!("workspace must be a directory");
        }
        let shell = shell.unwrap_or_else(|| {
            if cfg!(windows) {
                "powershell.exe".into()
            } else {
                std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
            }
        });
        Ok(Self {
            root,
            shell,
            excluded_env: Vec::new(),
        })
    }
    pub fn exclude_credentials(&mut self, names: impl IntoIterator<Item = String>) {
        self.excluded_env.extend(names);
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let rel = Path::new(path);
        if rel.as_os_str().is_empty()
            || rel.components().any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("path must be a workspace-relative path without traversal");
        }
        #[cfg(windows)]
        if path.contains(':') {
            bail!("alternate data streams are not supported");
        }
        let joined = self.root.join(rel);
        let target = if joined.exists() || joined.symlink_metadata().is_ok() {
            joined
                .canonicalize()
                .context("invalid file or dangling symlink")?
        } else {
            let parent = joined
                .parent()
                .context("missing parent directory")?
                .canonicalize()
                .context("parent directory must exist")?;
            parent.join(joined.file_name().context("missing file name")?)
        };
        if !target.starts_with(&self.root) {
            bail!("path escapes workspace");
        }
        if sensitive(rel) || sensitive(target.strip_prefix(&self.root)?) {
            bail!("credential and internal state files are excluded from repository tools");
        }
        Ok(target)
    }
    fn files(&self) -> Vec<PathBuf> {
        let root = self.root.clone();
        ignore::WalkBuilder::new(&self.root)
            .hidden(false)
            .require_git(false)
            .filter_entry(move |e| e.path().strip_prefix(&root).is_ok_and(|p| !sensitive(p)))
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .filter_map(|e| {
                e.path()
                    .strip_prefix(&self.root)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .filter(|p| !sensitive(p))
            .take(5000)
            .collect()
    }
    pub async fn read_only(&self, call: &ToolCall) -> Result<Option<String>> {
        match call.name.as_str() {
            "list_files" => {
                if call.arguments.as_object().is_none_or(|a| !a.is_empty()) {
                    bail!("list_files takes an empty object");
                }
                let this = self.clone();
                Ok(Some(
                    tokio::task::spawn_blocking(move || {
                        this.files()
                            .iter()
                            .map(|p| p.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .await?,
                ))
            }
            "read_file" => {
                let a: ReadArgs = serde_json::from_value(call.arguments.clone())?;
                let path = self.resolve(&a.path)?;
                Ok(Some(read(&path).await?))
            }
            "search" => {
                let a: SearchArgs = serde_json::from_value(call.arguments.clone())?;
                if a.query.is_empty() {
                    bail!("search query cannot be empty");
                }
                let this = self.clone();
                let files = tokio::task::spawn_blocking(move || this.files()).await?;
                let mut out = String::new();
                for file in files {
                    if out.len() >= MAX_OUTPUT {
                        out.push_str("\n[results truncated]");
                        break;
                    }
                    let path = self.resolve(&file.to_string_lossy())?;
                    if let Ok(text) = read(&path).await {
                        for (line, s) in text
                            .lines()
                            .enumerate()
                            .filter(|(_, s)| s.contains(&a.query))
                            .take(100)
                        {
                            use std::fmt::Write;
                            let excerpt: String = s.chars().take(1000).collect();
                            writeln!(out, "{}:{}:{excerpt}", file.display(), line + 1)?;
                            if out.len() >= MAX_OUTPUT {
                                break;
                            }
                        }
                    }
                }
                Ok(Some(out))
            }
            "write_file" | "shell" => Ok(None),
            _ => bail!("unknown tool: {}", call.name),
        }
    }
    pub async fn prepare(&self, call: &ToolCall) -> Result<Prepared> {
        match call.name.as_str() {
            "write_file" => {
                let a: WriteArgs = serde_json::from_value(call.arguments.clone())?;
                if a.content.len() > MAX_FILE as usize {
                    bail!("proposed file exceeds 256 KiB");
                }
                let path = self.resolve(&a.path)?;
                let old = if path.exists() {
                    Some(read(&path).await?)
                } else {
                    None
                };
                let original = old.as_deref().map(hash);
                let preview =
                    similar::TextDiff::from_lines(old.as_deref().unwrap_or(""), &a.content)
                        .unified_diff()
                        .context_radius(3)
                        .header(&format!("a/{}", a.path), &format!("b/{}", a.path))
                        .to_string();
                Ok(Prepared {
                    title: format!("Apply file change: {}", a.path),
                    preview,
                    action: Action::Write {
                        path: a.path,
                        expected_target: path,
                        original,
                        content: a.content,
                    },
                })
            }
            "shell" => {
                let a: ShellArgs = serde_json::from_value(call.arguments.clone())?;
                if a.command.trim().is_empty() || !(1..=600).contains(&a.timeout_seconds) {
                    bail!("shell requires a command and a timeout between 1 and 600 seconds");
                }
                Ok(Prepared {
                    title: "Execute command".into(),
                    preview: format!(
                        "shell: {}\ncwd: {}\ntimeout: {}s\n\n{}",
                        self.shell,
                        self.root.display(),
                        a.timeout_seconds,
                        a.command
                    ),
                    action: Action::Shell {
                        command: a.command,
                        timeout: a.timeout_seconds,
                    },
                })
            }
            _ => bail!("tool is not an approvable action"),
        }
    }
    pub async fn execute(&self, prepared: Prepared, cancel: &CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            bail!("cancelled");
        }
        match prepared.action {
            Action::Write {
                path,
                expected_target,
                original,
                content,
            } => {
                let target = self.resolve(&path)?;
                if target != expected_target {
                    bail!("file target changed since approval preview");
                }
                let current = if target.exists() {
                    Some(hash(&read(&target).await?))
                } else {
                    None
                };
                if current != original {
                    bail!("file changed since approval preview; request a fresh patch");
                }
                // Re-resolve immediately before writing to detect changed parent symlinks.
                if self.resolve(&path)? != target {
                    bail!("file target changed since preview");
                }
                tokio::fs::write(target, content).await?;
                Ok(format!("updated {path}"))
            }
            Action::Shell { command, timeout } => self.command(&command, timeout, cancel).await,
        }
    }
    async fn command(
        &self,
        command: &str,
        timeout: u64,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let mut cmd = tokio::process::Command::new(&self.shell);
        if cfg!(windows) {
            cmd.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ]);
        } else {
            cmd.args(["-c", command]);
        }
        cmd.current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        // Do not pass common API credentials to the child process.
        for (name, _) in
            std::env::vars().filter(|(n, _)| n.ends_with("_API_KEY") || n.ends_with("_TOKEN"))
        {
            cmd.env_remove(name);
        }
        for name in &self.excluded_env {
            cmd.env_remove(name);
        }
        let mut child = cmd.spawn().context("starting approved shell command")?;
        let stdout = child.stdout.take().context("missing stdout pipe")?;
        let stderr = child.stderr.take().context("missing stderr pipe")?;
        let mut out = tokio::spawn(capture(stdout));
        let mut err = tokio::spawn(capture(stderr));
        let status = tokio::select! {
            result=child.wait()=>result.context("waiting for shell command"),
            _=cancel.cancelled()=>{let _=child.kill().await;Err(anyhow::anyhow!("command cancelled; inspect workspace before retrying"))},
            _=tokio::time::sleep(Duration::from_secs(timeout))=>{let _=child.kill().await;Err(anyhow::anyhow!("command timed out; inspect workspace before retrying"))},
        };
        // A descendant can retain pipe handles; never await those indefinitely.
        let output = tokio::time::timeout(Duration::from_secs(2), async {
            Ok::<_, anyhow::Error>(((&mut out).await??, (&mut err).await??))
        })
        .await;
        if !matches!(&output, Ok(Ok(_))) {
            out.abort();
            err.abort();
        }
        let status = status?;
        let (stdout, stderr) = output.context("command descendants retained output pipes")??;
        Ok(format!(
            "exit: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "signal".into())
        ))
    }
}

async fn read(path: &Path) -> Result<String> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > MAX_FILE as usize {
        bail!("file exceeds 256 KiB");
    }
    String::from_utf8(bytes).context("file is not UTF-8 text")
}
async fn capture(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Result<String> {
    let mut output = Vec::with_capacity(MAX_OUTPUT);
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let keep = n.min(MAX_OUTPUT.saturating_sub(output.len()));
        output.extend_from_slice(&buf[..keep]);
        truncated |= keep < n;
    }
    let mut text = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    Ok(text)
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn sensitive(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(
            name.as_str(),
            ".git"
                | ".tools"
                | "target"
                | "node_modules"
                | ".forge"
                | ".aws"
                | ".ssh"
                | ".codex"
                | ".agents"
                | "credentials"
                | "credentials.json"
                | "secrets.toml"
                | "forge.db"
                | "config.toml"
        ) || name == ".env"
            || name.starts_with(".env.")
            || name.ends_with(".pem")
            || name.ends_with(".key")
            || name.ends_with(".db")
            || name.ends_with(".db-wal")
            || name.ends_with(".db-shm")
            || (name.starts_with("forge.") && name.ends_with(".lock"))
            || name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn commands_timeout_cancel_and_bound_output() {
        let d = tempfile::tempdir().unwrap();
        let t = Tools::new(d.path(), None).unwrap();
        let long = if cfg!(windows) {
            "Start-Sleep -Seconds 10"
        } else {
            "sleep 10"
        };
        let call = ToolCall {
            id: "cmd".into(),
            name: "shell".into(),
            arguments: json!({"command":long,"timeout_seconds":1}),
        };
        let p = t.prepare(&call).await.unwrap();
        let result = t.execute(p, &CancellationToken::new()).await.unwrap_err();
        assert!(result.to_string().contains("timed out"));
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let job = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let p = t.prepare(&call).await.unwrap();
        assert!(
            t.execute(p, &cancel)
                .await
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        job.await.unwrap();
        let command = if cfg!(windows) {
            "[Console]::Write(('x' * 100000))"
        } else {
            "head -c 100000 /dev/zero | tr '\\0' x"
        };
        let call = ToolCall {
            id: "output".into(),
            name: "shell".into(),
            arguments: json!({"command":command}),
        };
        let output = t
            .execute(t.prepare(&call).await.unwrap(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(output.contains("[output truncated]"));
        assert!(output.len() < MAX_OUTPUT + 200);
    }
    #[tokio::test]
    async fn traversal_and_stale_patch() {
        let d = tempfile::tempdir().unwrap();
        let t = Tools::new(d.path(), None).unwrap();
        assert!(t.resolve("../escape").is_err());
        assert!(t.resolve(".env").is_err());
        tokio::fs::write(d.path().join("a"), "before\n")
            .await
            .unwrap();
        let call = ToolCall {
            id: "1".into(),
            name: "write_file".into(),
            arguments: json!({"path":"a","content":"after\n"}),
        };
        let p = t.prepare(&call).await.unwrap();
        assert!(p.preview.contains("-before"));
        tokio::fs::write(d.path().join("a"), "external\n")
            .await
            .unwrap();
        assert!(t.execute(p, &CancellationToken::new()).await.is_err());
        assert_eq!(
            tokio::fs::read_to_string(d.path().join("a")).await.unwrap(),
            "external\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(b.path(), a.path().join("outside")).unwrap();
        let t = Tools::new(a.path(), None).unwrap();
        assert!(t.resolve("outside/new").is_err());
    }
}
