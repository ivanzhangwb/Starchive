//! Local settings and executable discovery. Callers should use a blocking worker.

use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use directories::BaseDirs;
use serde::Serialize;

#[cfg(target_os = "macos")]
use apple_native_keyring_store::keychain::Store as KeyringStore;
#[cfg(target_os = "linux")]
use dbus_secret_service_keyring_store::store::Store as KeyringStore;
#[cfg(windows)]
use windows_native_keyring_store::store::Store as KeyringStore;

const TOKEN_VARS: [&str; 3] = ["GH_TOKEN", "GITHUB_TOKEN", crate::github::TOKEN_ENV];
const PI_PATH_FILE: &str = "pi-path";
const GITHUB_MARKER: &str = "github-credential";

#[derive(Debug, Serialize)]
pub struct Status {
    pub github: GitHubStatus,
    pub pi: PiStatus,
}

#[derive(Debug, Serialize)]
pub struct GitHubStatus {
    pub available: bool,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct PiStatus {
    pub available: bool,
    pub source: String,
    pub path: Option<String>,
}

fn config_dir() -> Option<PathBuf> {
    Some(BaseDirs::new()?.config_dir().join("Starchive"))
}

fn config_for_write() -> Result<PathBuf> {
    let dir = config_dir().context("Could not locate the Starfolio settings directory")?;
    fs::create_dir_all(&dir).context("Could not create the Starfolio settings directory")?;
    Ok(dir)
}

pub fn status() -> Result<Status> {
    status_for(config_dir().as_deref(), &SearchPaths::new(), |name| {
        env::var_os(name)
    })
}

fn status_for(
    config: Option<&Path>,
    search: &SearchPaths,
    get: impl Fn(&str) -> Option<OsString>,
) -> Result<Status> {
    // Settings can report the configured credential without unlocking the native store.
    let (github_source, github_available) = if github_is_configured(config)? {
        ("saved", true)
    } else if let Some(token) = environment_github(&get) {
        ("environment", token.is_ok())
    } else if gh_token(search).is_some() {
        ("gh", true)
    } else {
        ("missing", false)
    };
    let (pi_source, pi_path) = resolve_pi(config, get("STARCHIVE_PI_PATH"), search);
    Ok(Status {
        github: GitHubStatus {
            available: github_available,
            source: github_source.into(),
        },
        pi: PiStatus {
            available: pi_path.is_some(),
            source: pi_source.into(),
            path: pi_path.map(|path| path.to_string_lossy().into_owned()),
        },
    })
}

pub fn github_token() -> Result<Option<String>> {
    let saved = if github_is_configured(config_dir().as_deref())? {
        Some(read_keyring()?)
    } else {
        None
    };
    resolve_github(
        saved,
        |name| env::var_os(name),
        || gh_token(&SearchPaths::new()),
    )
}

pub fn save_github(token: Option<&str>) -> Result<()> {
    let token = token.map(validate_token).transpose()?;
    let marker = if token.is_some() {
        config_for_write()?.join(GITHUB_MARKER)
    } else if let Some(dir) = config_dir() {
        dir.join(GITHUB_MARKER)
    } else {
        return Ok(());
    };

    update_saved_github(&marker, token, write_keyring)
}

fn update_saved_github(
    marker: &Path,
    token: Option<&str>,
    update_keyring: impl FnOnce(Option<&str>) -> Result<()>,
) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(token.is_some());
    let file = match options.open(marker) {
        Ok(file) => file,
        Err(error) if token.is_none() && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => bail!("Could not open the saved GitHub credential indicator"),
    };
    file.lock()
        .context("Could not lock the saved GitHub credential indicator")?;
    let old_size = file
        .metadata()
        .context("Could not read the saved GitHub credential indicator")?
        .len();
    if let Some(token) = token {
        // Establish the indicator before writing the token so it remains reachable on restart.
        (&file)
            .write_all(b"1")
            .context("Could not record the saved GitHub credential")?;
        if let Err(error) = update_keyring(Some(token)) {
            file.set_len(old_size)
                .context("Could not restore the saved GitHub credential indicator")?;
            return Err(error);
        }
    } else if old_size != 0 {
        update_keyring(None)?;
        file.set_len(0)
            .context("Could not clear the saved GitHub credential indicator")?;
    }
    Ok(())
}

