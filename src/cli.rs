//! Command-line entry points and configuration bootstrapping.
use crate::{
    accounting::{parse_usd, usd},
    config::{Config, Paths},
    credentials,
    model::{Event, EventKind},
    providers::HttpProvider,
    runtime::{Agent, Frontend},
    storage::Store,
    tools::Tools,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    io::{IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "forge",
    version,
    about = "A coding agent with explicit approvals and visible spending"
)]
struct Cli {
    /// Override the user config/data directory (useful for portable installs).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[arg(short = 'C', long, global = true, default_value = ".")]
    workspace: PathBuf,
    #[arg(short, long, global = true)]
    model: Option<String>,
    /// Compare two plans and refine task guidance before each turn (four additional model requests).
    #[arg(long, global = true)]
    refine: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single turn. Use '-' to read a prompt from stdin.
    Run {
        prompt: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        session: Option<String>,
    },
    /// List or resume saved conversations.
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Add/remove API keys in the operating system credential store.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List explicitly configured model aliases and prices.
    Models,
    /// Inspect request-level usage, including unresolved reservations.
    Usage {
        /// Resolve a charge after checking the provider's billing records.
        #[arg(long, requires="cost", conflicts_with_all=["session","project","day"])]
        resolve: Option<String>,
        /// Verified USD charge (0 for a confirmed unbilled request).
        #[arg(long, requires = "resolve")]
        cost: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        day: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Configure optional local monetary guards. Days are UTC.
    Budget {
        #[command(subcommand)]
        command: BudgetCommand,
    },
    /// Check configuration, workspace, and credential availability (no API calls).
    Doctor,
}
#[derive(Subcommand)]
enum SessionCommand {
    List,
    Resume {
        id: String,
    },
    /// Print the latest recorded refinement stages as JSON, newest first.
    Refinements {
        id: String,
    },
}
#[derive(Subcommand)]
enum AuthCommand {
    Add { alias: String },
    Remove { alias: String },
}
#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    Session,
    Project,
    Daily,
}
impl Scope {
    fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
            Self::Daily => "daily",
        }
    }
}
#[derive(Subcommand)]
enum BudgetCommand {
    List,
    Set {
        scope: Scope,
        /// Session ID or canonical project path; omit for daily.
        #[arg(long)]
        target: Option<String>,
        /// USD amount, e.g. 2.50.
        amount: String,
    },
    Remove {
        scope: Scope,
        #[arg(long)]
        target: Option<String>,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.command.is_some()
        && !matches!(
            &cli.command,
            Some(Command::Sessions {
                command: SessionCommand::Resume { .. }
            })
        )
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    }
    let paths = Paths::discover(cli.home.as_deref())?;
    let config = Config::load(&paths.config)?;
    let store = Store::open(&paths.database)?;
    match cli.command {
        Some(Command::Doctor) => {
            println!(
                "Forge {}\nconfig: {}\ndata: {}",
                env!("CARGO_PKG_VERSION"),
                paths.config.display(),
                paths.database.display()
            );
            let tools = Tools::new(&cli.workspace, config.shell.clone())?;
            println!(
                "workspace: {}\nshell: {}",
                tools.root().display(),
                tools.shell
            );
            if config.models.is_empty() {
                println!(
                    "No models configured. Copy config.example.toml to the config path above and fill in your model IDs, limits, and current pricing."
                );
            }
            for (alias, m) in &config.models {
                let status = match m.validate() {
                    Ok(()) => {
                        if credentials::resolve(alias, m).is_ok() {
                            "ready (not contacted)".into()
                        } else {
                            "credential unavailable".into()
                        }
                    }
                    Err(e) => format!("invalid: {e}"),
                };
                println!(
                    "{alias}: {status}; {}",
                    if m.pricing.is_some() {
                        "priced"
                    } else {
                        "unpriced"
                    }
                );
            }
            crate::config::project(tools.root())?;
            println!(
                "Budgets are local guards, not provider billing caps. API calls are never retried automatically."
            );
        }
        Some(Command::Models) => {
            for (alias, m) in &config.models {
                println!(
                    "{alias}  {:?} / {}  context={} output={}  {}",
                    m.provider,
                    m.model,
                    m.context_tokens,
                    m.max_output_tokens,
                    if m.pricing.is_some() {
                        "priced"
                    } else {
                        "unpriced"
                    }
                );
            }
        }
        Some(Command::Auth { command }) => match command {
            AuthCommand::Add { alias } => {
                if !config.models.contains_key(&alias) {
                    bail!("configure the model alias before adding its key");
                }
                let key = rpassword::prompt_password("API key (hidden): ")?;
                credentials::add(&alias, &key)?;
                println!("Credential saved to OS store for {alias}.");
            }
            AuthCommand::Remove { alias } => {
                credentials::remove(&alias)?;
                println!("Credential removed for {alias}.");
            }
        },
        Some(Command::Budget { command }) => match command {
            BudgetCommand::List => {
                for (scope, target, amount) in store.budgets()? {
                    println!("{scope} {target} {}", usd(Some(amount)));
                }
            }
            BudgetCommand::Set {
                scope,
                target,
                amount,
            } => {
                let target = budget_target(scope, target)?;
                store.set_budget(scope.name(), &target, Some(parse_usd(&amount)?))?;
                println!("{} budget set to {}", scope.name(), amount);
            }
            BudgetCommand::Remove { scope, target } => {
                let target = budget_target(scope, target)?;
                store.set_budget(scope.name(), &target, None)?;
                println!("Budget removed.");
            }
        },
        Some(Command::Usage {
            resolve,
            cost,
            session,
            project,
            day,
            json,
        }) => {
            if let Some(id) = resolve {
                let cost = parse_usd(cost.as_deref().context("--cost is required")?)?;
                store.resolve_charge(&id, cost)?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"request":id,"cost_nusd":cost,"state":"complete"})
                    );
                } else {
                    println!("Charge reconciled: {id} {}", usd(Some(cost)));
                }
                return Ok(());
            }
            let project = project
                .map(|p| p.canonicalize())
                .transpose()?
                .map(|p| p.to_string_lossy().into_owned());
            let rows: Vec<_> = store
                .ledger()?
                .into_iter()
                .filter(|r| {
                    session.as_ref().is_none_or(|s| &r.session == s)
                        && project.as_ref().is_none_or(|p| &r.project == p)
                        && day.as_ref().is_none_or(|d| &r.day == d)
                })
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                let mut total = 0u64;
                let mut unresolved = 0;
                let mut unpriced = 0;
                let mut input = 0u64;
                let mut output = 0u64;
                let mut cached = 0u64;
                let mut reserved = 0u64;
                for r in rows {
                    if let Some(u) = &r.usage {
                        input = input.saturating_add(u.input);
                        output = output.saturating_add(u.output);
                        cached = cached.saturating_add(u.cached_input);
                    }
                    if r.state != "complete" {
                        reserved = reserved.saturating_add(r.reserved_nusd.unwrap_or(0));
                    }
                    total = total.saturating_add(r.cost_nusd.unwrap_or(0));
                    unresolved += usize::from(r.state != "complete");
                    unpriced += usize::from(r.cost_nusd.is_none());
                    let tokens = r
                        .usage
                        .as_ref()
                        .map(|u| {
                            format!("in={} out={} cached={}", u.input, u.output, u.cached_input)
                        })
                        .unwrap_or_else(|| format!("estimated_input={}", r.estimated_input));
                    println!(
                        "{} {} {} {} {} reserved={}\n  session={} project={}",
                        r.id,
                        r.day,
                        r.state,
                        tokens,
                        usd(r.cost_nusd),
                        usd(r.reserved_nusd),
                        r.session,
                        r.project
                    );
                }
                println!(
                    "Priced total: {}; unresolved requests: {unresolved}; unpriced/unknown charges: {unpriced}",
                    usd(Some(total))
                );
                println!(
                    "Reported tokens: {input} input · {output} output · {cached} cached; unresolved reserved: {}",
                    usd(Some(reserved))
                );
            }
        }
        Some(Command::Sessions {
            command: SessionCommand::List,
        }) => {
            for s in store.sessions()? {
                println!("{}  {}  {}  {}", s.id, s.created, s.model, s.project);
            }
        }
        Some(Command::Sessions {
            command: SessionCommand::Refinements { id },
        }) => println!(
            "{}",
            serde_json::to_string_pretty(&store.refinements(&id)?)?
        ),
        Some(Command::Run {
            prompt,
            json,
            session,
        }) => {
            let mut prompt = prompt;
            if prompt == "-" {
                prompt.clear();
                std::io::stdin().read_to_string(&mut prompt)?;
            }
            let frontend = Arc::new(Console {
                json,
                interactive: !json && std::io::stdin().is_terminal(),
            });
            let mut agent = build(
                &config,
                store,
                &cli.workspace,
                cli.model.as_deref(),
                session.as_deref(),
                frontend,
            )?;
            agent.refine = cli.refine;
            let preferences = crate::config::project(agent.tools.root())?;
            agent
                .initialize(preferences.instructions.as_deref())
                .await?;
            let cancel = agent.cancel.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel.cancel();
                }
            });
            let result = agent.turn(&prompt).await;
            signal.abort();
            let status = result?;
            if status == "approval_required" {
                std::process::exit(2);
            }
        }
        command => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!("interactive UI needs a terminal; use forge run PROMPT --json");
            }
            let resume = match command {
                Some(Command::Sessions {
                    command: SessionCommand::Resume { id },
                }) => Some(id),
                _ => None,
            };
            let (frontend, rx) = crate::ui::channel();
            let mut agent = build(
                &config,
                store,
                &cli.workspace,
                cli.model.as_deref(),
                resume.as_deref(),
                frontend,
            )?;
            agent.refine = cli.refine;
            let preferences = crate::config::project(agent.tools.root())?;
            agent
                .initialize(preferences.instructions.as_deref())
                .await?;
            crate::ui::run(agent, rx).await?;
        }
    }
    Ok(())
}

