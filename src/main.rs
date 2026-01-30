//! devaipod - Sandboxed AI coding agents in reproducible dev environments
//!
//! This tool uses DevPod for container provisioning and adds AI agent sandboxing.

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, CommandFactory, Parser};
use color_eyre::eyre::{bail, Context, Result};
use dialoguer::Input;

mod config;
mod devcontainer;
mod forge;
mod git;
#[allow(dead_code)] // Preparatory infrastructure for GPU passthrough
mod gpu;
mod init;
mod pod;
mod podman;
mod proxy;
mod secrets;
mod service_gator;

/// Prefix for all devaipod pod names
const POD_NAME_PREFIX: &str = "devaipod-";

/// Normalize a workspace name to a full pod name by adding the prefix
///
/// The user-facing "short name" is what's shown by `devaipod list` and suggested
/// after `devaipod up` (the pod name with the prefix stripped). This function
/// always adds the prefix to convert back to the full pod name.
fn normalize_pod_name(name: &str) -> String {
    format!("{}{}", POD_NAME_PREFIX, name)
}

/// Strip the prefix from a pod name for display
fn strip_pod_prefix(name: &str) -> &str {
    name.strip_prefix(POD_NAME_PREFIX).unwrap_or(name)
}

/// Sanitize a name for use in pod names (alphanumeric and hyphens only)
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Generate a short unique suffix for pod names
fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Use lower 24 bits of timestamp in seconds + some randomness from nanos
    // This gives us a short but reasonably unique suffix
    let val = (now.as_secs() & 0xFFFFFF) ^ ((now.subsec_nanos() as u64) & 0xFFFF);
    format!("{:x}", val)
}

/// Create a pod name from a project name
///
/// Always generates a unique name to avoid conflicts with existing pods.
fn make_pod_name(project_name: &str) -> String {
    format!(
        "{}{}-{}",
        POD_NAME_PREFIX,
        sanitize_name(project_name),
        unique_suffix()
    )
}

/// Create a pod name for a PR
///
/// Always generates a unique name to avoid conflicts with existing pods.
fn make_pr_pod_name(repo: &str, pr_number: u64) -> String {
    format!(
        "{}{}-pr{}-{}",
        POD_NAME_PREFIX,
        sanitize_name(repo),
        pr_number,
        unique_suffix()
    )
}

// =============================================================================
// Host CLI - commands that run on the host machine (outside devcontainer)
// =============================================================================

#[derive(Debug, Parser)]
#[command(name = "devaipod")]
#[command(about = "Sandboxed AI coding agents in reproducible dev environments", long_about = None)]
struct HostCli {
    /// Path to config file (default: ~/.config/devaipod.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Enable verbose output (debug logging)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Quiet mode (only show warnings and errors)
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: HostCommand,
}

/// Mode of workspace creation (run vs up)
#[derive(Debug, Clone, Copy, Default)]
enum WorkspaceMode {
    /// Created with 'devaipod up' - interactive/manual control
    #[default]
    Up,
    /// Created with 'devaipod run' - automated task execution
    Run,
}

impl WorkspaceMode {
    fn as_str(&self) -> &'static str {
        match self {
            WorkspaceMode::Up => "up",
            WorkspaceMode::Run => "run",
        }
    }
}

/// Common CLI options for workspace creation commands
#[derive(Debug, Args)]
struct UpOptions {
    /// Task description for the AI agent (also stored as workspace description)
    #[arg(value_name = "TASK")]
    task: Option<String>,
    /// Store task description but don't send it to the agent as a prompt
    #[arg(short = 'n', long)]
    no_prompt: bool,
    /// Generate configuration files but don't start containers
    #[arg(long)]
    dry_run: bool,
    /// SSH into workspace after starting
    #[arg(short = 'S', long)]
    ssh: bool,
    /// Internal: mode of workspace creation (not exposed as CLI arg)
    #[arg(skip)]
    mode: WorkspaceMode,
    /// Use a specific container image instead of building from devcontainer.json
    ///
    /// This allows working with git repositories that have no devcontainer.json.
    /// The image must already exist locally or be pullable.
    #[arg(long, value_name = "IMAGE")]
    image: Option<String>,
    /// Explicit pod name (default: derived from source with unique suffix)
    ///
    /// Use this for predictable pod names, e.g. in CI/CD or testing.
    /// The devaipod- prefix will be added automatically if not present.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Configure service-gator scopes for AI agent access to external services.
    ///
    /// Format: service:scope where service is github, gitlab, jira, etc.
    /// Can be specified multiple times.
    ///
    /// Examples:
    ///   --service-gator=github:readonly-all       # Read-only access to all GitHub repos
    ///   --service-gator=github:myorg/myrepo       # Read access to specific repo
    ///   --service-gator=github:myorg/*            # Read access to all repos in org
    ///   --service-gator=github:myorg/repo:write   # Write access to specific repo
    #[arg(long = "service-gator", value_name = "SCOPE")]
    service_gator_scopes: Vec<String>,
    /// Use a specific image for the service-gator container.
    ///
    /// This allows testing locally-built service-gator images instead of
    /// pulling from ghcr.io/cgwalters/service-gator:latest.
    ///
    /// Example:
    ///   --service-gator-image localhost/service-gator:dev
    #[arg(long, value_name = "IMAGE")]
    service_gator_image: Option<String>,
}

/// Internal options for workspace creation (like `podman create` vs `podman run`)
///
/// This struct captures all the options needed to create a workspace pod without
/// starting it or performing post-setup actions like SSH. It's used by the common
/// `create_workspace` function that both `up` and `run` commands call.
#[derive(Debug, Clone)]
struct CreateOptions {
    /// Task description for the AI agent
    task: Option<String>,
    /// Use a specific container image instead of building from devcontainer.json
    image: Option<String>,
    /// Explicit pod name (default: derived from source with unique suffix)
    name: Option<String>,
    /// Service-gator scopes for AI agent access to external services
    service_gator_scopes: Vec<String>,
    /// Custom service-gator container image
    service_gator_image: Option<String>,
    /// Mode of workspace creation (up vs run)
    mode: WorkspaceMode,
}

impl CreateOptions {
    /// Build CreateOptions from UpOptions
    fn from_up_options(opts: &UpOptions) -> Self {
        Self {
            task: opts.task.clone(),
            image: opts.image.clone(),
            name: opts.name.clone(),
            service_gator_scopes: opts.service_gator_scopes.clone(),
            service_gator_image: opts.service_gator_image.clone(),
            mode: opts.mode,
        }
    }
}

#[derive(Debug, Parser)]
enum HostCommand {
    /// Create/start a workspace with AI agent
    ///
    /// Creates a podman pod with workspace and agent containers. The agent runs
    /// opencode in server mode and can be given tasks to work on.
    ///
    /// For remote URLs (GitHub repos/PRs), service-gator is automatically enabled
    /// with read + draft PR permissions for that repository.
    ///
    /// Examples:
    ///   devaipod up .                                      # Local repo
    ///   devaipod up . -S                                   # Local repo, SSH in after
    ///   devaipod up https://github.com/user/repo           # Remote repo
    ///   devaipod up https://github.com/user/repo/pull/123  # PR
    ///   devaipod up . 'fix the bug'                        # With task for agent
    ///   devaipod up . --service-gator=github:myorg/*       # Custom permissions
    Up {
        /// Source: local path, git URL, or PR URL
        source: String,
        #[command(flatten)]
        opts: UpOptions,
    },

