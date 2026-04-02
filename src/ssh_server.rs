//! Embedded SSH server for remote development
//!
//! This module provides a pure-Rust SSH server that runs on the host and
//! translates SSH requests into `podman exec` commands, enabling VSCode/Zed
//! Remote SSH to connect to devaipod containers without requiring a traditional
//! SSH daemon like dropbear or openssh inside the container.
//!
//! The SSH server speaks the real SSH protocol but uses stdin/stdout as the
//! transport layer. When used as a ProxyCommand, the SSH client on the host
//! connects through `devaipod exec --stdio <pod>` which runs this server.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use color_eyre::eyre::{Context as _, Result};
use pin_project::pin_project;
use russh::keys::PublicKey;
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Run the SSH server over stdio for a specific container
///
/// This is the main entry point called by `devaipod exec --stdio <pod>`.
/// The SSH server runs on the host and translates SSH shell/exec requests
/// into `podman exec` commands targeting the specified container.
pub async fn run_stdio_for_container(container: &str) -> Result<()> {
    let config = make_server_config()?;
    let handler = SshHandler::new(container.to_string());

    let stream = StdioStream::new();

    tracing::debug!("Starting SSH server over stdio for container {}", container);

    // Run the SSH server over stdin/stdout
    let session = russh::server::run_stream(Arc::new(config), stream, handler)
        .await
        .context("Failed to run SSH server")?;

    // Wait for the session to complete
    session.await.context("SSH session failed")?;

    Ok(())
}

/// Create the SSH server configuration
fn make_server_config() -> Result<russh::server::Config> {
    use russh::keys::{Algorithm, PrivateKey};

    // Generate an ephemeral host key for this session
    // This is fine because we're running over a trusted channel
    let key = PrivateKey::random(
        &mut russh::keys::ssh_key::rand_core::OsRng,
        Algorithm::Ed25519,
    )
    .context("Failed to generate host key")?;

    Ok(russh::server::Config {
        keys: vec![key],
        // No auth needed - the channel is already authenticated locally
        auth_rejection_time: std::time::Duration::from_secs(0),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        ..Default::default()
    })
}

/// Combined stdin/stdout stream for SSH transport
#[pin_project]
pub struct StdioStream {
    #[pin]
    stdin: tokio::io::Stdin,
    #[pin]
    stdout: tokio::io::Stdout,
}

impl StdioStream {
    pub fn new() -> Self {
        Self {
            stdin: tokio::io::stdin(),
            stdout: tokio::io::stdout(),
        }
    }
}

impl AsyncRead for StdioStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().stdin.poll_read(cx, buf)
    }
}

impl AsyncWrite for StdioStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().stdout.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().stdout.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().stdout.poll_shutdown(cx)
    }
}

/// SSH server handler implementing the russh server::Handler trait
struct SshHandler {
    /// Target container name for podman exec
    container: String,
    /// Active channels and their state
    channels: HashMap<ChannelId, ChannelState>,
    /// Senders for routing stdin data to spawned processes
    input_senders: HashMap<ChannelId, mpsc::Sender<Vec<u8>>>,
}

struct ChannelState {
    /// PTY information if allocated
    pty: Option<PtyInfo>,
}

#[allow(dead_code)]
struct PtyInfo {
    term: String,
    cols: u32,
    rows: u32,
}

impl SshHandler {
    fn new(container: String) -> Self {
        Self {
            container,
            channels: HashMap::new(),
            input_senders: HashMap::new(),
        }
    }
}

impl russh::server::Handler for SshHandler {
    type Error = color_eyre::eyre::Error;