fn budget_target(scope: Scope, target: Option<String>) -> Result<String> {
    match scope {
        Scope::Daily => {
            if target.is_some() {
                bail!("daily budget applies across projects; omit --target");
            }
            Ok("*".into())
        }
        Scope::Project => Ok(PathBuf::from(
            target.context("project scope requires --target PATH")?,
        )
        .canonicalize()?
        .to_string_lossy()
        .into_owned()),
        Scope::Session => target.context("session scope requires --target SESSION_ID"),
    }
}

fn build(
    config: &Config,
    store: Store,
    workspace: &std::path::Path,
    alias: Option<&str>,
    resume: Option<&str>,
    frontend: Arc<dyn Frontend>,
) -> Result<Agent> {
    let saved = resume.map(|id| store.session(id)).transpose()?;
    if let (Some(s), Some(alias)) = (&saved, alias)
        && s.model != alias
    {
        bail!("resuming a session requires its original model alias");
    }
    let (alias, model) = config.model(saved.as_ref().map(|s| s.model.as_str()).or(alias))?;
    let root = saved
        .as_ref()
        .map(|s| PathBuf::from(&s.project))
        .unwrap_or_else(|| workspace.into());
    let mut tools = Tools::new(&root, config.shell.clone())?;
    tools.exclude_credentials(config.models.values().filter_map(|m| m.key_env.clone()));
    let provider = Arc::new(HttpProvider::new(
        model.clone(),
        credentials::resolve(alias, model)?,
    )?);
    let session = match saved {
        Some(s) => s.id,
        None => store.create_session(&tools.root().to_string_lossy(), alias)?,
    };
    Ok(Agent {
        refine: false,
        store,
        session,
        model: model.clone(),
        provider,
        tools,
        frontend,
        cancel: CancellationToken::new(),
    })
}

