use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task
};

use crate::error::{OpenBoltError, OpenBoltResult};

pub enum KvmMode {
    Server,
    Client
}

pub async fn start(mode: KvmMode, local_ip: &str, peer_ip: &str) -> OpenBoltResult<Child> {
    let mut command = Command::new("lan-mouse");
    command.arg("--daemon");

    match mode {
        KvmMode::Server => {
            command.args(["--server", "--bind", &format!("{local_ip}:4242")]);
        }
        KvmMode::Client => {
            command.args(["--client", "--connect", &format!("{peer_ip}:4242")]);
        }
    }

    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn()?;

    if let Some(stdout) = child.stdout.take() {
        task::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "openbolt::kvm", "{line}");
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        task::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "openbolt::kvm", "{line}");
            }
        });
    }

    if child.id().is_none() {
        return Err(OpenBoltError::CommandFailed(
            "failed to launch lan-mouse".to_string()
        ));
    }

    Ok(child)
}
