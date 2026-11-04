//! Update a Forge source checkout from its canonical upstream.
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

const UPSTREAM: &str = "https://github.com/N-Aymane/forge-harness.git";

pub async fn update_source() -> Result<String> {
    let root = source_root()?;
    let git_root = PathBuf::from(git(&root, &["rev-parse", "--show-toplevel"]).await?);
    ensure!(
        git_root.canonicalize()? == root,
        "choose the root of the Forge checkout"
    );
    let origin = git(&root, &["remote", "get-url", "origin"]).await?;
    ensure!(
        matches!(
            origin.as_str(),
            UPSTREAM
                | "https://github.com/N-Aymane/forge-harness"
                | "git@github.com:N-Aymane/forge-harness.git"
                | "ssh://git@github.com/N-Aymane/forge-harness.git"
        ),
        "Forge checkout origin must point to N-Aymane/forge-harness"
    );
    fast_forward(&root, UPSTREAM).await
}

async fn fast_forward(root: &Path, upstream: &str) -> Result<String> {
    let before = clean_main(root).await?;
    git(
        root,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            upstream,
            "refs/heads/main",
        ],
    )
    .await?;
    let fetched = git(root, &["rev-parse", "--verify", "FETCH_HEAD^{commit}"]).await?;
    ensure!(
        clean_main(root).await? == before,
        "Forge checkout changed during the fetch"
    );
    git(
        root,
        &[
            "merge",
            "--ff-only",
            "--no-autostash",
            "--no-overwrite-ignore",
            &fetched,
        ],
    )
    .await?;
    let after = git(root, &["rev-parse", "HEAD"]).await?;
    if before == after {
        return Ok("Forge source already contains GitHub main.".into());
    }
    Ok("Forge source updated. Rebuild or reinstall from the checkout, then restart Forge.".into())
}

fn source_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FORGE_SOURCE_DIR") {
        let path = PathBuf::from(path);
        ensure!(
            path.is_absolute(),
            "FORGE_SOURCE_DIR must be an absolute path"
        );
        return path.canonicalize().context("open FORGE_SOURCE_DIR");
    }
    let exe = std::env::current_exe().context("locate Forge executable")?;
    for path in exe.ancestors().skip(1) {
        if path.file_name().is_some_and(|name| name == "target")
            && let Some(root) = path.parent()
            && root.join(".git").exists()
            && root.join("Cargo.toml").is_file()
        {
            return root.canonicalize().context("open Forge source checkout");
        }
    }
    bail!(
        "Set FORGE_SOURCE_DIR to the absolute path of your Forge Git checkout, then restart Forge"
    )
}

async fn clean_main(root: &Path) -> Result<String> {
    ensure!(
        git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await? == "main",
        "switch the Forge checkout to main before updating"
    );
    ensure!(
        git(root, &["status", "--porcelain", "--untracked-files=normal"])
            .await?
            .is_empty(),
        "commit or stash changes in the Forge checkout before updating"
    );
    git(root, &["rev-parse", "HEAD"]).await
}

async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .context("Git update timed out")?
        .context("run Git for Forge update")?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args[0],
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_fast_forwards_clean_main_and_preserves_local_edits() -> Result<()> {
        let upstream = tempfile::tempdir()?;
        let checkout = tempfile::tempdir()?;
        git(upstream.path(), &["init", "--initial-branch=main"]).await?;
        git(upstream.path(), &["config", "user.name", "Forge Test"]).await?;
        git(
            upstream.path(),
            &["config", "user.email", "forge@example.invalid"],
        )
        .await?;
        let upstream_file = upstream.path().join("version.txt");
        std::fs::write(&upstream_file, "first")?;
        std::fs::write(upstream.path().join(".gitignore"), "local.txt\n")?;
        git(upstream.path(), &["add", "version.txt", ".gitignore"]).await?;
        git(upstream.path(), &["commit", "-m", "first version"]).await?;
        let upstream_path = upstream
            .path()
            .to_str()
            .context("upstream path must be UTF-8")?;
        git(checkout.path(), &["clone", "--", upstream_path, "."]).await?;
        std::fs::write(&upstream_file, "second")?;
        std::fs::write(upstream.path().join("local.txt"), "upstream")?;
        git(upstream.path(), &["add", "-f", "local.txt"]).await?;
        git(upstream.path(), &["commit", "-am", "second version"]).await?;

        let checkout_file = checkout.path().join("version.txt");
        std::fs::write(&checkout_file, "local edit")?;
        let error = fast_forward(checkout.path(), upstream_path)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("commit or stash"));
        assert_eq!(std::fs::read_to_string(&checkout_file)?, "local edit");

        std::fs::write(&checkout_file, "first")?;
        let ignored_file = checkout.path().join("local.txt");
        std::fs::write(&ignored_file, "local settings")?;
        assert!(fast_forward(checkout.path(), upstream_path).await.is_err());
        assert_eq!(std::fs::read_to_string(&ignored_file)?, "local settings");

        std::fs::remove_file(ignored_file)?;
        fast_forward(checkout.path(), upstream_path).await?;
        assert_eq!(std::fs::read_to_string(&checkout_file)?, "second");
        Ok(())
    }
}
