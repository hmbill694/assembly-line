//! Where a job clones from and pushes to, and every rule about how that
//! remote can be spelled.

use crate::git;
use crate::paths::{LOCAL_HOST, RepoKey, UnkeyableRemote};
use std::path::Path;

/// The remote of a person's checkout that a job clones from and pushes its
/// branch back to.
pub const CHECKOUT_REMOTE: &str = "origin";

/// A remote as git takes it: a path, a `file://`, `ssh://` or other URL, or
/// git's scp-like `[user@]host:path`. Read once into what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    url: String,
    place: Place,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// A plain path, absolute or relative.
    Path,
    /// What follows `file://`.
    FileUrl { path: String },
    /// `ssh://` with a path, or scp-like: a host a token reaches over HTTPS
    /// instead.
    Ssh { host: Host, path: String },
    /// Any other scheme, `ssh://` with no path among them.
    OtherScheme {
        scheme: String,
        host: Host,
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Host {
    /// Without user info or port.
    name: String,
    /// What follows the colon after the host, which may be empty.
    port: Option<String>,
}

impl Host {
    fn from_authority(authority: &str) -> Host {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        match split_at_host_colon(host) {
            Some((name, port)) => Host {
                name: name.to_string(),
                port: Some(port.to_string()),
            },
            None => Host {
                name: host.to_string(),
                port: None,
            },
        }
    }

    /// The host as a key names it: lowercased, with a numeric port other
    /// than the scheme's default kept after a `+`, which no host name holds.
    /// Anything else after the colon is no port git can reach, and is
    /// dropped.
    fn key_segment(&self, scheme: &str) -> String {
        let name = self.name.to_ascii_lowercase();
        match self.port.as_deref() {
            Some(port)
                if !port.is_empty()
                    && port.chars().all(|c| c.is_ascii_digit())
                    && Some(port) != default_port(scheme) =>
            {
                format!("{name}+{port}")
            }
            _ => name,
        }
    }
}

fn default_port(scheme: &str) -> Option<&'static str> {
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Some("443"),
        "http" => Some("80"),
        "ssh" | "git+ssh" | "ssh+git" => Some("22"),
        "git" => Some("9418"),
        _ => None,
    }
}

impl Remote {
    #[must_use]
    pub fn from_url(url: impl Into<String>) -> Remote {
        let url = url.into();
        let place = match url.split_once("://") {
            Some(("file", path)) => Place::FileUrl {
                path: path.to_string(),
            },
            Some((scheme, rest)) => {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                let (host, path) = (Host::from_authority(authority), path.to_string());
                match (scheme, rest.contains('/')) {
                    ("ssh", true) => Place::Ssh { host, path },
                    _ => Place::OtherScheme {
                        scheme: scheme.to_string(),
                        host,
                        path,
                    },
                }
            }
            None => match scp_like_parts(&url) {
                Some((authority, path)) => Place::Ssh {
                    host: Host::from_authority(authority),
                    path: path.to_string(),
                },
                None => Place::Path,
            },
        };
        Remote { url, place }
    }