    /// SSH into a workspace
    ///
    /// By default, shows the workspace monitor which displays agent status.
    /// Press Ctrl-C to drop to an interactive shell, or pass a command directly.
    ///
    /// Examples:
    ///   devaipod ssh myworkspace           # Show agent monitor (Ctrl-C for shell)
    ///   devaipod ssh myworkspace bash      # Go directly to shell
    ///   devaipod ssh myworkspace -- ls -la # Run a specific command
    Ssh {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Stdio mode: pipe stdin/stdout for ProxyCommand use (VSCode/Zed remote dev)
        #[arg(long)]
        stdio: bool,
        /// Command to run instead of the monitor (e.g., 'bash' for direct shell)
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Generate SSH config entry for a workspace
    ///
    /// Outputs an SSH config block that can be added to ~/.ssh/config.
    /// This enables VSCode/Zed Remote SSH to connect via ProxyCommand.
    ///
    /// Example:
    ///   devaipod ssh-config my-repo >> ~/.ssh/config
    SshConfig {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// User to connect as (default: current user)
        #[arg(long)]
        user: Option<String>,
    },
    /// List workspaces
    List {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Stop a workspace
    Stop {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
    },
    /// Delete a workspace
    Delete {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Force deletion (stop running containers first)
        #[arg(short, long)]
        force: bool,
    },
    /// Rebuild a workspace with a new image
    ///
    /// Recreates the containers with a new or updated image while preserving
    /// the workspace volume (your code and changes). This is useful when:
    /// - The devcontainer.json has changed
    /// - You want to use a newer version of the dev image
    /// - You need to apply configuration changes
    ///
    /// By default, runs postStartCommand after rebuild. Use --run-create to also
    /// run onCreateCommand and postCreateCommand.
    ///
    /// Examples:
    ///   devaipod rebuild my-workspace
    ///   devaipod rebuild my-workspace --image ghcr.io/org/devenv:latest
    Rebuild {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Use a specific container image instead of rebuilding from devcontainer.json
        #[arg(long, value_name = "IMAGE")]
        image: Option<String>,
        /// Also run onCreateCommand and postCreateCommand (default: only postStartCommand)
        #[arg(long)]
        run_create: bool,
    },
    /// View container logs
    Logs {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Which container to show logs for (workspace, agent, gator, proxy)
        #[arg(short, long, default_value = "agent")]
        container: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show from the end
        #[arg(short = 'n', long)]
        tail: Option<u32>,
    },
    /// Show detailed status of a pod
    ///
    /// Displays pod status, container states, agent health, and exposed ports.
    Status {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Debug and diagnose a workspace
    ///
    /// Collects diagnostic information to help troubleshoot issues with
    /// the pod, service-gator, MCP connectivity, and agent health.
    ///
    /// Examples:
    ///   devaipod debug my-workspace
    ///   devaipod debug my-workspace --json
    Debug {
        /// Workspace name (devaipod- prefix optional)
        workspace: String,
        /// Output in JSON format for scripting
        #[arg(long)]
        json: bool,
    },
    /// Run an agent on a repository with a task
    ///
    /// Creates a workspace and starts the agent with a task. Returns immediately
    /// after setup (async by default). Use 'devaipod ssh <workspace>' to monitor
    /// the agent's progress.
    ///
    /// For issue URLs, the source repo is extracted and the default task is
    /// "Fix <issue_url>". If no task is provided and stdin is a TTY, prompts
    /// interactively with the default pre-filled.
    ///
    /// Examples:
    ///   devaipod run https://github.com/org/repo
    ///   devaipod run https://github.com/org/repo 'fix typos in README.md'
    ///   devaipod run https://github.com/org/repo/issues/123  # Default: "Fix <url>"
    ///   devaipod run . 'add unit tests for the parser module'
    Run {
        /// Source: local path, git URL, issue URL, or PR URL
        source: String,
        /// Task description for the AI agent
        #[arg(value_name = "TASK")]
        task: Option<String>,
        /// Task for the agent (alternative to positional argument)
        #[arg(short = 'c', long = "command", value_name = "TASK")]
        command: Option<String>,
        /// Use a specific container image instead of building from devcontainer.json
        #[arg(long, value_name = "IMAGE")]
        image: Option<String>,
        /// Explicit pod name (default: derived from source with unique suffix)
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Configure service-gator scopes for AI agent access to external services
        #[arg(long = "service-gator", value_name = "SCOPE")]
        service_gator_scopes: Vec<String>,
        /// Use a specific image for the service-gator container
        #[arg(long, value_name = "IMAGE")]
        service_gator_image: Option<String>,
    },
    /// Generate shell completions
    ///
    /// Outputs shell completion scripts to stdout for various shells.
    ///
    /// Examples:
    ///   devaipod completions bash > ~/.local/share/bash-completion/completions/devaipod
    ///   devaipod completions zsh > ~/.zfunc/_devaipod
    ///   devaipod completions fish > ~/.config/fish/completions/devaipod.fish
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Initialize devaipod configuration
    ///
    /// Interactive setup wizard for first-time users. Configures:
    /// - Dotfiles/homegit repository
    /// - Forge tokens (GitHub, GitLab, Forgejo) via podman secrets
    /// - OpenCode configuration recommendations
    ///
    /// Examples:
    ///   devaipod init
    ///   devaipod init --config ~/.config/devaipod-test.toml
    Init {
        /// Path to write config file (default: ~/.config/devaipod.toml)
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
}

// =============================================================================
// Container CLI - commands that run inside a devcontainer
// =============================================================================

#[derive(Debug, Parser)]
#[command(name = "devaipod")]
#[command(about = "Sandboxed AI coding agents (container mode)", long_about = None)]
struct ContainerCli {
    /// Path to config file (default: ~/.config/devaipod.toml)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: ContainerCommand,
}

#[derive(Debug, Parser)]
enum ContainerCommand {
    /// Configure the container environment for nested containers
    ///
    /// Sets up containers.conf, subuid/subgid, and starts the podman service.
    /// This command is idempotent and should be run at container startup.
    /// Typically called from postStartCommand in devcontainer.json.
    ConfigureEnv,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Detect context BEFORE parsing args - this determines which CLI we use
    if is_inside_devcontainer() {
        // Container mode - use default log level
        init_tracing(false, false);
        let cli = ContainerCli::parse();
        run_container(cli)
    } else {
        // Host mode - parse CLI first to check for --verbose flag
        let cli = HostCli::parse();
        init_tracing(cli.verbose, cli.quiet);
        run_host(cli).await
    }
}

/// Initialize tracing with the appropriate log level
fn init_tracing(verbose: bool, quiet: bool) {
    let format = tracing_subscriber::fmt::format()
        .without_time()
        .with_target(false)
        .compact();

    let default_level = if verbose {
        "debug"
    } else if quiet {
        "warn"
    } else {
        "info"
    };

    tracing_subscriber::fmt()
        .event_format(format)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .init();
}

/// Commands that don't require a config file to exist
fn command_requires_config(cmd: &HostCommand) -> bool {
    !matches!(
        cmd,
        HostCommand::Init { .. } | HostCommand::Completions { .. }
    )
}

async fn run_host(cli: HostCli) -> Result<()> {
    // Check if config file is required and exists
    if command_requires_config(&cli.command) {
        let config_path = cli
            .config
            .as_ref()
            .cloned()
            .unwrap_or_else(config::config_path);
        if !config_path.exists() {
            eprintln!("No configuration file found at {}", config_path.display());
            eprintln!();
            eprintln!("devaipod requires a configuration file to run.");
            eprintln!("Run 'devaipod init' to create one interactively.");
            eprintln!();
            eprintln!(
                "For more information, see: https://github.com/cgwalters/devaipod#configuration"
            );
            std::process::exit(1);
        }
    }

    let config = config::load_config(cli.config.as_deref())?;

    match cli.command {
        HostCommand::Up { source, opts } => cmd_up(&config, &source, opts).await,

        HostCommand::Ssh {
            workspace,
            stdio,
            command,
        } => cmd_ssh(&normalize_pod_name(&workspace), stdio, &command),
        HostCommand::SshConfig { workspace, user } => {
            cmd_ssh_config(&normalize_pod_name(&workspace), user.as_deref())
        }
        HostCommand::List { json } => cmd_list(json),
        HostCommand::Stop { workspace } => cmd_stop(&normalize_pod_name(&workspace)),
        HostCommand::Delete { workspace, force } => {
            cmd_delete(&normalize_pod_name(&workspace), force)
        }
        HostCommand::Rebuild {
            workspace,
            image,
            run_create,
        } => {
            cmd_rebuild(
                &config,
                &normalize_pod_name(&workspace),
                image.as_deref(),
                run_create,
            )
            .await
        }
        HostCommand::Logs {
            workspace,
            container,
            follow,
            tail,
        } => cmd_logs(&normalize_pod_name(&workspace), &container, follow, tail),
        HostCommand::Status { workspace, json } => {
            cmd_status(&normalize_pod_name(&workspace), json)
        }
        HostCommand::Debug { workspace, json } => {
            cmd_debug(&normalize_pod_name(&workspace), json)
        }
        HostCommand::Run {
            source,
            task,
            command,
            image,
            name,
            service_gator_scopes,
            service_gator_image,
        } => {
            // Check if source is an issue URL - if so, extract repo and set default task
            let (effective_source, default_task) =
                if let Some(issue_ref) = forge::parse_issue_url(&source) {
                    let issue_url = issue_ref.issue_url();
                    let repo_url = issue_ref.repo_url();
                    tracing::info!("Issue URL detected: {}", issue_ref.short_display());
                    (repo_url, Some(format!("Fix {}", issue_url)))
                } else {
                    (source.clone(), None)
                };

            // Merge task sources: positional arg takes precedence, then -c/--command
            // Note: default_task from issue URL is NOT merged here - it's used as
            // the pre-filled text in the interactive prompt instead
            let explicit_task = task.or(command);

            // Determine final task: explicit task, or prompt interactively
            let effective_task = match explicit_task {
                Some(t) => Some(t),
                None if std::io::stdin().is_terminal() => {
                    let prompt = Input::<String>::new()
                        .with_prompt("Task for the AI agent (leave empty to skip)");
                    // If we have a default from issue URL, pre-fill it
                    let prompt = if let Some(ref default) = default_task {
                        prompt.with_initial_text(default)
                    } else {
                        prompt
                    };
                    match prompt.allow_empty(true).interact_text() {
                        Ok(task) if task.trim().is_empty() => None,
                        Ok(task) => Some(task),
                        Err(dialoguer::Error::IO(e))
                            if e.kind() == std::io::ErrorKind::Interrupted =>
                        {
                            // User pressed Ctrl-C, exit gracefully
                            std::process::exit(130)
                        }
                        Err(e) => return Err(e).context("Failed to read task from terminal"),
                    }
                }
                // Non-interactive: use the default task from issue URL if available
                None => default_task,
            };

            cmd_run(
                &config,
                &effective_source,
                effective_task.as_deref(),
                image.as_deref(),
                name.as_deref(),
                &service_gator_scopes,
                service_gator_image.as_deref(),
            )
            .await
        }
        HostCommand::Completions { shell } => cmd_completions(shell),
        HostCommand::Init { config } => init::cmd_init(config.as_deref()),
    }
}

fn run_container(cli: ContainerCli) -> Result<()> {
    let _config = config::load_config(cli.config.as_deref())?;

    match cli.command {
        ContainerCommand::ConfigureEnv => cmd_configure_env(),
    }
}

/// Which lifecycle commands to run
#[derive(Clone, Copy)]
enum LifecycleMode {
    /// Run all commands (onCreateCommand, postCreateCommand, postStartCommand)
    Full,
    /// Skip onCreateCommand (for rebuild when workspace already exists)
    Rebuild,
}

/// Common post-creation steps for all up commands
///
/// After creating a pod with DevaipodPod::create(), this function handles:
/// - Starting the pod
/// - Copying bind_home files
/// - Installing dotfiles
/// - Running lifecycle commands
/// - Printing success message
async fn finalize_pod(
    podman: &podman::PodmanService,
    devaipod_pod: &pod::DevaipodPod,
    devcontainer_config: &devcontainer::DevcontainerConfig,
    config: &config::Config,
) -> Result<()> {
    finalize_pod_with_mode(
        podman,
        devaipod_pod,
        devcontainer_config,
        config,
        LifecycleMode::Full,
    )
    .await
}

/// Common post-creation steps with configurable lifecycle mode
async fn finalize_pod_with_mode(
    podman: &podman::PodmanService,
    devaipod_pod: &pod::DevaipodPod,
    devcontainer_config: &devcontainer::DevcontainerConfig,
    config: &config::Config,
    lifecycle_mode: LifecycleMode,
) -> Result<()> {
    // Start the pod
    devaipod_pod
        .start(podman)
        .await
        .context("Failed to start pod")?;

    // Copy bind_home files into containers
    tracing::debug!("Copying bind_home files...");
    devaipod_pod
        .copy_bind_home_files(
            podman,
            &devaipod_pod.workspace_bind_home,
            &devaipod_pod.agent_bind_home,
            &devaipod_pod.container_home,
            devcontainer_config.effective_user(),
        )
        .await
        .context("Failed to copy bind_home files")?;

    // Install dotfiles before lifecycle commands
    if let Some(ref dotfiles) = config.dotfiles {
        devaipod_pod
            .install_dotfiles(podman, dotfiles, devcontainer_config.effective_user())
            .await
            .context("Failed to install dotfiles")?;
        devaipod_pod
            .install_dotfiles_agent(podman, dotfiles)
            .await
            .context("Failed to install dotfiles in agent")?;
    }

    // Run lifecycle commands based on mode
    match lifecycle_mode {
        LifecycleMode::Full => {
            tracing::debug!("Running lifecycle commands...");
            devaipod_pod
                .run_lifecycle_commands(podman, devcontainer_config)
                .await
                .context("Failed to run lifecycle commands")?;
        }
        LifecycleMode::Rebuild => {
            tracing::debug!("Running rebuild lifecycle commands (postCreate + postStart)...");
            devaipod_pod
                .run_rebuild_lifecycle_commands(podman, devcontainer_config)
                .await
                .context("Failed to run lifecycle commands")?;
        }
    }

    // Success message
    let short_name = strip_pod_prefix(&devaipod_pod.pod_name);
    tracing::info!("Pod ready: {}", devaipod_pod.pod_name);
    tracing::info!("  SSH: devaipod ssh {}", short_name);
    tracing::info!("  Agent: http://localhost:{}", pod::OPENCODE_PORT);

    Ok(())
}

// =============================================================================
// Workspace Creation (shared by up and run commands)
// =============================================================================

/// Result of creating a workspace
struct CreateResult {
    /// The pod name that was created
    pod_name: String,
}

/// Create a workspace from a source (local path, remote URL, or PR)
///
/// This is the inner "create" operation that handles all the common pod setup
/// logic without any SSH or other post-setup behavior. Both `cmd_up` and `cmd_run`
/// use this function internally.
///
/// Like `podman create` vs `podman run`, this function just creates and starts
/// the pod but doesn't perform any interactive operations afterward.
async fn create_workspace(
    config: &config::Config,
    source: &str,
    opts: &CreateOptions,
) -> Result<CreateResult> {
    // Dispatch based on source type
    if let Some(pr_ref) = forge::parse_pr_url(source) {
        create_workspace_from_pr(config, pr_ref, opts).await
    } else if source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
    {
        create_workspace_from_remote(config, source, opts).await
    } else {
        create_workspace_from_local(config, source, opts).await
    }
}

/// Create a workspace from a local git repository
async fn create_workspace_from_local(
    config: &config::Config,
    source: &str,
    opts: &CreateOptions,
) -> Result<CreateResult> {
    let source_path = std::path::Path::new(source).canonicalize().ok();

    // Local path is required for non-remote sources
    let project_path = match source_path {
        Some(ref p) => p,
        None => {
            bail!("Path '{}' does not exist or is not accessible.", source);
        }
    };

    // Detect git repository info for cloning into containers
    let git_info =
        git::detect_git_info(project_path).context("Failed to detect git repository info")?;

    // Require a remote URL for cloning
    if git_info.remote_url.is_none() {
        bail!(
            "No git remote configured for {}.\n\
             devaipod clones the repository into containers and requires a git remote.\n\
             Configure with: git remote add origin <url>",
            project_path.display()
        );
    }

    // Warn about dirty working tree
    if git_info.is_dirty {
        eprintln!(
            "\n\u{26a0}\u{fe0f}  Warning: Uncommitted changes detected ({} file(s)):",
            git_info.dirty_files.len()
        );
        for file in git_info.dirty_files.iter().take(5) {
            eprintln!("     {}", file);
        }
        if git_info.dirty_files.len() > 5 {
            eprintln!("     ... and {} more", git_info.dirty_files.len() - 5);
        }
        eprintln!();
        eprintln!(
            "   The AI agent will work on commit {} and won't see uncommitted changes.",
            &git_info.commit_sha[..8]
        );
        eprintln!("   Consider committing or stashing your changes first.\n");
    }

    // Find and load devcontainer.json (optional when --image or default-image is provided)
    let devcontainer_json_path = devcontainer::try_find_devcontainer_json(project_path);
    let (devcontainer_config, effective_image) = if let Some(ref path) = devcontainer_json_path {
        (devcontainer::load(path)?, opts.image.clone())
    } else if opts.image.is_some() {
        tracing::info!("No devcontainer.json found, using defaults with --image override");
        (
            devcontainer::DevcontainerConfig::default(),
            opts.image.clone(),
        )
    } else if config.default_image.is_some() {
        tracing::info!(
            "No devcontainer.json found, using default-image from config: {}",
            config.default_image.as_ref().unwrap()
        );
        (
            devcontainer::DevcontainerConfig::default(),
            config.default_image.clone(),
        )
    } else {
        bail!(
            "No devcontainer.json found in {}.\n\
             Either add a devcontainer.json, use --image, or set default-image in config.",
            project_path.display()
        );
    };

    // Derive project/pod name from path
    let project_name = project_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    // Use explicit name if provided, otherwise generate a unique name
    let pod_name = if let Some(ref name) = opts.name {
        normalize_pod_name(name)
    } else {
        make_pod_name(&project_name)
    };

    // Check for API keys and warn if none are configured (helps first-run experience)
    check_api_keys_configured();

    // Parse CLI service-gator scopes and merge with file config
    let service_gator_config = if !opts.service_gator_scopes.is_empty() {
        let cli_scopes = service_gator::parse_scopes(&opts.service_gator_scopes)
            .context("Failed to parse --service-gator scopes")?;
        service_gator::merge_configs(&config.service_gator, &cli_scopes)
    } else {
        config.service_gator.clone()
    };

    // Check if gator should be enabled (from merged config)
    let enable_gator = service_gator_config.is_enabled();

    // Start podman service
    tracing::debug!("Starting podman service...");
    let podman = podman::PodmanService::spawn()
        .await
        .context("Failed to start podman service")?;

    // Check if network isolation should be enabled
    let enable_network_isolation = config.network_isolation.enabled;

    // Create the pod with all containers
    tracing::debug!("Creating pod '{}'...", pod_name);
    let source = pod::WorkspaceSource::LocalRepo(git_info);

    // Build extra labels for task description and mode
    let mut extra_labels = Vec::new();
    extra_labels.push((
        "io.devaipod.mode".to_string(),
        opts.mode.as_str().to_string(),
    ));
    if let Some(ref task_desc) = opts.task {
        extra_labels.push(("io.devaipod.task".to_string(), task_desc.clone()));
    }

    let devaipod_pod = pod::DevaipodPod::create(
        &podman,
        project_path,
        &devcontainer_config,
        &pod_name,
        enable_gator,
        enable_network_isolation,
        config,
        &source,
        &extra_labels,
        Some(&service_gator_config),
        effective_image.as_deref(),
        opts.service_gator_image.as_deref(),
        opts.task.as_deref(),
    )
    .await
    .context("Failed to create devaipod pod")?;

    finalize_pod(&podman, &devaipod_pod, &devcontainer_config, config).await?;

    drop(podman);

    Ok(CreateResult { pod_name })
}

/// Create a workspace from a remote git URL
async fn create_workspace_from_remote(
    config: &config::Config,
    remote_url: &str,
    opts: &CreateOptions,
) -> Result<CreateResult> {
    tracing::info!("Setting up {}...", remote_url);

    // Extract repo name from URL for naming
    let repo_name = git::extract_repo_name(remote_url).unwrap_or_else(|| "project".to_string());

    // Clone the repository to a temp directory to read devcontainer.json and get default branch
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_path = temp_dir.path();

    tracing::debug!("Cloning repository to read devcontainer.json...");

    // Use authenticated URL if GH_TOKEN is available (for private repos)
    let gh_token = git::get_github_token_with_secret(config);
    let clone_url = git::authenticated_clone_url(remote_url, gh_token.as_deref());

    // Clone the repository (shallow clone for speed)
    let clone_output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            &clone_url,
            temp_path.to_str().unwrap(),
        ])
        .output()
        .await
        .context("Failed to clone repository")?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        bail!("Failed to clone repository: {}", stderr);
    }

    // Get the default branch name
    let branch_output = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(temp_path)
        .output()
        .await
        .context("Failed to get default branch")?;

    let default_branch = if branch_output.status.success() {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    } else {
        "main".to_string() // Fallback
    };

    // Find and load devcontainer.json from the cloned repo (optional when --image or default-image is provided)
    let devcontainer_json_path = devcontainer::try_find_devcontainer_json(temp_path);
    let (devcontainer_config, effective_image) = if let Some(ref path) = devcontainer_json_path {
        (devcontainer::load(path)?, opts.image.clone())
    } else if opts.image.is_some() {
        tracing::info!(
            "No devcontainer.json found in repository, using defaults with --image override"
        );
        (
            devcontainer::DevcontainerConfig::default(),
            opts.image.clone(),
        )
    } else if config.default_image.is_some() {
        tracing::info!(
            "No devcontainer.json found in repository, using default-image from config: {}",
            config.default_image.as_ref().unwrap()
        );
        (
            devcontainer::DevcontainerConfig::default(),
            config.default_image.clone(),
        )
    } else {
        bail!(
            "No devcontainer.json found in {}.\n\
             Either add a devcontainer.json to the repository, use --image, or set default-image in config.",
            remote_url
        );
    };

    // Use explicit name if provided, otherwise generate a unique name
    let pod_name = if let Some(ref name) = opts.name {
        normalize_pod_name(name)
    } else {
        make_pod_name(&repo_name)
    };

    // For remote URLs, auto-enable service-gator with readonly + draft PR access
    // to the target repository (unless user provided explicit scopes)
    let service_gator_config = if !opts.service_gator_scopes.is_empty() {
        let cli_scopes = service_gator::parse_scopes(&opts.service_gator_scopes)
            .context("Failed to parse --service-gator scopes")?;
        service_gator::merge_configs(&config.service_gator, &cli_scopes)
    } else if let Some(repo_ref) = forge::parse_repo_url(remote_url) {
        // Auto-configure: read + create-draft for the target repo
        let mut sg_config = config.service_gator.clone();
        let owner_repo = repo_ref.owner_repo();

        match repo_ref.forge_type {
            forge::ForgeType::GitHub => {
                sg_config.gh.repos.insert(
                    owner_repo.clone(),
                    config::GhRepoPermission {
                        read: true,
                        create_draft: true,
                        pending_review: false,
                        write: false,
                    },
                );
                tracing::debug!(
                    "Auto-enabled service-gator for {} (read + draft PRs)",
                    owner_repo
                );
            }
            forge::ForgeType::GitLab | forge::ForgeType::Forgejo | forge::ForgeType::Gitea => {
                // TODO: Add GitLab/Forgejo/Gitea support to service-gator config
                tracing::debug!(
                    "Auto service-gator not yet supported for {} ({})",
                    repo_ref.forge_type,
                    owner_repo
                );
            }
        }
        sg_config
    } else {
        config.service_gator.clone()
    };

    // Start podman service
    let podman = podman::PodmanService::spawn()
        .await
        .context("Failed to start podman service")?;

    let enable_gator = service_gator_config.is_enabled();
    let enable_network_isolation = config.network_isolation.enabled;

    // Create source from remote repo info
    let remote_info = git::RemoteRepoInfo {
        remote_url: remote_url.to_string(),
        default_branch: default_branch.clone(),
        repo_name: repo_name.clone(),
    };
    let source = pod::WorkspaceSource::RemoteRepo(remote_info);

    // Build extra labels for task description and mode
    let mut extra_labels = Vec::new();
    extra_labels.push((
        "io.devaipod.mode".to_string(),
        opts.mode.as_str().to_string(),
    ));
    if let Some(ref task_desc) = opts.task {
        extra_labels.push(("io.devaipod.task".to_string(), task_desc.clone()));
    }

    // Create the pod
    tracing::debug!("Creating pod '{}'...", pod_name);
    let devaipod_pod = pod::DevaipodPod::create(
        &podman,
        temp_path,
        &devcontainer_config,
        &pod_name,
        enable_gator,
        enable_network_isolation,
        config,
        &source,
        &extra_labels,
        Some(&service_gator_config),
        effective_image.as_deref(),
        opts.service_gator_image.as_deref(),
        opts.task.as_deref(),
    )
    .await
    .context("Failed to create devaipod pod")?;

    finalize_pod(&podman, &devaipod_pod, &devcontainer_config, config).await?;

    drop(podman);

    Ok(CreateResult { pod_name })
}

/// Create a workspace from a PR/MR URL
async fn create_workspace_from_pr(
    config: &config::Config,
    pr_ref: forge::PullRequestRef,
    opts: &CreateOptions,
) -> Result<CreateResult> {
    tracing::info!(
        "Setting up PR #{} ({}/{})...",
        pr_ref.number,
        pr_ref.owner,
        pr_ref.repo
    );

    // Fetch PR metadata (pass config for GH_TOKEN from podman secrets)
    let pr_info = forge::fetch_pr_info(&pr_ref, Some(config))
        .await
        .context("Failed to fetch PR information")?;

    tracing::debug!("PR: {}", pr_info.title);
    tracing::debug!("Head: {} @ {}", pr_info.head_ref, &pr_info.head_sha[..8]);

    // Clone PR head to get the devcontainer.json from the PR
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_path = temp_dir.path();

    tracing::debug!("Cloning PR head to read devcontainer.json...");

    // Use authenticated URL if GH_TOKEN is available (for private repos)
    let gh_token = git::get_github_token_with_secret(config);
    let clone_url = git::authenticated_clone_url(&pr_info.head_clone_url, gh_token.as_deref());

    // Clone from the PR's head repository and checkout the specific commit
    let clone_output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            &pr_info.head_ref,
            &clone_url,
            temp_path.to_str().unwrap(),
        ])
        .output()
        .await
        .context("Failed to clone PR head repository")?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        bail!("Failed to clone PR head repository: {}", stderr);
    }

    // Find and load devcontainer.json from the cloned repo (optional when --image or default-image is provided)
    let devcontainer_json_path = devcontainer::try_find_devcontainer_json(temp_path);
    let (devcontainer_config, effective_image) = if let Some(ref path) = devcontainer_json_path {
        (devcontainer::load(path)?, opts.image.clone())
    } else if opts.image.is_some() {
        tracing::info!("No devcontainer.json found in PR, using defaults with --image override");
        (
            devcontainer::DevcontainerConfig::default(),
            opts.image.clone(),
        )
    } else if config.default_image.is_some() {
        tracing::info!(
            "No devcontainer.json found in PR, using default-image from config: {}",
            config.default_image.as_ref().unwrap()
        );
        (
            devcontainer::DevcontainerConfig::default(),
            config.default_image.clone(),
        )
    } else {
        bail!(
            "No devcontainer.json found in PR.\n\
             Either add a devcontainer.json to the PR, use --image, or set default-image in config."
        );
    };

    // Use explicit name if provided, otherwise generate a unique name
    let pod_name = if let Some(ref name) = opts.name {
        normalize_pod_name(name)
    } else {
        make_pr_pod_name(&pr_ref.repo, pr_ref.number)
    };

    // Start podman service
    let podman = podman::PodmanService::spawn()
        .await
        .context("Failed to start podman service")?;

    // Check for gator and network isolation settings
    let enable_gator = config.service_gator.is_enabled();
    let enable_network_isolation = config.network_isolation.enabled;

    // Create source from PR info
    let source = pod::WorkspaceSource::PullRequest(pr_info);

    // Build extra labels for task description and mode
    let mut extra_labels = Vec::new();
    extra_labels.push((
        "io.devaipod.mode".to_string(),
        opts.mode.as_str().to_string(),
    ));
    if let Some(ref task_desc) = opts.task {
        extra_labels.push(("io.devaipod.task".to_string(), task_desc.clone()));
    }

    // Create the pod
    // Note: For PR workflows, we use the file-based service_gator config (no CLI override yet)
    tracing::debug!("Creating pod '{}'...", pod_name);
    let devaipod_pod = pod::DevaipodPod::create(
        &podman,
        temp_path, // Use temp path for image building context
        &devcontainer_config,
        &pod_name,
        enable_gator,
        enable_network_isolation,
        config,
        &source,
        &extra_labels,
        None, // Use config.service_gator for PR workflows
        effective_image.as_deref(),
        opts.service_gator_image.as_deref(),
        opts.task.as_deref(),
    )
    .await
    .context("Failed to create devaipod pod")?;

    finalize_pod(&podman, &devaipod_pod, &devcontainer_config, config).await?;

    drop(podman);

    Ok(CreateResult { pod_name })
}