struct Console {
    json: bool,
    interactive: bool,
}
#[async_trait]
impl Frontend for Console {
    async fn event(&self, event: Event) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(&event)?);
            std::io::stdout().flush()?;
            return Ok(());
        }
        match event.kind {
            EventKind::Started => eprintln!("session: {}", event.session),
            EventKind::Text { text } => {
                print!("{text}");
                std::io::stdout().flush()?;
            }
            EventKind::Activity { text } => eprintln!("\n· {text}"),
            EventKind::Usage {
                reported: Some(u),
                cost_nusd,
                warning,
                ..
            } => eprintln!(
                "\n{} in · {} out · {} cached · {}{}",
                u.input,
                u.output,
                u.cached_input,
                usd(cost_nusd),
                if warning { " · budget ≥80%" } else { "" }
            ),
            EventKind::Finished { status } => eprintln!("\n{status}"),
            EventKind::Error { message } => eprintln!("\n{message}"),
            _ => {}
        }
        Ok(())
    }
    async fn approve(&self, _id: &str, title: &str, preview: &str) -> Result<Option<bool>> {
        if !self.interactive {
            return Ok(None);
        }
        let title = title.to_string();
        let preview = preview.to_string();
        tokio::task::spawn_blocking(move || {
            eprintln!("\n{title}\n{preview}\nApprove? [y/N]");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            Ok(Some(line.trim().eq_ignore_ascii_case("y")))
        })
        .await?
    }
}