    /// Accept any authentication - we trust the local connection
    fn auth_none(&mut self, _user: &str) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        std::future::ready(Ok(Auth::Accept))
    }

    fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        std::future::ready(Ok(Auth::Accept))
    }

    fn auth_publickey(
        &mut self,
        _user: &str,
        _public_key: &PublicKey,
    ) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        std::future::ready(Ok(Auth::Accept))
    }

    fn auth_publickey_offered(
        &mut self,
        _user: &str,
        _public_key: &PublicKey,
    ) -> impl Future<Output = Result<Auth, Self::Error>> + Send {
        std::future::ready(Ok(Auth::Accept))
    }

    /// Accept session channel requests
    fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        self.channels
            .insert(channel.id(), ChannelState { pty: None });
        std::future::ready(Ok(true))
    }

    /// Handle PTY allocation requests
    fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        if let Some(state) = self.channels.get_mut(&channel) {
            state.pty = Some(PtyInfo {
                term: term.to_string(),
                cols: col_width,
                rows: row_height,
            });
        }
        let result = session.channel_success(channel);
        async move { result.map_err(Into::into) }
    }

    /// Handle window size changes
    fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        if let Some(state) = self.channels.get_mut(&channel)
            && let Some(pty) = &mut state.pty
        {
            pty.cols = col_width;
            pty.rows = row_height;
            // TODO: Send SIGWINCH to the process if running with PTY
        }
        std::future::ready(Ok(()))
    }

    /// Handle shell requests - start an interactive shell via podman exec
    fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let result = session.channel_success(channel);
        let handle = session.handle();
        let container = self.container.clone();

        // Create channel for stdin routing
        let (tx, rx) = mpsc::channel(64);
        self.input_senders.insert(channel, tx);

        // Check if PTY was requested
        let has_pty = self
            .channels
            .get(&channel)
            .and_then(|s| s.pty.as_ref())
            .is_some();

        async move {
            result?;
            tokio::spawn(async move {
                if let Err(e) =
                    run_podman_exec(handle, channel, &container, None, rx, has_pty).await
                {
                    tracing::error!("Shell error: {}", e);
                }
            });
            Ok(())
        }
    }

    /// Handle exec requests - run a specific command via podman exec
    fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let command = String::from_utf8_lossy(data).to_string();
        let result = session.channel_success(channel);
        let handle = session.handle();
        let container = self.container.clone();

        // Create channel for stdin routing
        let (tx, rx) = mpsc::channel(64);
        self.input_senders.insert(channel, tx);

        // Check if PTY was requested
        let has_pty = self
            .channels
            .get(&channel)
            .and_then(|s| s.pty.as_ref())
            .is_some();

        async move {
            result?;
            tokio::spawn(async move {
                if let Err(e) =
                    run_podman_exec(handle, channel, &container, Some(&command), rx, has_pty).await
                {
                    tracing::error!("Exec error: {}", e);
                }
            });
            Ok(())
        }
    }

    /// Handle SFTP subsystem requests
    fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let is_sftp = name == "sftp";
        let result = if is_sftp {
            session.channel_success(channel_id)
        } else {
            session.channel_failure(channel_id)
        };
        let handle = session.handle();
        let container = self.container.clone();

        // Create channel for stdin routing
        let (tx, rx) = mpsc::channel(64);
        if is_sftp {
            self.input_senders.insert(channel_id, tx);
        }

        async move {
            result?;
            if is_sftp {
                tokio::spawn(async move {
                    if let Err(e) = run_sftp(handle, channel_id, &container, rx).await {
                        tracing::error!("SFTP error: {}", e);
                    }
                });
            }
            Ok(())
        }
    }

    /// Handle incoming data from client - route to spawned process
    fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        // Route data to the spawned process for this channel
        if let Some(tx) = self.input_senders.get(&channel) {
            let tx = tx.clone();
            let data = data.to_vec();
            return std::future::ready(
                // Use try_send to avoid blocking - if buffer is full, drop data
                match tx.try_send(data) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!("Input buffer full, dropping data");
                        Ok(())
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
                },
            );
        }
        std::future::ready(Ok(()))
    }

    /// Handle channel EOF - close the input sender
    fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        // Remove the sender to signal EOF to the spawned process
        self.input_senders.remove(&channel);
        std::future::ready(Ok(()))
    }

    /// Handle channel close
    fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.channels.remove(&channel);
        self.input_senders.remove(&channel);
        std::future::ready(Ok(()))
    }

    /// Handle direct-tcpip (local port forwarding)
    ///
    /// This forwards to the container's network namespace via podman exec
    fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut Session,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        let target = format!("{}:{}", host_to_connect, port_to_connect);
        let container = self.container.clone();

        async move {
            tokio::spawn(async move {
                if let Err(e) = handle_direct_tcpip(channel, &container, &target).await {
                    tracing::debug!("Port forward to {} failed: {}", target, e);
                }
            });
            Ok(true)
        }
    }
}