// =============================================================================
// Command Implementations
// =============================================================================

/// Create/start a workspace with AI agent
///
/// This is a thin wrapper around `create_workspace` that handles:
/// - Dry-run mode (prints what would be created)
/// - Optional SSH into the workspace after creation
///
/// Uses podman-native multi-container setup with a pod containing:
/// - workspace: The user's development environment
/// - agent: Container running opencode serve with restricted security
/// - gator (optional): Service-gator MCP server container
async fn cmd_up(config: &config::Config, source: &str, opts: UpOptions) -> Result<()> {
    // Handle dry-run mode
    if opts.dry_run {
        return cmd_dry_run(config, source, &opts).await;
    }

    // Create the workspace using the common create function
    let create_opts = CreateOptions::from_up_options(&opts);
    let result = create_workspace(config, source, &create_opts).await?;

    // Optionally SSH into the workspace - go directly to bash, not the monitor
    // (the monitor is for observing a running agent, but `up -S` is for interactive work)
    if opts.ssh {
        return cmd_ssh(&result.pod_name, false, &["bash".to_string()]);
    }

    Ok(())
}

/// Run an agent on a repository with a task
///
/// This is a thin wrapper around `create_workspace` that:
/// - Sets mode to Run (for tracking)
/// - Does not SSH by default (async execution)
///
/// It creates a workspace and starts the agent with the task, then returns
/// immediately. Use `devaipod ssh <workspace>` to monitor the agent's progress.
async fn cmd_run(
    config: &config::Config,
    source: &str,
    command: Option<&str>,
    image: Option<&str>,
    explicit_name: Option<&str>,
    service_gator_scopes: &[String],
    service_gator_image: Option<&str>,
) -> Result<()> {
    // Build CreateOptions with mode=Run
    let create_opts = CreateOptions {
        task: command.map(|s| s.to_string()),
        image: image.map(|s| s.to_string()),
        name: explicit_name.map(|s| s.to_string()),
        service_gator_scopes: service_gator_scopes.to_vec(),
        service_gator_image: service_gator_image.map(|s| s.to_string()),
        mode: WorkspaceMode::Run,
    };

    // Create the workspace - no SSH by default (async execution)
    let _result = create_workspace(config, source, &create_opts).await?;

    Ok(())
}