fn github_is_configured(dir: Option<&Path>) -> Result<bool> {
    let Some(dir) = dir else {
        return Ok(false);
    };
    match dir.join(GITHUB_MARKER).metadata() {
        Ok(metadata) => Ok(metadata.len() != 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("Could not check for a saved GitHub credential"),
    }
}

fn validate_token(token: &str) -> Result<&str> {
    let token = token.trim();
    if token.is_empty() {
        bail!("The GitHub credential is empty");
    }
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!("The GitHub credential contains invalid characters");
    }
    Ok(token)
}

fn environment_github(get: impl Fn(&str) -> Option<OsString>) -> Option<Result<String>> {
    TOKEN_VARS.into_iter().find_map(|name| {
        let token = match get(name)?.into_string() {
            Ok(token) if token.trim().is_empty() => return None,
            Ok(token) => token,
            Err(_) => {
                return Some(Err(anyhow!(
                    "The GitHub credential contains invalid characters"
                )))
            }
        };
        Some(validate_token(&token).map(str::to_owned))
    })
}

fn resolve_github(
    saved: Option<String>,
    get: impl Fn(&str) -> Option<OsString>,
    gh: impl FnOnce() -> Option<String>,
) -> Result<Option<String>> {
    if saved.is_some() {
        return Ok(saved);
    }
    if let Some(token) = environment_github(get) {
        // An invalid credential must not trigger a sync under a different GitHub account.
        return token.map(Some);
    }
    Ok(gh())
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn keyring_entry() -> Result<keyring_core::Entry> {
    use keyring_core::api::CredentialStoreApi;

    KeyringStore::new()
        .and_then(|store| store.build("Starchive", "github.com", None))
        .map_err(|_| anyhow!("Could not access the operating system credential store"))
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn read_keyring() -> Result<String> {
    match keyring_entry()?.get_password() {
        Ok(token) => Ok(validate_token(&token)?.to_owned()),
        Err(keyring_core::Error::NoEntry) => bail!("The saved GitHub credential is missing from the operating system credential store. Clear or replace it in Settings"),
        Err(_) => bail!(
            "Could not read the saved GitHub credential from the operating system credential store"
        ),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn write_keyring(token: Option<&str>) -> Result<()> {
    let entry = keyring_entry()?;
    match token {
        Some(token) => entry.set_password(token),
        None => match entry.delete_credential() {
            Err(keyring_core::Error::NoEntry) => Ok(()),
            result => result,
        },
    }
    .map_err(|_| {
        anyhow!(
            "Could not update the saved GitHub credential in the operating system credential store"
        )
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn read_keyring() -> Result<String> {
    bail!("The operating system credential store is unavailable on this platform")
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn write_keyring(_token: Option<&str>) -> Result<()> {
    bail!("The operating system credential store is unavailable on this platform")
}

fn gh_token(search: &SearchPaths) -> Option<String> {
    let name = if cfg!(windows) { "gh.exe" } else { "gh" };
    let executable = search.find(name)?;
    let mut command = Command::new(&executable);
    command.env("PATH", search.child_path(&executable));
    read_gh_token(command, Duration::from_secs(2))
}

fn read_gh_token(mut command: Command, timeout: Duration) -> Option<String> {
    const MAX_OUTPUT: u64 = 4096;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .args(["auth", "token", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env_remove("GH_DEBUG")
        .env_remove("DEBUG")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env_remove(TOKEN_VARS[0])
        .env_remove(TOKEN_VARS[1])
        .env_remove(TOKEN_VARS[2])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let (send, receive) = mpsc::sync_channel(1);
    let reader = child.stdout.take().and_then(|stdout| {
        thread::Builder::new()
            .spawn(move || {
                let mut output = Vec::new();
                let result = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut output);
                if result.is_ok() && output.len() <= MAX_OUTPUT as usize {
                    let _ = send.send(output);
                }
            })
            .ok()
    });

    if reader.is_some() {
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let output = receive
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .ok()?;
                    return validate_token(std::str::from_utf8(&output).ok()?)
                        .ok()
                        .map(str::to_owned);
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => break,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

pub fn pi_path() -> Option<PathBuf> {
    resolve_pi(
        config_dir().as_deref(),
        env::var_os("STARCHIVE_PI_PATH"),
        &SearchPaths::new(),
    )
    .1
}

pub fn save_pi(path: Option<&str>) -> Result<()> {
    if let Some(path) = path {
        let dirs = BaseDirs::new();
        let path = validate_pi_path(Path::new(path), dirs.as_ref().map(BaseDirs::home_dir))?;
        write_pi_path(&config_for_write()?.join(PI_PATH_FILE), Some(&path))
    } else if let Some(dir) = config_dir() {
        write_pi_path(&dir.join(PI_PATH_FILE), None)
    } else {
        Ok(())
    }
}

fn write_pi_path(file: &Path, path: Option<&Path>) -> Result<()> {
    if let Some(path) = path {
        let value = path.to_str().context("The pi path must be valid text")?;
        let dir = file
            .parent()
            .context("Could not locate the pi settings directory")?;
        let mut temp =
            tempfile::NamedTempFile::new_in(dir).context("Could not create the pi setting")?;
        temp.write_all(value.as_bytes())
            .context("Could not write the pi setting")?;
        temp.as_file()
            .sync_all()
            .context("Could not flush the pi setting")?;
        temp.persist(file)
            .map_err(|_| anyhow!("Could not save the pi setting"))?;
    } else if let Err(error) = fs::remove_file(file) {
        if error.kind() != io::ErrorKind::NotFound {
            bail!("Could not clear the pi setting");
        }
    }
    Ok(())
}

fn validate_pi_path(path: &Path, home: Option<&Path>) -> Result<PathBuf> {
    resolve_path(path, home).context("Select an existing executable file for pi")
}

fn resolve_path(path: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let expanded;
    let path = if let Ok(suffix) = path.strip_prefix("~") {
        expanded = home?.join(suffix);
        &expanded
    } else {
        path
    };
    executable(&std::path::absolute(path).ok()?)
}

fn resolve_pi(
    config: Option<&Path>,
    environment: Option<OsString>,
    search: &SearchPaths,
) -> (&'static str, Option<PathBuf>) {
    let saved = config.and_then(|dir| fs::read_to_string(dir.join(PI_PATH_FILE)).ok());
    if let Some(path) = saved
        .as_deref()
        .and_then(|path| resolve_path(Path::new(path), search.home.as_deref()))
    {
        return ("saved", Some(path));
    }
    if let Some(path) = environment.as_deref().map(Path::new).and_then(|path| {
        if path.file_name() == Some(path.as_os_str()) {
            search.find(path)
        } else {
            resolve_path(path, search.home.as_deref())
        }
    }) {
        return ("environment", Some(path));
    }
    match search.find("pi") {
        Some(path) => ("auto", Some(path)),
        None => ("missing", None),
    }
}

#[cfg(not(windows))]
fn executable(path: &Path) -> Option<PathBuf> {
    crate::runtime::find_executable(path)
}

#[cfg(windows)]
fn executable(path: &Path) -> Option<PathBuf> {
    const EXTENSIONS: [&str; 4] = ["exe", "com", "cmd", "bat"];
    if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
        return EXTENSIONS
            .iter()
            .any(|ext| ext.eq_ignore_ascii_case(extension))
            .then(|| path.to_owned())
            .filter(|path| path.is_file());
    }
    EXTENSIONS
        .into_iter()
        .map(|ext| path.with_extension(ext))
        .find(|path| path.is_file())
}

pub fn child_path(executable: &Path) -> OsString {
    SearchPaths::new().child_path(executable)
}

struct SearchPaths {
    home: Option<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl SearchPaths {
    fn new() -> Self {
        let home = BaseDirs::new().map(|dirs| dirs.home_dir().to_owned());
        let mut dirs = env::var_os("PATH")
            .map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default();
        dirs.extend(known_bin_dirs(home.as_deref(), |name| env::var_os(name)));
        Self { home, dirs }
    }

    fn find(&self, name: impl AsRef<Path>) -> Option<PathBuf> {
        self.dirs
            .iter()
            .find_map(|dir| executable(&std::path::absolute(dir.join(&name)).ok()?))
    }

    fn child_path(&self, executable: &Path) -> OsString {
        let resolved = executable.canonicalize().ok();
        let mut seen = HashSet::new();
        let dirs = executable
            .parent()
            .into_iter()
            .chain(resolved.as_deref().and_then(Path::parent))
            .chain(self.dirs.iter().map(PathBuf::as_path))
            .filter_map(|dir| std::path::absolute(dir).ok())
            .filter(|dir| seen.insert(dir.clone()) && env::join_paths([dir]).is_ok());
        env::join_paths(dirs).unwrap_or_default()
    }
}

fn known_bin_dirs(home: Option<&Path>, get: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let get_path = |name| {
        get(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(home) = home {
        dirs.extend(
            [
                ".pi/agent/bin",
                ".local/bin",
                "bin",
                ".npm-global/bin",
                ".npm/bin",
                ".local/share/npm/bin",
                ".local/share/pnpm",
                "Library/pnpm",
                ".volta/bin",
                ".asdf/shims",
                ".local/share/mise/shims",
                "scoop/shims",
            ]
            .map(|dir| home.join(dir)),
        );
    }
    for (name, suffix) in [
        ("PI_CODING_AGENT_DIR", "bin"),
        ("HOMEBREW_PREFIX", "bin"),
        ("PNPM_HOME", ""),
        ("VOLTA_HOME", "bin"),
        ("NVM_SYMLINK", ""),
    ] {
        if let Some(path) = get_path(name) {
            dirs.push(path.join(suffix));
        }
    }
    if let Some(prefix) = get_path("npm_config_prefix").or_else(|| get_path("NPM_CONFIG_PREFIX")) {
        dirs.push(if cfg!(windows) {
            prefix
        } else {
            prefix.join("bin")
        });
    }
    if let Some(nvm) = get_path("NVM_DIR").or_else(|| home.map(|home| home.join(".nvm"))) {
        dirs.extend(nvm_bin_dirs(&nvm.join("versions/node"), "bin"));
    }
    #[cfg(windows)]
    {
        if let Some(nvm) = get_path("NVM_HOME") {
            dirs.extend(nvm_bin_dirs(&nvm, ""));
        }
        for (name, suffix) in [
            ("APPDATA", "npm"),
            ("LOCALAPPDATA", "Programs/nodejs"),
            ("LOCALAPPDATA", "Programs/GitHub CLI"),
            ("LOCALAPPDATA", "Microsoft/WinGet/Links"),
            ("ProgramFiles", "nodejs"),
            ("ProgramFiles", "GitHub CLI"),
            ("ProgramFiles(x86)", "nodejs"),
            ("ProgramFiles(x86)", "GitHub CLI"),
            ("SCOOP", "shims"),
        ] {
            if let Some(path) = get_path(name) {
                dirs.push(path.join(suffix));
            }
        }
    }
    #[cfg(unix)]
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/home/linuxbrew/.linuxbrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
        ]
        .map(PathBuf::from),
    );
    dirs
}

fn nvm_bin_dirs(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut versions: Vec<_> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let mut parts = name.to_str()?.strip_prefix('v')?.split('.');
            let version = [
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
            ];
            (parts.next().is_none()).then(|| (version, entry.path().join(suffix)))
        })
        .collect();
    versions.sort_unstable_by_key(|(version, _)| std::cmp::Reverse(*version));
    versions.into_iter().map(|(_, dir)| dir).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEST_TOKEN: &str = "synthetic-settings-token";

    #[test]
    fn github_precedence_prefers_saved_then_standard_and_legacy_env_then_gh() -> Result<()> {
        assert_eq!(
            resolve_github(
                Some(TEST_TOKEN.into()),
                |_| panic!("unexpected environment lookup"),
                || panic!("unexpected gh lookup")
            )?,
            Some(TEST_TOKEN.into())
        );
        for (index, preferred) in TOKEN_VARS.iter().enumerate() {
            let environment = |name: &str| {
                if TOKEN_VARS[index..].contains(&name) {
                    Some(name.into())
                } else {
                    Some(" ".into())
                }
            };
            assert_eq!(
                resolve_github(None, environment, || panic!("unexpected gh lookup"))?,
                Some((*preferred).into())
            );
        }
        assert_eq!(
            resolve_github(None, |_| None, || Some(TEST_TOKEN.into()))?,
            Some(TEST_TOKEN.into())
        );
        Ok(())
    }

    #[test]
    fn status_and_errors_contain_no_credential() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let search = SearchPaths {
            home: None,
            dirs: Vec::new(),
        };
        let missing = status_for(Some(temp.path()), &search, |_| None)?;
        assert_eq!(
            serde_json::to_value(missing)?,
            json!({
                "github": {"available": false, "source": "missing"},
                "pi": {"available": false, "source": "missing", "path": null},
            })
        );

        let environment = |name: &str| (name == "GH_TOKEN").then(|| TEST_TOKEN.into());
        let status = status_for(Some(temp.path()), &search, environment)?;
        assert_eq!(
            serde_json::to_value(status)?.get("github"),
            Some(&json!({
                "available": true, "source": "environment"
            }))
        );
        let invalid = format!("{TEST_TOKEN}\ninvalid");
        let environment = |name: &str| (name == "GH_TOKEN").then(|| invalid.clone().into());
        let error =
            resolve_github(None, environment, || panic!("unexpected gh lookup")).unwrap_err();
        assert!(error.to_string().contains("invalid characters"));
        assert!(!format!("{error:?}").contains(TEST_TOKEN));
        let status = status_for(Some(temp.path()), &search, environment)?;
        assert_eq!(status.github.source, "environment");
        assert!(!status.github.available);
        Ok(())
    }

    #[test]
    fn github_marker_uses_no_secret_and_rolls_back_a_failed_save() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let marker = temp.path().join(GITHUB_MARKER);
        let search = SearchPaths {
            home: None,
            dirs: Vec::new(),
        };
        let result =
            update_saved_github(&marker, Some(TEST_TOKEN), |_| bail!("keyring unavailable"));
        assert!(result.is_err());
        assert!(!github_is_configured(Some(temp.path()))?);
        update_saved_github(&marker, Some(TEST_TOKEN), |token| {
            assert_eq!(token, Some(TEST_TOKEN));
            Ok(())
        })?;
        assert_eq!(fs::read(&marker)?, b"1");
        let status = status_for(Some(temp.path()), &search, |_| {
            Some("environment-token".into())
        })?;
        assert_eq!(status.github.source, "saved");
        assert!(status.github.available);
        update_saved_github(&marker, None, |token| {
            assert!(token.is_none());
            Ok(())
        })?;
        assert!(!github_is_configured(Some(temp.path()))?);
        Ok(())
    }

    fn fake_executable(directory: &Path, name: &str) -> Result<PathBuf> {
        fs::create_dir_all(directory)?;
        let path = directory.join(name);
        let path = if cfg!(windows) {
            path.with_extension("cmd")
        } else {
            path
        };
        fs::write(&path, "test executable")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(std::path::absolute(path)?)
    }

    #[test]
    fn pi_precedence_and_atomic_replacement_with_validation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = temp.path();
        let first = fake_executable(&config.join("first"), "custom-pi")?;
        let second = fake_executable(&config.join("second"), "pi-alt")?;
        let automatic = fake_executable(&config.join("auto"), "pi")?;
        let search = SearchPaths {
            home: Some(config.into()),
            dirs: vec![config.join("auto")],
        };
        let marker = config.join(PI_PATH_FILE);
        let environment = || Some(second.as_os_str().to_owned());

        assert_eq!(
            resolve_pi(Some(config), environment(), &search),
            ("environment", Some(second.clone()))
        );
        let validated = validate_pi_path(Path::new("~/first/custom-pi"), Some(config))?;
        assert_eq!(validated, first);
        write_pi_path(&marker, Some(&validated))?;
        assert_eq!(
            resolve_pi(Some(config), environment(), &search),
            ("saved", Some(first))
        );
        assert!(validate_pi_path(config, Some(config)).is_err());
        assert!(validate_pi_path(&config.join("missing"), Some(config)).is_err());
        write_pi_path(&marker, Some(&second))?;
        assert_eq!(
            resolve_pi(Some(config), None, &search),
            ("saved", Some(second.clone()))
        );
        fs::remove_file(&second)?;
        assert_eq!(
            resolve_pi(Some(config), environment(), &search),
            ("auto", Some(automatic))
        );
        write_pi_path(&marker, None)?;
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn finds_pi_and_node_without_shell_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let home = temp.path();
        let pi = fake_executable(&home.join(".pi/agent/bin"), "pi")?;
        let older = fake_executable(&home.join(".nvm/versions/node/v9.0.0/bin"), "node")?;
        let newer = fake_executable(&home.join(".nvm/versions/node/v22.2.0/bin"), "node")?;
        let search = SearchPaths {
            home: Some(home.into()),
            dirs: known_bin_dirs(Some(home), |_| None),
        };
        assert_eq!(search.find("pi").as_ref(), Some(&pi));
        assert_eq!(search.find("node").as_ref(), Some(&newer));
        let path = search.child_path(&pi);
        let dirs: Vec<_> = env::split_paths(&path).collect();
        assert_eq!(dirs.first().map(PathBuf::as_path), pi.parent());
        let newer_index = dirs
            .iter()
            .position(|dir| Some(dir.as_path()) == newer.parent())
            .unwrap();
        let older_index = dirs
            .iter()
            .position(|dir| Some(dir.as_path()) == older.parent())
            .unwrap();
        assert!(newer_index < older_index);
        let first = fake_executable(&home.join("inherited-path"), "pi")?;
        let mut search = search;
        search.dirs.insert(0, home.join("inherited-path"));
        assert_eq!(search.find("pi"), Some(first));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn gh_probe_is_noninteractive_bounded_and_reads_only_successful_stdout() {
        fn shell(script: &str) -> Command {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script, "test-gh"]);
            command
        }
        let mut gh = shell(
            r#"
            [ "$*" = 'auth token --hostname github.com' ] || exit 1
            [ "$GH_PROMPT_DISABLED" = 1 ] || exit 2
            [ -z "${GH_TOKEN+x}${GITHUB_TOKEN+x}${STARCHIVE_GITHUB_TOKEN+x}" ] || exit 3
            if read -r ignored; then exit 4; fi
            printf 'synthetic-settings-token\n'
        "#,
        );
        for name in TOKEN_VARS {
            gh.env(name, TEST_TOKEN);
        }
        let timeout = Duration::from_secs(2);
        assert_eq!(read_gh_token(gh, timeout).as_deref(), Some(TEST_TOKEN));
        assert!(read_gh_token(shell("printf synthetic-settings-token; exit 1"), timeout).is_none());
        let start = Instant::now();
        assert!(read_gh_token(shell("exec /bin/sleep 3"), Duration::from_millis(50)).is_none());
        assert!(start.elapsed() < timeout);
    }
}