/// Run a command in the container via podman exec
async fn run_podman_exec(
    handle: russh::server::Handle,
    channel: ChannelId,
    container: &str,
    command: Option<&str>,
    mut input_rx: mpsc::Receiver<Vec<u8>>,
    has_pty: bool,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut cmd = podman_command_async();
    cmd.arg("exec");

    // Add -t for PTY mode if requested
    if has_pty {
        cmd.arg("-it");
    } else {
        cmd.arg("-i");
    }

    cmd.arg(container);

    if let Some(command) = command {
        // Run the command via sh -c for proper parsing
        cmd.args(["sh", "-c", command]);
    } else {
        // Interactive shell - try bash first, fall back to sh
        cmd.args(["sh", "-c", "exec bash 2>/dev/null || exec sh"]);
    }

    // Set up stdio for the process
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("Failed to spawn podman exec")?;

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();

    // Spawn task to read from process stdout and send to channel
    let handle_stdout = handle.clone();
    let stdout_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if handle_stdout
                        .data(channel, buf[..n].to_vec())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    tracing::trace!("stdout read error: {}", e);
                    break;
                }
            }
        }
    });

    // Spawn task to read from process stderr and send to channel (extended data)
    let handle_stderr = handle.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if handle_stderr
                        .extended_data(channel, 1, buf[..n].to_vec())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    tracing::trace!("stderr read error: {}", e);
                    break;
                }
            }
        }
    });

    // Spawn task to read from input channel and write to process stdin
    let stdin_task = tokio::spawn(async move {
        while let Some(data) = input_rx.recv().await {
            if stdin.write_all(&data).await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
        // Close stdin when the receiver is closed (EOF)
        drop(stdin);
    });

    // Wait for the process to exit
    let status = child
        .wait()
        .await
        .context("Failed to wait for podman exec")?;
    let exit_code = status.code().unwrap_or(1) as u32;

    // Wait for output tasks to complete
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    stdin_task.abort(); // Stop waiting for input

    // Send exit status and close channel
    let _ = handle.exit_status_request(channel, exit_code).await;
    let _ = handle.close(channel).await;

    Ok(())
}

/// Async version of podman_command for tokio
fn podman_command_async() -> Command {
    let podman_path = std::env::var("PODMAN_PATH").unwrap_or_else(|_| "podman".to_string());
    let mut cmd = Command::new(podman_path);
    // Use container socket if available
    if let Ok(socket_path) = crate::podman::get_container_socket() {
        cmd.arg("--url");
        cmd.arg(format!("unix://{}", socket_path.display()));
    }
    cmd
}

/// Handle SFTP subsystem by running sftp-server in the container
async fn run_sftp(
    handle: russh::server::Handle,
    channel: ChannelId,
    container: &str,
    input_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    // Try multiple sftp-server paths (Debian/Ubuntu vs RHEL/Fedora)
    let sftp_command = "for p in /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server; do [ -x $p ] && exec $p -e; done; echo 'sftp-server not found' >&2; exit 1";
    run_podman_exec(
        handle,
        channel,
        container,
        Some(sftp_command),
        input_rx,
        false,
    )
    .await
}

/// Handle direct TCP/IP port forwarding via the container
async fn handle_direct_tcpip(channel: Channel<Msg>, container: &str, target: &str) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Parse host:port
    let parts: Vec<&str> = target.split(':').collect();
    if parts.len() != 2 {
        color_eyre::eyre::bail!("Invalid target: {}", target);
    }
    let host = parts[0];
    let port = parts[1];

    // Use bash's /dev/tcp for port forwarding (more reliable than socat which may not exist)
    // Fall back to nc/netcat if available
    let forward_cmd = format!(
        "exec bash -c 'exec 3<>/dev/tcp/{}/{} && cat <&3 & cat >&3' 2>/dev/null || \
         nc {} {} 2>/dev/null || \
         echo 'No port forwarding tool available' >&2",
        host, port, host, port
    );

    let mut cmd = podman_command_async();
    cmd.arg("exec")
        .arg("-i")
        .arg(container)
        .args(["sh", "-c", &forward_cmd]);

    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let mut child = cmd.spawn().context("Failed to spawn port forward")?;

    let mut stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();

    let channel_stream = channel.into_stream();
    let (mut ch_read, mut ch_write) = tokio::io::split(channel_stream);

    // Read from container stdout and write to channel
    let handle_read = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if ch_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Read from channel and write to container stdin
    let handle_write = tokio::spawn(async move {
        let mut stdin = stdin;
        let mut buf = vec![0u8; 4096];
        loop {
            match ch_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdin.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let _ = handle_read.await;
    let _ = handle_write.await;
    let _ = child.wait().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config() {
        let config = make_server_config().unwrap();
        assert!(!config.keys.is_empty());
    }
}