/// Handle dry-run mode for the up command
///
/// Prints what would be created without actually creating anything.
async fn cmd_dry_run(config: &config::Config, source: &str, opts: &UpOptions) -> Result<()> {
    // Dispatch based on source type for dry-run info
    if let Some(pr_ref) = forge::parse_pr_url(source) {
        // PR dry-run
        let pr_info = forge::fetch_pr_info(&pr_ref, Some(config))
            .await
            .context("Failed to fetch PR information")?;

        let pod_name = if let Some(ref name) = opts.name {
            normalize_pod_name(name)
        } else {
            make_pr_pod_name(&pr_ref.repo, pr_ref.number)
        };

        tracing::info!("Dry run mode - would create pod '{}'", pod_name);
        tracing::info!("  PR: {}", pr_info.pr_ref.short_display());
        tracing::info!("  Head: {} @ {}", pr_info.head_ref, &pr_info.head_sha[..8]);
        tracing::info!("  Clone URL: {}", pr_info.head_clone_url);
        if opts.image.is_some() {
            tracing::info!("  devcontainer: (none, using image override)");
        }
    } else if source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
    {
        // Remote URL dry-run
        let repo_name = git::extract_repo_name(source).unwrap_or_else(|| "project".to_string());
        let pod_name = if let Some(ref name) = opts.name {
            normalize_pod_name(name)
        } else {
            make_pod_name(&repo_name)
        };

        // Parse service-gator config for dry-run info
        let service_gator_config = if !opts.service_gator_scopes.is_empty() {
            let cli_scopes = service_gator::parse_scopes(&opts.service_gator_scopes)
                .context("Failed to parse --service-gator scopes")?;
            service_gator::merge_configs(&config.service_gator, &cli_scopes)
        } else {
            config.service_gator.clone()
        };

        tracing::info!("Dry run mode - would create pod '{}'", pod_name);
        tracing::info!("  Remote URL: {}", source);
        if opts.image.is_none() {
            tracing::info!("  (would clone to read devcontainer.json)");
        } else {
            tracing::info!("  devcontainer: (none, using image override)");
        }
        tracing::info!("  gator enabled: {}", service_gator_config.is_enabled());
        if let Some(ref img) = opts.service_gator_image {
            tracing::info!("  gator image: {}", img);
        }
        if let Some(ref task) = opts.task {
            tracing::info!("  Task: {}", task);
        }
    } else {
        // Local path dry-run
        let source_path = std::path::Path::new(source).canonicalize().ok();
        let project_path = match source_path {
            Some(ref p) => p,
            None => {
                bail!("Path '{}' does not exist or is not accessible.", source);
            }
        };

        let project_name = project_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());

        let pod_name = if let Some(ref name) = opts.name {
            normalize_pod_name(name)
        } else {
            make_pod_name(&project_name)
        };

        // Parse service-gator config for dry-run info
        let service_gator_config = if !opts.service_gator_scopes.is_empty() {
            let cli_scopes = service_gator::parse_scopes(&opts.service_gator_scopes)
                .context("Failed to parse --service-gator scopes")?;
            service_gator::merge_configs(&config.service_gator, &cli_scopes)
        } else {
            config.service_gator.clone()
        };

        let devcontainer_json_path = devcontainer::try_find_devcontainer_json(project_path);

        tracing::info!("Dry run: would create pod '{}'", pod_name);
        tracing::info!("  project: {}", project_path.display());
        if let Some(ref path) = devcontainer_json_path {
            tracing::info!("  devcontainer: {}", path.display());
        } else {
            tracing::info!("  devcontainer: (none, using image override)");
        }
        tracing::info!("  gator enabled: {}", service_gator_config.is_enabled());
        if let Some(ref img) = opts.service_gator_image {
            tracing::info!("  gator image: {}", img);
        }
    }

    Ok(())
}

