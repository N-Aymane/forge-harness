# Forge setup and operation guide

This guide takes you from a clean machine to an approved coding-agent turn. Forge is a Rust application; Python is needed only for the optional terminal test and screenshot capture.

## 1. Choose how to install

| Method | Use it when |
| --- | --- |
| Build from source | You want a local build, development tools, or compatibility with your system libraries. |
| GitHub Actions artifact | You want a binary built by the repository's Windows, macOS, or Linux workflow. |

Source: [N-Aymane/forge-harness](https://github.com/N-Aymane/forge-harness). Binary artifacts are attached to successful runs on the [Actions page](https://github.com/N-Aymane/forge-harness/actions/workflows/ci.yml). They are build artifacts, not a published package-manager release.

### Windows prerequisites

1. Install Git for Windows if `git --version` is unavailable.
2. Install [Rust through rustup](https://rust-lang.org/tools/install/), using the default MSVC toolchain.
3. Install Visual Studio Build Tools and the **Desktop development with C++** workload, including a Windows SDK. The [official Windows Rust setup guide](https://learn.microsoft.com/en-us/windows/dev-environment/rust/setup) explains these prerequisites.
4. Reopen PowerShell and verify:

```powershell
git --version
rustc --version
cargo --version
```

Forge requires Rust **1.88 or later**, with the 2024 edition. You do not need to install MinGW for the default MSVC setup.

### Linux prerequisites

On Ubuntu/Debian:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libdbus-1-dev curl ca-certificates
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
rustc --version
cargo --version
```

The D-Bus headers are needed to compile Linux Secret Service support. A running desktop Secret Service is needed only if you choose to store keys there. Headless machines can use environment variables instead.

### macOS prerequisites

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
rustc --version
cargo --version
```

Finish the Command Line Tools installer before building. Keychain supplies the native credential store.

## 2. Build and install

These commands work in PowerShell, Bash, and Zsh:

```text
git clone https://github.com/N-Aymane/forge-harness.git
cd forge-harness
cargo build --locked --release
cargo install --path . --locked
forge --version
forge doctor
```

The version should be `forge 0.1.0`. `cargo install` puts the executable in Cargo's binary directory, normally `~/.cargo/bin`. If that directory is not on PATH, reopen your terminal or invoke the executable directly:

```powershell
# Windows, from the repository:
.\target\release\forge.exe doctor
```

```bash
# Linux/macOS, from the repository:
./target/release/forge doctor
```

For a GitHub Actions artifact, download the archive matching your operating system and architecture, extract it, then run `forge --version` from the extracted directory. Put the executable in a directory on PATH if you want to invoke it globally. Linux binaries use the build runner's system libraries; build locally if your distribution is older or incompatible.

## 3. Create your configuration

`forge doctor` prints the exact user configuration and data paths. Copy `config.example.toml` from this repository to the printed **config** path, creating its parent directory if necessary. Edit the copied file, not the example tracked in Git.

For a completely predictable portable setup, choose a private user-owned directory and pass `--home` on every command:

```powershell
$forgeHome = Join-Path $env:LOCALAPPDATA 'ForgePortable'
New-Item -ItemType Directory -Force $forgeHome | Out-Null
Copy-Item config.example.toml (Join-Path $forgeHome 'config.toml')
forge --home $forgeHome doctor
```

```bash
forge_home="$HOME/.forge-portable"
mkdir -p "$forge_home"
cp config.example.toml "$forge_home/config.toml"
forge --home "$forge_home" doctor
```

With `--home`, the configuration is `config.toml` and the database is `forge.db` in that directory. Store it outside repositories you intend to share.

### Select a provider and model

Every model alias explicitly specifies its API protocol, endpoint, model ID, context limit, output limit, and credential source. There are no automatic model or provider substitutions.

| `provider` | API protocol | Example `base_url` |
| --- | --- | --- |
| `openai` | OpenAI Responses | `https://api.openai.com/v1` |
| `anthropic` | Anthropic Messages | `https://api.anthropic.com/v1` |
| `compatible` | OpenAI-compatible Chat Completions | Your server's base URL, including its API prefix |

The adapter appends `responses`, `messages`, or `chat/completions` respectively. Do not include that final endpoint in `base_url`. Remote endpoints require HTTPS; HTTP is allowed on loopback for local servers.

Start with one alias:

```toml
default_model = "primary"

[models.primary]
provider = "openai"
model = "REPLACE_WITH_A_MODEL_AVAILABLE_TO_YOUR_ACCOUNT"
base_url = "https://api.openai.com/v1"
context_tokens = 128000
max_output_tokens = 4096
```

The limits above are illustrative. Set them to the selected model's supported limits; the context limit must exceed the positive output limit. Consult your provider's current model documentation. For Anthropic, change the protocol and base URL and use a Claude model ID available to your account. Compatible servers must support streaming and the tool-calling protocol for coding actions.

`forge models` lists aliases. `forge --model ALIAS` overrides the default for a new session. Unused aliases can remain in the example configuration, but the selected alias needs valid credentials.

## 4. Add an API key

### Recommended on a desktop: the OS credential store

Ensure the selected model has **no `key_env` line**, then run:

```text
forge auth add primary
forge doctor
```

The hidden prompt saves the key in Windows Credential Manager, macOS Keychain, or Linux Secret Service. It is keyed by model alias. When using a portable directory, include it consistently:

```powershell
forge --home $forgeHome auth add primary
forge --home $forgeHome doctor
```

Remove a stored key with `forge auth remove primary`. That does not delete or revoke the key at the provider.

### Environment variables for CI or headless use

Add this line inside the model's table:

```toml
key_env = "OPENAI_API_KEY"
```

Set that variable in the process that launches Forge. In PowerShell, a hidden prompt avoids putting the actual key in the command history:

```powershell
$forgeKey = Read-Host 'API key' -AsSecureString
$env:OPENAI_API_KEY = [System.Net.NetworkCredential]::new('', $forgeKey).Password
forge doctor
```

In Bash:

```bash
read -rsp 'API key: ' OPENAI_API_KEY
printf '\n'
export OPENAI_API_KEY
forge doctor
```

Use `ANTHROPIC_API_KEY` or a custom variable if configured. `key_env` takes precedence by selecting that credential source: when it is set, Forge requires that environment variable and does not fall back to the OS store. Local servers that require no real key can use an explicitly configured placeholder environment value.

Only API-key authentication is implemented. Subscription-account OAuth is not implemented. API keys never belong in committed TOML, screenshots, issue reports, or command-line arguments.

## 5. Run your first coding turn

Open a small test repository or disposable directory first:

```text
forge -C /path/to/repository
```

On Windows, for example:

```powershell
forge -C 'C:\work\my-project'
```

Type a task such as: `Inspect this repository and propose a small README improvement.` Reads and searches run without approval. A proposed write opens a unified diff; a shell action shows its exact shell, command, working directory, and timeout. Press **y** to approve or **n** to deny. Enter submits a message; Alt+Enter inserts a newline where supported. Use Ctrl+P for the command palette, Tab for panels, and Page Up/Page Down to scroll.

The terminal images in [the screenshot walkthrough](SCREENSHOTS.md) come from the actual automated PTY test. The test uses a deterministic local provider, so no paid provider is being represented as tested by those images.

## 6. Configure pricing and spending limits

Without configured pricing, costs display **unpriced**. Token usage can still be recorded. A monetary budget blocks an unpriced model rather than treating it as free.

Rates are integer **nano-USD per token**. Convert a published USD-per-million-token rate by multiplying it by **1,000**. For example, a hypothetical $2 per million input tokens becomes `2000` nano-USD per input token. The following numbers are examples, not provider price claims:

```toml
[models.primary.pricing]
input_nusd_per_token = 2000
output_nusd_per_token = 8000
cached_input_nusd_per_token = 200
# Required when your provider reports separately billed cache writes:
cache_write_nusd_per_token = 2500
```

Verify current rates with your provider before setting them. A genuinely free local model can use explicit zero rates.

```text
forge budget set daily 5.00
forge budget set project 2.50 --target /path/to/repository
forge budget list
forge usage
```

After creating a session, you can also set a session-specific limit:

```text
forge sessions list
forge budget set session 1.00 --target SESSION_ID
forge usage --session SESSION_ID --json
```

All applicable limits must allow each request. The daily boundary uses UTC and applies across projects. Before a request, Forge transactionally reserves estimated input plus the maximum configured output cost. Concurrent processes cannot reserve the same remaining balance. The UI warns at 80% of an applicable limit.

Local estimates use serialized UTF-8 size plus headroom; they are deliberately conservative, not tokenizer-exact. Actual provider-reported usage reconciles the reservation. Reasoning tokens are not charged a second time on top of output tokens. Rates are snapshotted per request.

**Budgets are local guards, not guaranteed provider billing caps.** External API clients, provider accounting, or interrupted responses can differ from local estimates. Missing usage and interrupted requests retain their reservation. Inspect provider billing before retrying; Forge does not automatically retry a potentially billed request.

```text
forge usage --resolve REQUEST_ID --cost 0.0025
```

That command explicitly records a verified charge. Use zero only after confirming that the provider did not bill the request. Remove a limit with `forge budget remove daily`, or supply the original scope/target for project and session limits.

## 7. Sessions, context, and automation

```text
forge sessions list
forge sessions resume SESSION_ID
forge run "Review the changes" --session SESSION_ID
```

Resume restores the saved workspace and model alias. Missing tool outcomes are marked interrupted/unknown and are never automatically replayed. Inspect the workspace before proposing a replacement for an interrupted mutation.

At 80% of configured input capacity, Forge offers a paid summary of older context. It asks before sending that request, applies the same budget controls, retains full history, and preserves system instructions and the latest user turn/tool exchanges. It blocks oversized input if summarization is declined or cannot fit.

For scripts:

```bash
forge run "Explain the project" --json
forge run - --json < prompt.txt
```

Every JSONL envelope has `version`, `session`, and `type`. Noninteractive/JSON runs stop with `approval_required` and exit **2** when an action needs approval. Success is exit **0**; errors are exit **1**. There is no unattended approval bypass.

Project preferences can go in `.forge.toml`:

```toml
instructions = "Follow the existing conventions and run the relevant tests."
```

Project configuration cannot override credential destinations, endpoints, shells, or approval requirements. User configuration can select a shell; otherwise Forge uses PowerShell on Windows and the configured POSIX shell on Linux/macOS.

## 8. Verify your installation and reproduce the screenshots

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

On Linux/macOS, the basic terminal smoke test uses only Python's standard library:

```bash
python3 tests/tui_smoke.py target/debug/forge
```

To save PNGs and terminal evidence on Linux:

```bash
sudo apt-get install -y fonts-dejavu-core
python3 -m pip install -r tests/requirements-screenshots.txt
python3 tests/tui_smoke.py target/debug/forge --screenshots target/screenshots
cargo test --locked --all-targets > target/test-results.txt 2>&1
python3 tests/render_test_log.py target/test-results.txt target/screenshots/05-test-results.png
```

In an externally managed system Python, use a virtual environment before installing the optional screenshot dependencies. The CI workflow uses a dedicated Python environment. `FORGE_SCREENSHOT_FONT` can select another monospace TrueType font when using the renderer elsewhere.

The screenshots are PNG renderings of the captured terminal byte stream, interpreted by pyte with a fixed dark theme and drawn by Pillow. Each image has matching `.ansi`, `.txt`, and `.json` evidence, including a SHA-256 hash of the original terminal bytes. They are not generated UI mockups.

GitHub Actions builds/tests Windows, Linux, and macOS; checks the MSRV; captures the Linux terminal walkthrough; and uploads screenshots and binary archives. Optional real-provider tests are disabled by default and require explicit config/model selection, configured pricing, and a $0.01 local reservation budget. See `tests/live.rs`.

## 9. Troubleshooting

| Symptom | Action |
| --- | --- |
| `cargo` or `forge` is not found | Reopen the terminal, check Cargo's bin directory on PATH, or invoke the built executable by path. |
| A different `forge` program starts | Check `forge --version` and invoke this repository's executable directly. |
| `link.exe` / C compiler missing | Install the platform build prerequisites above. |
| Linux D-Bus build error | Install `pkg-config` and the D-Bus development headers. |
| Credential service unavailable | Use an explicitly configured `key_env` variable on a headless machine. |
| Key was saved but the variable is still required | Remove `key_env` to select the OS store, or set the named variable. |
| Model not found / HTTP 401 or 403 | Check the selected alias, model access, key, endpoint, and account permissions. HTTP bodies are omitted to protect secrets. |
| Budget blocks an apparently cheap request | Reservations include maximum output and conservative input. Inspect limits and unresolved charges. |
| `unpriced model cannot be used` | Configure verified pricing or remove the monetary limit; unpriced does not mean zero. |
| Context cannot be summarized | Start a new session, reduce input, or explicitly select a model with a suitable context limit. |
| Path rejected | Use a workspace-relative path; repository tools exclude traversal, symlink escapes, credential files, and internal state. |
| New file's parent is missing | Ask for an approved directory-creation command before the file write. |
| Noninteractive run exits 2 | This is an approval-required stop; review interactively rather than expecting automatic tool execution. |
| Windows error 4551 / Code Integrity rejection | Use an allowed build environment or CI artifacts and consult your device administrator. Forge does not change signing/security policy. |

## Execution and data boundaries

File reads/writes must be UTF-8 and are limited to 256 KiB. Writes are previewed and rechecked for changed contents/targets. Shell output is bounded; the default timeout is 120 seconds, with a maximum of 600 seconds.

Approved commands run with your OS permissions: approval is **not an OS sandbox**. Commands may affect files/network outside the workspace. Cancellation kills the immediate shell; descendants can survive. Inspect the state before retrying interrupted commands. Common API-key/token variables and configured credential variables are removed from child environments.

Conversation history, approvals, tool results, and usage live in the local plaintext SQLite database. Keep that directory private. The active API key is redacted from provider text/tool output, including split stream chunks. No cloud session synchronization, MCP/plugins, subagents, automatic model routing, or subscription-account login is implemented in v0.1.