    /// The remote a job of `checkout` clones from: its [`CHECKOUT_REMOTE`],
    /// made to mean the same from the scratch directory the clone runs in.
    ///
    /// # Errors
    ///
    /// An error saying to add the remote when the checkout has none, or
    /// git's own when the remote cannot be listed or pushes elsewhere.
    pub async fn of_checkout(checkout: &Path) -> anyhow::Result<Remote> {
        let url = git::remote_url(checkout, CHECKOUT_REMOTE)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the repository has no '{CHECKOUT_REMOTE}' remote — a job clones from it and \
                     pushes its branch back to it, so add one"
                )
            })?;
        Ok(Remote::from_url(url).resolved_against(checkout))
    }

    /// Git reads a relative path from the repository whose remote it is;
    /// from anywhere else it names nothing.
    fn resolved_against(self, repo: &Path) -> Remote {
        match self.place {
            Place::Path if !Path::new(&self.url).is_absolute() => {
                Remote::from_url(repo.join(&self.url).to_string_lossy().into_owned())
            }
            _ => self,
        }
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether the remote is a path on this machine, plain or `file://`,
    /// rather than a host.
    #[must_use]
    pub fn is_path_on_this_machine(&self) -> bool {
        matches!(self.place, Place::Path | Place::FileUrl { .. })
    }

    /// The HTTPS form of an SSH remote, which is what a token can
    /// authenticate. Any other remote comes back as it is.
    #[must_use]
    pub fn https_equivalent(&self) -> String {
        match &self.place {
            // scp-like `host:/srv/r.git` names an absolute path, which a URL
            // carries with a single slash.
            Place::Ssh { host, path } => {
                format!("https://{}/{}", host.name, path.trim_start_matches('/'))
            }
            Place::Path | Place::FileUrl { .. } | Place::OtherScheme { .. } => self.url.clone(),
        }
    }

    /// The `--repo` a round is launched with. A container has no SSH keys,
    /// so it reaches the remote over HTTPS. On the host an absolute path is
    /// passed as a `file://` URL: `run --repo` reads a path holding `.git`
    /// as a checkout and clones that checkout's own remote, and a remote
    /// that is a non-bare repository holds one.
    #[must_use]
    pub fn url_for_launch(&self, in_a_container: bool) -> String {
        match (in_a_container, Path::new(&self.url).is_absolute()) {
            (true, _) => self.https_equivalent(),
            (false, true) => format!("file://{}", self.url),
            (false, false) => self.url.clone(),
        }
    }

    /// The repository's place under the state root: its host, then the path
    /// segments of its HTTPS form, so the SSH and HTTPS spellings of one
    /// repository are one key. A repository on this machine has the host
    /// [`LOCAL_HOST`]. A trailing `/` and one `.git` are no part of it.
    ///
    /// # Errors
    ///
    /// When the remote is a relative path, or a segment is empty, `.` or
    /// `..`, or holds anything but ASCII letters, digits, `.`, `_` and `-`:
    /// validated, never sanitized.
    pub fn repo_key(&self) -> Result<RepoKey, UnkeyableRemote> {
        let (host, path) = match &self.place {
            Place::Path if self.url.starts_with('/') => (LOCAL_HOST.to_string(), self.url.as_str()),
            Place::Path => return Err(self.unkeyable()),
            Place::FileUrl { path } => (LOCAL_HOST.to_string(), path.as_str()),
            Place::Ssh { host, path } => (host.key_segment("ssh"), path.as_str()),
            Place::OtherScheme { scheme, host, path } => (host.key_segment(scheme), path.as_str()),
        };
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        RepoKey::from_host_and_path(host, path.split('/').filter(|segment| !segment.is_empty()))
            .ok_or_else(|| self.unkeyable())
    }

    fn unkeyable(&self) -> UnkeyableRemote {
        UnkeyableRemote {
            url: self.url.clone(),
        }
    }
}

impl std::fmt::Display for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.url)
    }
}

/// git's scp-like `[user@]host:path`, split at the colon that ends the host,
/// or `None` for anything else. git recognises it by a colon before any
/// slash; a URL with a scheme is never one, and a local path has no such
/// colon.
fn scp_like_parts(url: &str) -> Option<(&str, &str)> {
    (!url.contains("://"))
        .then(|| split_at_host_colon(url))
        .flatten()
        .filter(|(authority, _)| !authority.contains('/'))
}

/// `s` split at the colon that ends a host: the first one outside a
/// bracketed IPv6 literal, whose own colons end nothing.
fn split_at_host_colon(s: &str) -> Option<(&str, &str)> {
    let searched_from = match (s.find('['), s.find(':')) {
        (Some(open), Some(colon)) if open < colon => {
            s[open..].find(']').map_or(open, |close| open + close)
        }
        _ => 0,
    };
    let colon = searched_from + s[searched_from..].find(':')?;
    Some((&s[..colon], &s[colon + 1..]))
}