/// Check if any API keys are configured for the AI agent and warn if not
///
/// This helps users on first run understand that they need to configure
/// API keys for the agent to function properly. Only warns if no config
/// file exists - if the user has a config file, we assume they've set
/// things up properly (e.g. via secrets, env vars in config, etc).
fn check_api_keys_configured() {
    // If a config file exists, assume the user has configured things properly
    if config::config_path().exists() {
        return;
    }

    // Check for DEVAIPOD_AGENT_* env vars (legacy mechanism)
    let agent_env_vars = config::collect_agent_env_vars();

    if agent_env_vars.is_empty() {
        eprintln!();
        eprintln!("Warning: No devaipod configuration found.");
        eprintln!("   Run 'devaipod init' to create a config file.");
        eprintln!("   See: https://opencode.ai/docs/providers/");
        eprintln!();
    }
}

/// Check if we're running inside a toolbox container
fn is_toolbox() -> bool {
    std::env::var_os("TOOLBOX_PATH").is_some()
}

/// Build a std::process::Command for running podman CLI.
///
/// In toolbox mode, uses flatpak-spawn to run podman on the host.
/// Otherwise, runs podman directly.
fn podman_command() -> ProcessCommand {
    if is_toolbox() {
        let mut cmd = ProcessCommand::new("flatpak-spawn");
        cmd.args(["--host", "podman"]);
        cmd
    } else {
        ProcessCommand::new("podman")
    }
}

/// SSH into workspace using podman exec
fn cmd_ssh(pod_name: &str, stdio: bool, command: &[String]) -> Result<()> {
    let container = format!("{}-workspace", pod_name);

    if stdio {
        // Stdio mode: pipe stdin/stdout directly for ProxyCommand use
        // VSCode/Zed Remote SSH uses this to tunnel SSH protocol
        let mut cmd = podman_command();
        cmd.args(["exec", "-i", &container]);

        if command.is_empty() {
            // Default to workspace monitor (Ctrl-C drops to shell), with fallback if unavailable
            cmd.args([
                "/bin/sh",
                "-c",
                "if command -v python3 >/dev/null && [ -f /opt/devaipod/scripts/workspace_monitor.py ]; then exec python3 /opt/devaipod/scripts/workspace_monitor.py; else echo 'Monitor not available, dropping to shell'; exec bash; fi",
            ]);
        } else {
            cmd.args(command);
        }

        let status = cmd.status().context("Failed to run podman exec")?;

        if !status.success() {
            bail!(
                "podman exec failed for container '{}' (exit code {:?}). \
                 The container may not exist or is not running. \
                 Run 'devaipod list' to see available pods.",
                container,
                status.code()
            );
        }
    } else {
        // Interactive mode with TTY
        tracing::info!("Connecting to container '{}'...", container);

        let mut cmd = podman_command();
        cmd.args(["exec", "-it", &container]);

        if command.is_empty() {
            // Default to workspace monitor (Ctrl-C drops to shell), with fallback if unavailable
            cmd.args([
                "/bin/sh",
                "-c",
                "if command -v python3 >/dev/null && [ -f /opt/devaipod/scripts/workspace_monitor.py ]; then exec python3 /opt/devaipod/scripts/workspace_monitor.py; else echo 'Monitor not available, dropping to shell'; exec bash; fi",
            ]);
        } else {
            cmd.args(command);
        }

        let status = cmd.status().context("Failed to run podman exec")?;

        if !status.success() {
            bail!(
                "podman exec failed for container '{}' (exit code {:?}). \
                 The container may not exist or is not running. \
                 Run 'devaipod list' to see available pods.",
                container,
                status.code()
            );
        }
    }

    Ok(())
}

/// Get the SSH config directory path (~/.ssh/config.d)
fn get_ssh_config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".ssh").join("config.d"))
}

/// Get the SSH config file path for a workspace
///
/// The config file is named after the short workspace name (without prefix)
fn get_ssh_config_path(pod_name: &str) -> Result<PathBuf> {
    let short_name = strip_pod_prefix(pod_name);
    Ok(get_ssh_config_dir()?.join(format!("{}{}", POD_NAME_PREFIX, short_name)))
}

