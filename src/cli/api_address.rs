use anyhow::{Context, Result};
use std::{
    net::{Ipv4Addr, SocketAddr},
    process::Command,
    str::FromStr,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiAddress {
    Address(SocketAddr),
    Tailscale,
}

impl FromStr for ApiAddress {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "tailscale" {
            Ok(Self::Tailscale)
        } else {
            value
                .parse()
                .map(Self::Address)
                .map_err(|_| "Expected an IP:PORT address (IPv6: [IP]:PORT) or 'tailscale'".into())
        }
    }
}

impl ApiAddress {
    pub async fn resolve(self) -> Result<SocketAddr> {
        match self {
            Self::Address(address) => Ok(address),
            Self::Tailscale => {
                tokio::task::spawn_blocking(|| {
                    let executable = crate::subprocess::executable(None, "tailscale")
                        .context("--api tailscale requires the Tailscale CLI on PATH")?;
                    tailscale_address(&mut Command::new(executable))
                })
                .await?
            }
        }
    }
}

fn tailscale_address(command: &mut Command) -> Result<SocketAddr> {
    let bytes = crate::subprocess::run(
        command.args(["ip", "-4"]),
        None,
        &crate::subprocess::cancel(),
        Duration::from_secs(5),
        |_| {},
    )
    .context("Cannot get the Tailscale address; check that Tailscale is running and connected")?;
    let address = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.trim().parse::<Ipv4Addr>().ok())
        .context(
            "tailscale ip -4 did not return one IPv4 address; check the Tailscale connection",
        )?;
    Ok(SocketAddr::from((address, 8700)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Action, Args, Server};
    use clap::Parser;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn api_accepts_tailscale_and_explicit_addresses() {
        for (value, expected) in [
            ("tailscale", ApiAddress::Tailscale),
            (
                "127.0.0.1:8700",
                ApiAddress::Address("127.0.0.1:8700".parse().unwrap()),
            ),
            (
                "[::1]:8800",
                ApiAddress::Address("[::1]:8800".parse().unwrap()),
            ),
        ] {
            let args = Args::try_parse_from(["vtamp", "server", "start", "--api", value]).unwrap();
            assert!(matches!(args.command, Some(Action::Server {
                command: Server::Start { api: Some(actual), .. }
            }) if actual == expected));
        }
        for value in ["tailscale:8800", "localhost:8700", "100.64.0.1"] {
            assert!(Args::try_parse_from(["vtamp", "server", "start", "--api", value]).is_err());
        }
        assert!(Args::try_parse_from(["vtamp", "server", "start", "--api"]).is_err());
        assert!(
            Args::try_parse_from([
                "vtamp",
                "server",
                "start",
                "--api",
                "tailscale",
                "--remote",
                "/tmp/remote.sock"
            ])
            .is_err()
        );
    }

    #[test]
    fn tailscale_lookup_checks_arguments_output_and_exit_status() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("tailscale");
        for (body, succeeds) in [
            ("printf '100.101.102.103\\n'", true),
            ("printf ''", false),
            ("printf 'not an address\\n'", false),
            ("printf 'fd7a:115c:a1e0::1\\n'", false),
            ("printf '100.101.102.103\\n100.101.102.104\\n'", false),
            (
                "printf '100.101.102.103\\n'; echo disconnected >&2; exit 1",
                false,
            ),
        ] {
            std::fs::write(
                &executable,
                format!("#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = ip ] && [ \"$2\" = -4 ] || exit 2\n{body}\n"),
            ).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = tailscale_address(&mut Command::new(&executable));
            if succeeds {
                assert_eq!(result.unwrap(), "100.101.102.103:8700".parse().unwrap());
            } else {
                assert!(result.is_err(), "{body}");
            }
        }
        assert!(tailscale_address(&mut Command::new(temp.path().join("missing"))).is_err());
    }
}
