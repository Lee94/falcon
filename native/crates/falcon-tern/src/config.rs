use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use falcon_client::{ApiError, FalconClient};
use serde::Deserialize;
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    url: String,
    password_file: Option<PathBuf>,
    desktop_passwords: Option<PathBuf>,
    profile: Option<String>,
    sessiond_root: Option<PathBuf>,
}

pub struct Connection {
    pub client: FalconClient,
    pub sessiond_root: Option<PathBuf>,
}

fn referenced_path(config_path: &Path, value: PathBuf) -> PathBuf {
    if value.is_absolute() {
        value
    } else {
        config_path.parent().unwrap_or(Path::new(".")).join(value)
    }
}

fn server_url(value: &str) -> Result<Url> {
    // Do not include the input in errors: it could contain a mistakenly pasted credential.
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("config url must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("config url must be an absolute HTTP(S) URL");
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        bail!("config url must not contain credentials, a query, or a fragment");
    }
    Ok(url)
}

pub async fn request<T>(future: impl Future<Output = Result<T, ApiError>>) -> Result<T> {
    let result = tokio::time::timeout(REQUEST_TIMEOUT, future).await.context("Falcon request timed out")?;
    match result {
        Err(error) if error.is_unauthorized() => {
            bail!("Falcon authentication required or saved password rejected; configure passwordFile or desktopPasswords/profile");
        }
        other => other.map_err(Into::into),
    }
}

pub async fn load_connection(path: &Path) -> Result<Connection> {
    let bytes = tokio::fs::read(path).await.context("cannot read Falcon Tern config")?;
    let config: Config = serde_json::from_slice(&bytes).context("invalid Falcon Tern config JSON")?;
    let url = server_url(&config.url)?;
    if config.sessiond_root.is_some()
        && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
    {
        bail!("sessiondRoot requires a loopback Falcon URL on the same machine");
    }
    let sessiond_root = config.sessiond_root.map(|root| referenced_path(path, root));
    let password = if let Some(password_file) = config.password_file {
        let value = tokio::fs::read_to_string(referenced_path(path, password_file))
            .await.context("cannot read passwordFile")?;
        Some(value.trim_end_matches(['\r', '\n']).to_owned())
    } else if let Some(passwords_file) = config.desktop_passwords {
        let bytes = tokio::fs::read(referenced_path(path, passwords_file))
            .await.context("cannot read desktopPasswords")?;
        let mut passwords: BTreeMap<String, String> = serde_json::from_slice(&bytes)
            .context("desktopPasswords must be a JSON object mapping profile IDs to passwords")?;
        Some(passwords.remove(config.profile.as_deref().unwrap_or("local"))
            .context("profile not found in desktopPasswords")?)
    } else {
        None
    };
    let client = FalconClient::new(url);
    client.set_relogin_password(password);
    // The first protected request performs login and REST replay through FalconClient.
    Ok(Connection { client, sessiond_root })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rejects_credentials_and_non_http_schemes() {
        for value in ["file:///tmp/falcon", "ws://localhost", "http://user:secret@localhost", "http://localhost?password=secret"] {
            assert!(server_url(value).is_err());
        }
        assert_eq!(server_url("https://example.com/falcon/").unwrap().path(), "/falcon/");
    }
}