/// Check if ~/.ssh/config has Include directive for config.d
fn ssh_config_has_include() -> bool {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return false,
    };
    let ssh_config = PathBuf::from(home).join(".ssh").join("config");

    if !ssh_config.exists() {
        return false;
    }

    let content = match std::fs::read_to_string(&ssh_config) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Check for Include directive that covers config.d/*
    // Common patterns: "Include config.d/*", "Include ~/.ssh/config.d/*"
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("Include") {
            let rest = line.strip_prefix("Include").unwrap_or("").trim();
            if rest.contains("config.d/*") || rest.contains("config.d/") {
                return true;
            }
        }
    }

    false
}

/// Remove SSH config file for a workspace
fn remove_ssh_config(workspace: &str) -> Result<()> {
    let config_path = get_ssh_config_path(workspace)?;
    if config_path.exists() {
        std::fs::remove_file(&config_path)
            .with_context(|| format!("Failed to remove {}", config_path.display()))?;
        tracing::info!("Removed SSH config: {}", config_path.display());
    }
    Ok(())
}

/// Generate SSH config entry for a workspace
fn cmd_ssh_config(pod_name: &str, user: Option<&str>) -> Result<()> {
    // Determine username: --user flag, or current user
    let username = user
        .map(|s| s.to_string())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "user".to_string());

    // Find the devaipod binary path for the ProxyCommand
    let devaipod_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "devaipod".to_string());

    // Create SSH config content
    let config_content = format!(
        r#"# Generated by devaipod ssh-config
Host {pod}.devaipod
    ProxyCommand {devaipod} ssh --stdio {pod}
    User {user}
    StrictHostKeyChecking no
    UserKnownHostsFile /dev/null
    LogLevel ERROR
"#,
        pod = pod_name,
        devaipod = devaipod_path,
        user = username,
    );

    // Ensure ~/.ssh/config.d directory exists
    let config_dir = get_ssh_config_dir()?;
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create {}", config_dir.display()))?;

    // Write the config file
    let config_path = get_ssh_config_path(pod_name)?;
    std::fs::write(&config_path, &config_content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    println!("Added SSH config to {}", config_path.display());

    // Check if Include directive exists in ~/.ssh/config
    if !ssh_config_has_include() {
        println!();
        println!("Add this line to the TOP of ~/.ssh/config:");
        println!("Include ~/.ssh/config.d/*");
    }

    Ok(())
}

/// List devaipod pods using podman pod ps
fn cmd_list(json_output: bool) -> Result<()> {
    let filter = format!("name={}*", POD_NAME_PREFIX);
    let output = podman_command()
        .args(["pod", "ps", "--filter", &filter, "--format=json"])
        .output()
        .context("Failed to run podman pod ps")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!(
                "podman pod ps failed with exit code {:?}",
                output.status.code()
            );
        } else {
            bail!("podman pod ps failed: {}", stderr);
        }
    }

    let pods: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|_| Vec::new());

    if json_output {
        // For JSON output, enrich with labels from pod inspect
        let mut enriched_pods = Vec::new();
        for pod in &pods {
            let mut enriched = pod.clone();
            if let Some(name) = pod.get("Name").and_then(|v| v.as_str()) {
                if let Some(labels) = get_pod_labels(name) {
                    enriched["Labels"] = labels;
                }
            }
            enriched_pods.push(enriched);
        }
        println!("{}", serde_json::to_string_pretty(&enriched_pods)?);
        return Ok(());
    }

    if pods.is_empty() {
        println!("No devaipod workspaces found.");
        println!("Use 'devaipod up <path>' to create one.");
        return Ok(());
    }

    // Collect pod info with labels
    struct PodInfo {
        name: String,
        status: String,
        containers: usize,
        created: String,
        repo: Option<String>,
        pr: Option<String>,
        task: Option<String>,
        mode: Option<String>,
        agent_status: Option<bool>,
    }

    let mut pod_infos: Vec<PodInfo> = Vec::new();
    for pod in &pods {
        let full_name = pod.get("Name").and_then(|v| v.as_str()).unwrap_or("-");
        let name = strip_pod_prefix(full_name).to_string();
        let status = pod
            .get("Status")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
            .to_string();
        let containers = pod
            .get("Containers")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let created = pod
            .get("Created")
            .and_then(|v| v.as_str())
            .unwrap_or("-")
            .to_string();

        // Get labels from pod inspect (use full name for podman commands)
        let (repo, pr, task, mode) = if let Some(labels) = get_pod_labels(full_name) {
            let repo = labels
                .get("io.devaipod.repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let pr = labels
                .get("io.devaipod.pr")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let task = labels
                .get("io.devaipod.task")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mode = labels
                .get("io.devaipod.mode")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (repo, pr, task, mode)
        } else {
            (None, None, None, None)
        };

        // Check agent status for running pods
        let agent_status = if status.to_lowercase() == "running" {
            check_agent_health(full_name)
        } else {
            None
        };

        pod_infos.push(PodInfo {
            name,
            status,
            containers,
            created,
            repo,
            pr,
            task,
            mode,
            agent_status,
        });
    }

    // Calculate column widths
    let name_width = pod_infos
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let repo_width = pod_infos
        .iter()
        .filter_map(|p| p.repo.as_ref())
        .map(|s| s.len())
        .max()
        .unwrap_or(0)
        .max(4);

    // Check if any pods have repo/PR/task info
    let has_repo_info = pod_infos.iter().any(|p| p.repo.is_some());
    let has_task_info = pod_infos.iter().any(|p| p.task.is_some());

    // Print header - include MODE column when there are task-based workspaces
    if has_repo_info {
        if has_task_info {
            println!(
                "{:<name_width$}  {:<18}  {:<4}  {:<repo_width$}  {:<6}  {:<25}  CREATED",
                "NAME",
                "STATUS",
                "MODE",
                "REPO",
                "PR",
                "TASK",
                name_width = name_width,
                repo_width = repo_width
            );
        } else {
            println!(
                "{:<name_width$}  {:<18}  {:<repo_width$}  {:<6}  CREATED",
                "NAME",
                "STATUS",
                "REPO",
                "PR",
                name_width = name_width,
                repo_width = repo_width
            );
        }
    } else if has_task_info {
        println!(
            "{:<name_width$}  {:<18}  {:<4}  {:<25}  CREATED",
            "NAME",
            "STATUS",
            "MODE",
            "TASK",
            name_width = name_width
        );
    } else {
        println!(
            "{:<name_width$}  {:<18}  {:<12}  CREATED",
            "NAME",
            "STATUS",
            "CONTAINERS",
            name_width = name_width
        );
    }

    // Print pods
    for info in &pod_infos {
        let created_display = format_created_time(&info.created);

        // Build status display with agent status suffix for running pods
        let base_status = match info.status.to_lowercase().as_str() {
            "running" => "Running",
            "stopped" => "Stopped",
            "exited" => "Exited",
            "degraded" => "Degraded",
            _ => &info.status,
        };

        // For running pods, append agent status
        let status_display = if info.status.to_lowercase() == "running" {
            match info.agent_status {
                Some(true) => format!("{} [agent:ok]", base_status),
                Some(false) => format!("{} [agent:--]", base_status),
                None => base_status.to_string(),
            }
        } else {
            base_status.to_string()
        };

        // Show mode (run/up) if available
        let mode_display = info.mode.as_deref().unwrap_or("-");

        // Truncate task to 25 chars for display (shortened to make room for mode)
        let task_display = info
            .task
            .as_ref()
            .map(|t| {
                if t.len() > 25 {
                    format!("{}...", &t[..22])
                } else {
                    t.clone()
                }
            })
            .unwrap_or_else(|| "-".to_string());

        if has_repo_info {
            let repo_display = info.repo.as_deref().unwrap_or("-");
            let pr_display = info
                .pr
                .as_ref()
                .map(|n| format!("#{}", n))
                .unwrap_or_else(|| "-".to_string());

            if has_task_info {
                println!(
                    "{:<name_width$}  {:<18}  {:<4}  {:<repo_width$}  {:<6}  {:<25}  {}",
                    info.name,
                    status_display,
                    mode_display,
                    repo_display,
                    pr_display,
                    task_display,
                    created_display,
                    name_width = name_width,
                    repo_width = repo_width
                );
            } else {
                println!(
                    "{:<name_width$}  {:<18}  {:<repo_width$}  {:<6}  {}",
                    info.name,
                    status_display,
                    repo_display,
                    pr_display,
                    created_display,
                    name_width = name_width,
                    repo_width = repo_width
                );
            }
        } else if has_task_info {
            println!(
                "{:<name_width$}  {:<18}  {:<4}  {:<25}  {}",
                info.name,
                status_display,
                mode_display,
                task_display,
                created_display,
                name_width = name_width
            );
        } else {
            println!(
                "{:<name_width$}  {:<18}  {:<12}  {}",
                info.name,
                status_display,
                format!(
                    "{} container{}",
                    info.containers,
                    if info.containers == 1 { "" } else { "s" }
                ),
                created_display,
                name_width = name_width
            );
        }
    }

    Ok(())
}

/// Get labels for a pod using podman pod inspect
fn get_pod_labels(pod_name: &str) -> Option<serde_json::Value> {
    let output = podman_command()
        .args(["pod", "inspect", "--format", "{{json .Labels}}", pod_name])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(json_str.trim()).ok()
}

/// Format a timestamp to a more readable format
fn format_created_time(timestamp: &str) -> String {
    // Podman returns timestamps like "2025-01-26T10:30:00.000000000Z"
    // Try to parse and show a relative or short format
    if timestamp.len() >= 10 {
        // Just show the date portion for simplicity
        timestamp[..10].to_string()
    } else {
        timestamp.to_string()
    }
}

/// Stop a pod using podman pod stop
fn cmd_stop(pod_name: &str) -> Result<()> {
    tracing::info!("Stopping pod '{}'...", pod_name);

    let output = podman_command()
        .args(["pod", "stop", pod_name])
        .output()
        .context("Failed to run podman pod stop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // Ignore "not running" errors
        if !stderr.contains("not running") && !stderr.contains("no such pod") {
            if stderr.is_empty() {
                bail!(
                    "podman pod stop failed with exit code {:?}",
                    output.status.code()
                );
            } else {
                bail!("podman pod stop failed: {}", stderr);
            }
        }
    }

    tracing::info!("Pod '{}' stopped", pod_name);
    Ok(())
}

/// Delete a pod using podman pod rm
fn cmd_delete(pod_name: &str, force: bool) -> Result<()> {
    tracing::info!("Deleting pod '{}'...", pod_name);

    // Stop the pod first (graceful shutdown)
    // This gives containers time to handle SIGTERM before we remove them
    let stop_output = podman_command()
        .args(["pod", "stop", pod_name])
        .output()
        .context("Failed to run podman pod stop")?;

    if !stop_output.status.success() {
        // Pod might already be stopped, or might not exist - continue with rm
        tracing::debug!(
            "Pod stop returned non-zero (may already be stopped): {}",
            String::from_utf8_lossy(&stop_output.stderr).trim()
        );
    }

    let mut cmd = podman_command();
    cmd.args(["pod", "rm"]);

    if force {
        cmd.arg("--force");
    }

    cmd.arg(pod_name);

    let output = cmd.output().context("Failed to run podman pod rm")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            bail!(
                "podman pod rm failed with exit code {:?}",
                output.status.code()
            );
        } else {
            bail!("podman pod rm failed: {}", stderr);
        }
    }

    tracing::info!("Pod '{}' deleted", pod_name);

    // Clean up SSH config file if it exists
    if let Err(e) = remove_ssh_config(pod_name) {
        tracing::warn!("Failed to remove SSH config: {}", e);
    }

    Ok(())
}

/// Rebuild a workspace with a new image while preserving the workspace volume
///
/// This stops and removes the containers but keeps the volumes intact,
/// then recreates the containers with the new/updated image.
async fn cmd_rebuild(
    config: &config::Config,
    pod_name: &str,
    image_override: Option<&str>,
    run_create: bool,
) -> Result<()> {
    tracing::info!("Rebuilding workspace '{}'...", strip_pod_prefix(pod_name));

    // Get pod labels to find the repo URL
    let labels = get_pod_labels(pod_name)
        .ok_or_else(|| color_eyre::eyre::eyre!("Pod '{}' not found", pod_name))?;

    let repo_url = labels
        .get("io.devaipod.repo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "Pod '{}' has no repository label. Cannot determine source for rebuild.",
                pod_name
            )
        })?;

    // Convert repo label back to URL (github.com/owner/repo -> https://github.com/owner/repo)
    let remote_url = format!("https://{}", repo_url);
    tracing::debug!("Repository: {}", remote_url);

    // Get task label if present (to preserve it)
    let task = labels
        .get("io.devaipod.task")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Stop the pod first
    tracing::info!("Stopping containers...");
    let stop_output = podman_command()
        .args(["pod", "stop", pod_name])
        .output()
        .context("Failed to stop pod")?;

    if !stop_output.status.success() {
        tracing::debug!(
            "Pod stop returned non-zero (may already be stopped): {}",
            String::from_utf8_lossy(&stop_output.stderr).trim()
        );
    }

    // Remove the pod but keep volumes
    tracing::info!("Removing containers (keeping volumes)...");
    let rm_output = podman_command()
        .args(["pod", "rm", "--force", pod_name])
        .output()
        .context("Failed to remove pod")?;

    if !rm_output.status.success() {
        let stderr = String::from_utf8_lossy(&rm_output.stderr);
        bail!("Failed to remove pod: {}", stderr.trim());
    }

    // Clone the repo to get the latest devcontainer.json
    tracing::info!("Fetching latest devcontainer configuration...");
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let temp_path = temp_dir.path();

    let clone_output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            &remote_url,
            temp_path.to_str().unwrap(),
        ])
        .output()
        .await
        .context("Failed to clone repository")?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        bail!("Failed to clone repository: {}", stderr);
    }

    // Load devcontainer.json
    let devcontainer_json_path = devcontainer::try_find_devcontainer_json(temp_path);
    let devcontainer_config = if let Some(ref path) = devcontainer_json_path {
        devcontainer::load(path)?
    } else if image_override.is_some() {
        tracing::info!("No devcontainer.json found, using defaults with image override");
        devcontainer::DevcontainerConfig::default()
    } else {
        bail!(
            "No devcontainer.json found in repository.\n\
             Use --image to specify a container image."
        );
    };

    // Get the default branch name from the cloned repo
    let branch_output = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(temp_path)
        .output()
        .await
        .context("Failed to get default branch")?;

    let default_branch = if branch_output.status.success() {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    } else {
        "main".to_string()
    };

    // Extract repo name from URL
    let repo_name = git::extract_repo_name(&remote_url).unwrap_or_else(|| "workspace".to_string());

    // Create remote info for workspace source
    let remote_info = git::RemoteRepoInfo {
        remote_url: remote_url.clone(),
        default_branch,
        repo_name,
    };
    let source = pod::WorkspaceSource::RemoteRepo(remote_info);

    // Start podman service
    let podman = podman::PodmanService::spawn()
        .await
        .context("Failed to start podman service")?;

    let enable_gator = config.service_gator.is_enabled();
    let enable_network_isolation = config.network_isolation.enabled;

    // Build extra labels
    let mut extra_labels = Vec::new();
    if let Some(ref task_desc) = task {
        extra_labels.push(("io.devaipod.task".to_string(), task_desc.clone()));
    }

    // Recreate the pod - volumes already exist so they'll be reused
    // Note: We don't pass the task for rebuilds - the agent home volume persists
    // and contains the original task file, so it will be picked up on restart.
    tracing::info!("Recreating containers with new image...");
    let devaipod_pod = pod::DevaipodPod::create(
        &podman,
        temp_path,
        &devcontainer_config,
        pod_name,
        enable_gator,
        enable_network_isolation,
        config,
        &source,
        &extra_labels,
        None,
        image_override,
        None, // gator_image_override not yet supported for rebuild
        None, // task - agent home volume persists with original task
    )
    .await
    .context("Failed to recreate pod")?;

    let lifecycle_mode = if run_create {
        LifecycleMode::Full
    } else {
        LifecycleMode::Rebuild
    };

    finalize_pod_with_mode(
        &podman,
        &devaipod_pod,
        &devcontainer_config,
        config,
        lifecycle_mode,
    )
    .await?;

    tracing::info!(
        "Workspace '{}' rebuilt successfully",
        strip_pod_prefix(pod_name)
    );

    Ok(())
}

/// View container logs
fn cmd_logs(pod_name: &str, container: &str, follow: bool, tail: Option<u32>) -> Result<()> {
    let container_name = format!("{}-{}", pod_name, container);

    let mut cmd = podman_command();
    cmd.arg("logs");

    if follow {
        cmd.arg("-f");
    }

    // Convert tail to string outside of the conditional to ensure it lives long enough
    let tail_str;
    if let Some(n) = tail {
        tail_str = n.to_string();
        cmd.args(["--tail", &tail_str]);
    }

    cmd.arg(&container_name);

    let status = cmd.status().context("Failed to get container logs")?;

    if !status.success() {
        bail!(
            "Container '{}' not found or not running. Use 'devaipod list' to see pods.",
            container_name
        );
    }

    Ok(())
}

/// Show detailed status of a pod
fn cmd_status(pod_name: &str, json_output: bool) -> Result<()> {
    // Get pod info using podman pod inspect
    let pod_output = podman_command()
        .args(["pod", "inspect", pod_name])
        .output()
        .context("Failed to run podman pod inspect")?;

    if !pod_output.status.success() {
        let stderr = String::from_utf8_lossy(&pod_output.stderr);
        if stderr.contains("no such pod") || stderr.contains("not found") {
            bail!(
                "Pod '{}' not found. Use 'devaipod list' to see available pods.",
                pod_name
            );
        }
        bail!("podman pod inspect failed: {}", stderr.trim());
    }

    let pod_json_array: serde_json::Value =
        serde_json::from_slice(&pod_output.stdout).context("Failed to parse pod inspect output")?;

    // podman pod inspect returns an array, get the first element
    let pod_json = pod_json_array
        .as_array()
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or(pod_json_array);

    // Get container list using podman container ls
    let containers_output = podman_command()
        .args([
            "container",
            "ls",
            "--all",
            "--filter",
            &format!("pod={}", pod_name),
            "--format",
            "json",
        ])
        .output()
        .context("Failed to run podman container ls")?;

    let containers_json: serde_json::Value = if containers_output.status.success() {
        serde_json::from_slice(&containers_output.stdout).unwrap_or(serde_json::json!([]))
    } else {
        serde_json::json!([])
    };

    // Check agent health if pod is running
    let pod_state = pod_json
        .get("State")
        .and_then(|s| s.as_str())
        .unwrap_or("Unknown");

    let agent_health = if pod_state == "Running" {
        check_agent_health(pod_name)
    } else {
        None
    };

    // Get ports from pod
    let ports = extract_pod_ports(&pod_json);

    // Extract service-gator config from pod labels
    let gator_config = extract_service_gator_label(&pod_json);

    if json_output {
        // Build JSON output
        let status = serde_json::json!({
            "pod": {
                "name": pod_name,
                "state": pod_state,
                "id": pod_json.get("Id").and_then(|v| v.as_str()).unwrap_or(""),
            },
            "containers": containers_json,
            "agent_health": agent_health,
            "ports": ports,
            "service_gator": gator_config,
        });
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        // Human-readable output
        println!("Pod: {}", pod_name);
        println!("Status: {}", format_pod_state(pod_state));
        if let Some(id) = pod_json.get("Id").and_then(|v| v.as_str()) {
            // Show short ID
            println!("ID: {}", &id[..12.min(id.len())]);
        }
        println!();

        // Containers section
        println!("Containers:");
        if let Some(containers) = containers_json.as_array() {
            if containers.is_empty() {
                println!("  (none)");
            } else {
                for container in containers {
                    let name = container
                        .get("Names")
                        .and_then(|n| n.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let state = container
                        .get("State")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");
                    let image = container
                        .get("Image")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");
                    // Truncate image name for display
                    let image_display = if image.len() > 40 {
                        format!("{}...", &image[..37])
                    } else {
                        image.to_string()
                    };
                    println!(
                        "  {} - {} ({})",
                        name,
                        format_container_state(state),
                        image_display
                    );
                }
            }
        }
        println!();

        // Agent health section
        println!("Agent Health:");
        match agent_health {
            Some(true) => println!("  Healthy (responding at localhost:{})", pod::OPENCODE_PORT),
            Some(false) => println!("  Unhealthy (not responding)"),
            None => println!("  Unknown (pod not running)"),
        }
        println!();

        // Ports section
        println!("Exposed Ports:");
        if ports.is_empty() {
            println!("  (none)");
        } else {
            for port in &ports {
                println!("  {}", port);
            }
        }
        println!();

        // Service-gator section
        println!("Service-Gator:");
        if let Some(ref config) = gator_config {
            println!("  {}", config);
        } else {
            println!("  (not configured)");
        }
    }

    Ok(())
}

/// Debug and diagnose a workspace
///
/// Collects diagnostic information about the pod, gator, and agent.
fn cmd_debug(pod_name: &str, json_output: bool) -> Result<()> {
    use serde_json::json;

    // Get pod info
    let pod_output = podman_command()
        .args(["pod", "inspect", pod_name])
        .output()
        .context("Failed to run podman pod inspect")?;

    if !pod_output.status.success() {
        let stderr = String::from_utf8_lossy(&pod_output.stderr);
        if stderr.contains("no such pod") || stderr.contains("not found") {
            bail!(
                "Pod '{}' not found. Use 'devaipod list' to see available pods.",
                pod_name
            );
        }
        bail!("podman pod inspect failed: {}", stderr.trim());
    }

    let pod_json_array: serde_json::Value =
        serde_json::from_slice(&pod_output.stdout).context("Failed to parse pod inspect output")?;
    let pod_json = pod_json_array
        .as_array()
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or(pod_json_array);

    let pod_state = pod_json
        .get("State")
        .and_then(|s| s.as_str())
        .unwrap_or("Unknown");

    // Extract project name from labels
    let project_name = pod_json
        .get("Labels")
        .and_then(|l| l.get("io.devaipod.repo"))
        .and_then(|v| v.as_str())
        .map(|s| s.rsplit('/').next().unwrap_or(s))
        .unwrap_or("unknown");

    // Check gator container
    let gator_container = format!("{}-gator", pod_name);
    let gator_info = collect_gator_debug(&gator_container, project_name);

    // Check agent container
    let agent_info = collect_agent_debug(pod_name);

    // Check MCP connectivity
    let mcp_info = collect_mcp_debug(pod_name);

    if json_output {
        let debug_info = json!({
            "pod": {
                "name": pod_name,
                "state": pod_state,
                "project": project_name,
            },
            "gator": gator_info,
            "agent": agent_info,
            "mcp": mcp_info,
        });
        println!("{}", serde_json::to_string_pretty(&debug_info)?);
    } else {
        println!("=== Pod Debug: {} ===\n", pod_name);
        println!("State: {}", format_pod_state(pod_state));
        println!("Project: {}", project_name);
        println!();

        // Gator section
        println!("--- Gator Container ---");
        if let Some(info) = &gator_info {
            println!(
                "  Present: {}",
                if info.get("present").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "yes"
                } else {
                    "no"
                }
            );
            if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
                println!("  Version: {}", version);
            }
            if let Some(mount_type) = info.get("mount_type").and_then(|v| v.as_str()) {
                let readonly = info
                    .get("mount_readonly")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                println!(
                    "  Workspace mount: {} ({})",
                    mount_type,
                    if readonly { "read-only" } else { "read-write" }
                );
            }
            if let Some(git_ok) = info.get("git_accessible").and_then(|v| v.as_bool()) {
                println!(
                    "  Git accessible: {}",
                    if git_ok { "yes" } else { "NO - check mount!" }
                );
            }
        } else {
            println!("  (not present or error inspecting)");
        }
        println!();

        // Agent section
        println!("--- Agent Container ---");
        if let Some(info) = &agent_info {
            if let Some(healthy) = info.get("healthy").and_then(|v| v.as_bool()) {
                println!(
                    "  Health: {}",
                    if healthy {
                        "healthy"
                    } else {
                        "NOT responding"
                    }
                );
            }
            if let Some(mcp_config) = info.get("mcp_configured").and_then(|v| v.as_bool()) {
                println!(
                    "  MCP configured: {}",
                    if mcp_config { "yes" } else { "no" }
                );
            }
        } else {
            println!("  (error checking agent)");
        }
        println!();

        // MCP section
        println!("--- MCP Connectivity ---");
        if let Some(info) = &mcp_info {
            if let Some(reachable) = info.get("gator_reachable").and_then(|v| v.as_bool()) {
                println!(
                    "  Gator reachable from agent: {}",
                    if reachable { "yes" } else { "NO" }
                );
            }
        } else {
            println!("  (unable to check)");
        }
    }

    Ok(())
}

/// Collect debug info for the gator container
fn collect_gator_debug(
    gator_container: &str,
    project_name: &str,
) -> Option<serde_json::Value> {
    use serde_json::json;

    // Check if container exists
    let inspect_output = podman_command()
        .args(["inspect", gator_container])
        .output()
        .ok()?;

    if !inspect_output.status.success() {
        return Some(json!({ "present": false }));
    }

    let container_json: serde_json::Value =
        serde_json::from_slice(&inspect_output.stdout).ok()?;
    let container = container_json.as_array()?.first()?;

    // Get version
    let version_output = podman_command()
        .args(["exec", gator_container, "service-gator", "--version"])
        .output()
        .ok();
    let version = version_output
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    // Get mount info
    let mounts = container.get("Mounts")?.as_array()?;
    let workspace_mount = mounts.iter().find(|m| {
        m.get("Destination")
            .and_then(|d| d.as_str())
            .map(|d| d.starts_with("/workspaces"))
            .unwrap_or(false)
    });

    let (mount_type, mount_readonly) = workspace_mount
        .map(|m| {
            let t = m
                .get("Type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let rw = m.get("RW").and_then(|v| v.as_bool()).unwrap_or(true);
            (t.to_string(), !rw)
        })
        .unwrap_or(("none".to_string(), false));

    // Check if .git is accessible
    let git_path = format!("/workspaces/{}/.git", project_name);
    let git_check = podman_command()
        .args(["exec", gator_container, "test", "-d", &git_path])
        .status()
        .ok();
    let git_accessible = git_check.map(|s| s.success()).unwrap_or(false);

    Some(json!({
        "present": true,
        "version": version,
        "mount_type": mount_type,
        "mount_readonly": mount_readonly,
        "git_accessible": git_accessible,
    }))
}

/// Collect debug info for the agent container
fn collect_agent_debug(pod_name: &str) -> Option<serde_json::Value> {
    use serde_json::json;

    let agent_container = format!("{}-agent", pod_name);

    // Check health
    let healthy = check_agent_health(pod_name);

    // Check if MCP is configured (look at OPENCODE_CONFIG_CONTENT env)
    let env_check = podman_command()
        .args([
            "exec",
            &agent_container,
            "/bin/sh",
            "-c",
            "echo $OPENCODE_CONFIG_CONTENT",
        ])
        .output()
        .ok();
    let mcp_configured = env_check
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("service-gator"))
        .unwrap_or(false);

    Some(json!({
        "healthy": healthy,
        "mcp_configured": mcp_configured,
    }))
}

/// Collect MCP connectivity info
fn collect_mcp_debug(pod_name: &str) -> Option<serde_json::Value> {
    use serde_json::json;

    let agent_container = format!("{}-agent", pod_name);

    // Test if gator port is reachable from agent
    let port_check = format!("nc -z localhost {} 2>/dev/null", pod::GATOR_PORT);
    let gator_reachable = podman_command()
        .args(["exec", &agent_container, "/bin/sh", "-c", &port_check])
        .status()
        .ok()
        .map(|s| s.success())
        .unwrap_or(false);

    Some(json!({
        "gator_reachable": gator_reachable,
    }))
}

/// Check if the agent is listening on its port
fn check_agent_health(pod_name: &str) -> Option<bool> {
    let workspace_container = format!("{}-workspace", pod_name);

    // Use nc to check if the port is accepting connections.
    // This is more reliable than HTTP health checks since opencode's
    // endpoints may return errors during/after initialization.
    let check_cmd = format!("nc -z localhost {} 2>/dev/null", pod::OPENCODE_PORT);
    let result = podman_command()
        .args(["exec", &workspace_container, "/bin/sh", "-c", &check_cmd])
        .status();

    match result {
        Ok(status) => Some(status.success()),
        Err(_) => None,
    }
}

/// Extract service-gator config from pod labels
fn extract_service_gator_label(pod_json: &serde_json::Value) -> Option<String> {
    pod_json
        .get("Labels")
        .and_then(|labels| labels.get("io.devaipod.service-gator"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract exposed ports from pod inspect JSON
fn extract_pod_ports(pod_json: &serde_json::Value) -> Vec<String> {
    let mut ports = Vec::new();

    // Ports are typically in InfraConfig.PortBindings
    if let Some(infra) = pod_json.get("InfraConfig") {
        if let Some(bindings) = infra.get("PortBindings") {
            if let Some(obj) = bindings.as_object() {
                for (container_port, host_bindings) in obj {
                    if let Some(arr) = host_bindings.as_array() {
                        for binding in arr {
                            let host_ip = binding
                                .get("HostIp")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0.0.0.0");
                            let host_port = binding
                                .get("HostPort")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if !host_port.is_empty() {
                                ports.push(format!(
                                    "{}:{} -> {}",
                                    host_ip, host_port, container_port
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    ports
}

/// Format pod state for display
fn format_pod_state(state: &str) -> &str {
    match state {
        "Running" => "Running",
        "Stopped" => "Stopped",
        "Exited" => "Exited",
        "Created" => "Created",
        "Paused" => "Paused",
        "Degraded" => "Degraded",
        _ => state,
    }
}

/// Format container state for display
fn format_container_state(state: &str) -> &str {
    match state.to_lowercase().as_str() {
        "running" => "running",
        "exited" => "exited",
        "created" => "created",
        "paused" => "paused",
        "dead" => "dead",
        "removing" => "removing",
        _ => state,
    }
}

/// Generate shell completions
fn cmd_completions(shell: clap_complete::Shell) -> Result<()> {
    let mut cmd = HostCli::command();
    clap_complete::generate(shell, &mut cmd, "devaipod", &mut std::io::stdout());
    Ok(())
}

/// Check if we're running inside a devpod devcontainer
///
/// DevPod sets `DEVPOD=true` in devcontainers it creates.
/// This distinguishes devaipod devcontainers from other container
/// environments like toolbox containers.
fn is_inside_devcontainer() -> bool {
    std::env::var("DEVPOD")
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Standard path for the podman socket (started by devaipod-init.sh)
const PODMAN_SOCKET: &str = "/run/podman/podman.sock";

/// Configure the container environment for nested containers.
///
/// This command is idempotent and should be run at container startup.
/// It configures:
/// - /etc/containers/containers.conf with nested-friendly defaults
/// - /etc/subuid and /etc/subgid for nested user namespaces
/// - Starts the podman service at /run/podman/podman.sock
/// - Sets up /etc/profile.d/podman-remote.sh for CONTAINER_HOST
fn cmd_configure_env() -> Result<()> {
    // Must run as root
    if !rustix::process::geteuid().is_root() {
        bail!("configure-env must be run as root (use sudo)");
    }

    configure_containers_conf()?;
    configure_subuid()?;
    configure_podman_service()?;
    configure_profile()?;

    tracing::info!("Container environment configured successfully");
    Ok(())
}

/// Configure /etc/containers/containers.conf for nested containers
fn configure_containers_conf() -> Result<()> {
    let conf_dir = Path::new("/etc/containers");
    let conf_path = conf_dir.join("containers.conf");

    // Create directory if needed
    std::fs::create_dir_all(conf_dir).context("Failed to create /etc/containers")?;

    // Build the TOML configuration as a string (easier to include comments)
    let config_str = r#"[containers]
# Disable cgroups - nested cgroups don't work in user namespaces
cgroups = "disabled"
# Use host network - avoids network namespace issues
netns = "host"
# Use cgroupfs manager (systemd not available in containers)
cgroup_manager = "cgroupfs"
# Allow ping without special capabilities
default_sysctls = ["net.ipv4.ping_group_range=0 0"]

[engine]
cgroup_manager = "cgroupfs"
"#;

    // Check if already configured correctly
    if conf_path.exists() {
        let existing = std::fs::read_to_string(&conf_path).unwrap_or_default();
        if existing == config_str {
            tracing::debug!("containers.conf already configured");
            return Ok(());
        }
    }

    let full_config = format!(
        "# Generated by devaipod configure-env\n\
         # Optimized for nested container environments\n\n\
         {config_str}"
    );
    std::fs::write(&conf_path, &full_config).context("Failed to write containers.conf")?;

    tracing::info!("Configured {}", conf_path.display());
    Ok(())
}

/// Configure /etc/subuid and /etc/subgid for nested user namespaces
fn configure_subuid() -> Result<()> {
    // Find the container user
    let user = ["vscode", "devenv", "codespace"]
        .iter()
        .find(|u| {
            ProcessCommand::new("id")
                .arg(u)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .copied();

    let Some(user) = user else {
        tracing::debug!("No standard container user found, skipping subuid configuration");
        return Ok(());
    };

    // Parse /proc/self/uid_map to find max UID in this namespace
    let uid_map = std::fs::read_to_string("/proc/self/uid_map").unwrap_or_default();
    let max_uid: u64 = uid_map
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let inside: u64 = parts[0].parse().ok()?;
                let count: u64 = parts[2].parse().ok()?;
                Some(inside + count)
            } else {
                None
            }
        })
        .max()
        .unwrap_or(0);

    // If we have full UID range, default config should work
    if max_uid > 100000 {
        tracing::debug!(
            "Full UID range available (max={}), using default subuid",
            max_uid
        );
        return Ok(());
    }

    // Check if current subuid config already works
    let current_subuid = std::fs::read_to_string("/etc/subuid").unwrap_or_default();
    if let Some(line) = current_subuid
        .lines()
        .find(|l| l.starts_with(&format!("{}:", user)))
    {
        if let Some(start_str) = line.split(':').nth(1) {
            if let Ok(start) = start_str.parse::<u64>() {
                if start > 0 && start < max_uid {
                    tracing::debug!("subuid already configured correctly for {}", user);
                    return Ok(());
                }
            }
        }
    }

    // Reconfigure for constrained namespace
    let subuid_start: u64 = 10000;
    let subuid_count = max_uid.saturating_sub(subuid_start);

    if subuid_count < 1000 {
        tracing::warn!(
            "Limited UID range (max={}), nested podman may not work",
            max_uid
        );
        return Ok(());
    }

    let subuid_entry = format!("{}:{}:{}\n", user, subuid_start, subuid_count);

    std::fs::write("/etc/subuid", &subuid_entry).context("Failed to write /etc/subuid")?;
    std::fs::write("/etc/subgid", &subuid_entry).context("Failed to write /etc/subgid")?;

    tracing::info!(
        "Configured subuid/subgid: {}:{}:{}",
        user,
        subuid_start,
        subuid_count
    );

    // Reset podman storage if it exists (may have wrong mappings)
    let user_home = std::env::var("HOME").unwrap_or_else(|_| format!("/home/{}", user));
    let storage_path = PathBuf::from(&user_home).join(".local/share/containers/storage");
    if storage_path.exists() {
        tracing::info!("Resetting podman storage for new UID mappings");
        let _ = std::fs::remove_dir_all(&storage_path);
    }

    Ok(())
}

/// Start the podman service
fn configure_podman_service() -> Result<()> {
    let socket_path = Path::new(PODMAN_SOCKET);
    let socket_dir = socket_path.parent().unwrap();

    // Check if podman is available
    if !ProcessCommand::new("podman")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        tracing::debug!("podman not found, skipping service setup");
        return Ok(());
    }

    // Check if already running
    if socket_path.exists() {
        // Try to connect to verify it's working
        if ProcessCommand::new("podman")
            .args(["--remote", "info"])
            .env(
                "CONTAINER_HOST",
                format!("unix://{}", socket_path.display()),
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            tracing::debug!("Podman service already running");
            return Ok(());
        }
        // Socket exists but not working, remove it
        let _ = std::fs::remove_file(socket_path);
    }

    // Create socket directory
    std::fs::create_dir_all(socket_dir).context("Failed to create /run/podman")?;

    // Start podman service in background
    ProcessCommand::new("podman")
        .args(["system", "service", "--time=0"])
        .arg(format!("unix://{}", socket_path.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to start podman service")?;

    // Wait for socket to appear and chmod it
    for _ in 0..50 {
        if socket_path.exists() {
            // Make socket world-accessible
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o666);
            std::fs::set_permissions(socket_path, perms)?;
            tracing::info!("Podman service started at {}", socket_path.display());
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    bail!("Podman service did not start in time")
}

/// Configure /etc/profile.d/podman-remote.sh
fn configure_profile() -> Result<()> {
    let profile_path = Path::new("/etc/profile.d/devaipod-podman.sh");

    let content = r#"# Generated by devaipod configure-env
# Use rootful podman service (safe in rootless devcontainer)
if [ -S /run/podman/podman.sock ]; then
    export CONTAINER_HOST="unix:///run/podman/podman.sock"
fi
"#;

    // Check if already configured
    if profile_path.exists() {
        let existing = std::fs::read_to_string(profile_path).unwrap_or_default();
        if existing == content {
            tracing::debug!("Profile already configured");
            return Ok(());
        }
    }

    std::fs::write(profile_path, content).context("Failed to write profile.d script")?;
    tracing::info!("Configured {}", profile_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn test_host_cli_has_expected_commands() {
        // Verify HostCli has the expected host-only commands
        let cmd = HostCli::command();
        let subcommands: Vec<_> = cmd.get_subcommands().map(|c| c.get_name()).collect();

        assert!(subcommands.contains(&"up"), "Missing 'up' command");
        assert!(subcommands.contains(&"run"), "Missing 'run' command");
        assert!(subcommands.contains(&"ssh"), "Missing 'ssh' command");
        assert!(
            subcommands.contains(&"ssh-config"),
            "Missing 'ssh-config' command"
        );
        assert!(subcommands.contains(&"list"), "Missing 'list' command");
        assert!(subcommands.contains(&"stop"), "Missing 'stop' command");
        assert!(subcommands.contains(&"delete"), "Missing 'delete' command");
        assert!(subcommands.contains(&"logs"), "Missing 'logs' command");
        assert!(subcommands.contains(&"status"), "Missing 'status' command");
        assert!(
            subcommands.contains(&"completions"),
            "Missing 'completions' command"
        );
    }

    #[test]
    fn test_container_cli_has_expected_commands() {
        // Verify ContainerCli has the expected container-only commands
        let cmd = ContainerCli::command();
        let subcommands: Vec<_> = cmd.get_subcommands().map(|c| c.get_name()).collect();

        assert!(
            subcommands.contains(&"configure-env"),
            "Missing 'configure-env' command"
        );

        // Should NOT have host-only commands
        assert!(
            !subcommands.contains(&"up"),
            "'up' should not be in container CLI"
        );
    }

    #[test]
    fn test_is_inside_devcontainer_detection() {
        // This tests the detection function - result depends on runtime environment
        // Just verify it runs without panicking
        let _inside = is_inside_devcontainer();
    }
}
