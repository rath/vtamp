use crate::{model::*, platform::Paths, wire};
use anyhow::{Context, Result, bail};
use std::{
    fs::OpenOptions,
    os::unix::process::CommandExt,
    process::{Command as Process, Stdio},
    time::Duration,
};
use tokio::net::UnixStream;

#[derive(Clone)]
pub struct Client {
    pub paths: Paths,
}

/// How a server should be started when none is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Device {
        /// Also cast the audio played through the device.
        cast: bool,
        /// Serve the cast over HTTP at this address.
        http: Option<std::net::SocketAddr>,
        /// Serve the JSON API and track files over HTTP at this address.
        api: Option<std::net::SocketAddr>,
    },
    Headless {
        http: Option<std::net::SocketAddr>,
        api: Option<std::net::SocketAddr>,
    },
    /// Relay the server reachable at this socket path.
    Relay(std::path::PathBuf),
}

impl Launch {
    pub fn mode(&self) -> ServerMode {
        match self {
            Self::Device { .. } => ServerMode::Device,
            Self::Headless { .. } => ServerMode::Headless,
            Self::Relay(_) => ServerMode::Relay,
        }
    }
}

impl Client {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }
    pub async fn request(&self, command: Command) -> Result<Reply> {
        let mut stream = self.connect().await?;
        tokio::time::timeout(Duration::from_secs(125), async {
            wire::write(
                &mut stream,
                &Request {
                    version: PROTOCOL_VERSION,
                    request: command,
                },
            )
            .await?;
            let reply: Reply = wire::read(&mut stream).await?;
            if reply.version != PROTOCOL_VERSION {
                return Err(ApiError::new(
                    "version_mismatch",
                    "Client and server protocol versions differ; restart the server",
                )
                .into());
            }
            Ok(reply)
        })
        .await
        .context("Command timed out; outcome is unknown. Inspect status before retrying")?
    }
    async fn connect(&self) -> Result<UnixStream> {
        tokio::time::timeout(
            Duration::from_secs(3),
            UnixStream::connect(self.paths.socket()),
        )
        .await?
        .map_err(|e| {
            ApiError::new(
                "server_unavailable",
                format!("Cannot connect to vtamp: {e}. Run vtamp server start"),
            )
            .into()
        })
    }
    pub async fn watch(&self) -> Result<(State, UnixStream)> {
        let mut stream = self.connect().await?;
        wire::write(
            &mut stream,
            &Request {
                version: PROTOCOL_VERSION,
                request: Command::Watch,
            },
        )
        .await?;
        let reply: Reply =
            tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream)).await??;
        if reply.version != PROTOCOL_VERSION {
            bail!("Incompatible server protocol");
        }
        let state = serde_json::from_value(reply.into_data()?)?;
        Ok((state, stream))
    }
    /// Independent, optional stream; never starts a server or changes playback.
    pub async fn spectrum(&self) -> Result<(crate::spectrum::SpectrumFrame, UnixStream)> {
        let mut stream = self.connect().await?;
        wire::write(
            &mut stream,
            &Request {
                version: PROTOCOL_VERSION,
                request: Command::SpectrumWatch,
            },
        )
        .await?;
        let reply: Reply =
            tokio::time::timeout(Duration::from_secs(3), wire::read(&mut stream)).await??;
        if reply.version != PROTOCOL_VERSION {
            bail!("Incompatible server protocol");
        }
        Ok((serde_json::from_value(reply.into_data()?)?, stream))
    }
    /// Subscribe to the audio cast. After the returned description the
    /// connection carries raw Ogg Opus pages; never starts a server.
    pub async fn cast(&self) -> Result<(CastInfo, UnixStream)> {
        let mut stream = self.connect().await?;
        wire::write(
            &mut stream,
            &Request {
                version: PROTOCOL_VERSION,
                request: Command::CastWatch,
            },
        )
        .await?;
        let reply: Reply =
            tokio::time::timeout(Duration::from_secs(3), wire::read(&mut stream)).await??;
        if reply.version != PROTOCOL_VERSION {
            bail!("Incompatible server protocol");
        }
        Ok((serde_json::from_value(reply.into_data()?)?, stream))
    }
    pub async fn cast_info(&self) -> Result<CastInfo> {
        Ok(serde_json::from_value(
            self.request(Command::CastInfo).await?.into_data()?,
        )?)
    }
    pub async fn server_info(&self) -> Result<ServerInfo> {
        Ok(serde_json::from_value(
            self.request(Command::ServerInfo).await?.into_data()?,
        )?)
    }
    pub async fn ensure(&self) -> Result<()> {
        self.ensure_with(&Launch::Device {
            cast: false,
            http: None,
            api: None,
        })
        .await
    }
    /// Start a server if none is reachable, in the requested mode. An already
    /// running server keeps its own mode.
    pub async fn ensure_with(&self, launch: &Launch) -> Result<()> {
        // A relay answers server_info itself even while its remote is down;
        // other servers prove their player thread with a status round trip.
        let probe = || match launch {
            Launch::Relay(_) => Command::ServerInfo,
            _ => Command::Status,
        };
        // Never spawn over a reachable server, even if its protocol is incompatible.
        if UnixStream::connect(self.paths.socket()).await.is_ok() {
            self.request(probe()).await?.into_data()?;
            return Ok(());
        }
        self.paths.prepare()?;
        if self
            .paths
            .log()
            .metadata()
            .is_ok_and(|m| m.len() > 5 * 1024 * 1024)
        {
            let _ = std::fs::rename(
                self.paths.log(),
                self.paths.data.join("server.previous.log"),
            );
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.log())?;
        let mut command = Process::new(std::env::current_exe()?);
        command.arg("server").arg("run");
        match launch {
            Launch::Device { cast, http, api } => {
                if *cast {
                    command.arg("--cast");
                }
                if let Some(http) = http {
                    command.arg("--cast-http").arg(http.to_string());
                }
                if let Some(api) = api {
                    command.arg("--api").arg(api.to_string());
                }
            }
            Launch::Headless { http, api } => {
                command.arg("--headless");
                if let Some(http) = http {
                    command.arg("--cast-http").arg(http.to_string());
                }
                if let Some(api) = api {
                    command.arg("--api").arg(api.to_string());
                }
            }
            Launch::Relay(remote) => {
                command.arg("--remote").arg(remote);
            }
        }
        command
            .current_dir(&self.paths.data)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        // setsid is async-signal-safe and disconnects the child from the launching terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::umask(0o077);
                Ok(())
            });
        }
        let mut child = command.spawn().context("Cannot start background server")?;
        for _ in 0..100 {
            if UnixStream::connect(self.paths.socket()).await.is_ok() {
                self.request(probe()).await?.into_data()?;
                // Reap when a concurrent starter won, or when the daemon eventually stops.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let exit = child.try_wait()?;
        bail!(
            "Server did not become ready (exit: {exit:?}). See {}",
            self.paths.log().display()
        )
    }
}
