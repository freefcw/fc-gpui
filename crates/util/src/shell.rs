#[cfg(any(windows, test))]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::LazyLock;
use std::{borrow::Cow, fmt, path::Path};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellKind {
    #[default]
    Posix,
    Csh,
    Tcsh,
    Rc,
    Fish,
    PowerShell,
    Nushell,
    Cmd,
    Xonsh,
}

pub fn get_system_shell() -> String {
    if cfg!(windows) {
        get_windows_system_shell()
    } else {
        std::env::var("SHELL").unwrap_or("/bin/sh".to_string())
    }
}

pub fn get_default_system_shell() -> String {
    if cfg!(windows) {
        get_windows_system_shell()
    } else {
        "/bin/sh".to_string()
    }
}

/// Get the default system shell, preferring git-bash on Windows.
pub fn get_default_system_shell_preferring_bash() -> String {
    #[cfg(windows)]
    {
        get_windows_git_bash().unwrap_or_else(|| get_windows_system_shell())
    }

    #[cfg(not(windows))]
    {
        "/bin/sh".to_string()
    }
}

/// True Git for Windows install root: `git-bash.exe` plus `bin\bash.exe`.
#[cfg(any(windows, test))]
fn find_bash_in_installation(install_root: &Path) -> Option<PathBuf> {
    if !install_root.join("git-bash.exe").is_file() {
        return None;
    }
    let bash = install_root.join("bin").join("bash.exe");
    bash.is_file().then_some(bash)
}

/// Resolve Git Bash from a `git` executable path.
///
/// Git for Windows ships `git` under `cmd\` or, when already inside Git Bash,
/// `mingw64\bin\` (prepended to `PATH`). Walk one extra parent so the latter
/// still finds the install root.
#[cfg(any(windows, test))]
fn find_bash_from_git_binary(git: &Path) -> Option<PathBuf> {
    let binary_directory = git.parent()?;
    let parent = binary_directory.parent()?;
    find_bash_in_installation(parent).or_else(|| find_bash_in_installation(parent.parent()?))
}

#[cfg(any(windows, test))]
fn find_bash_using_git_install(
    git_install_root: Option<PathBuf>,
    git_binary: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(bash) = git_install_root.and_then(|path| find_bash_in_installation(&path)) {
        return Some(bash);
    }
    find_bash_from_git_binary(git_binary?)
}

#[cfg(windows)]
pub fn get_windows_git_bash() -> Option<String> {
    fn find_bash_in_git() -> Option<PathBuf> {
        let git = which::which("git").ok();
        find_bash_using_git_install(
            std::env::var_os("GIT_INSTALL_ROOT").map(PathBuf::from),
            git.as_deref(),
        )
    }

    static GIT_BASH: LazyLock<Option<String>> = LazyLock::new(|| {
        let bash = find_bash_in_git().map(|p| p.to_string_lossy().into_owned());
        if let Some(ref path) = bash {
            log::info!("Found bash at {}", path);
        }
        bash
    });

    (*GIT_BASH).clone()
}

#[cfg(windows)]
pub fn get_powershell() -> Option<String> {
    fn find_pwsh_in_programfiles(find_alternate: bool, find_preview: bool) -> Option<PathBuf> {
        #[cfg(target_pointer_width = "64")]
        let env_var = if find_alternate {
            "ProgramFiles(x86)"
        } else {
            "ProgramFiles"
        };

        #[cfg(target_pointer_width = "32")]
        let env_var = if find_alternate {
            "ProgramW6432"
        } else {
            "ProgramFiles"
        };

        let install_base_dir = PathBuf::from(std::env::var_os(env_var)?).join("PowerShell");
        install_base_dir
            .read_dir()
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| matches!(entry.file_type(), Ok(ft) if ft.is_dir()))
            .filter_map(|entry| {
                let dir_name = entry.file_name();
                let dir_name = dir_name.to_string_lossy();

                let version = if find_preview {
                    let dash_index = dir_name.find('-')?;
                    if &dir_name[dash_index + 1..] != "preview" {
                        return None;
                    };
                    dir_name[..dash_index].parse::<u32>().ok()?
                } else {
                    dir_name.parse::<u32>().ok()?
                };

                let exe_path = entry.path().join("pwsh.exe");
                if exe_path.is_file() {
                    Some((version, exe_path))
                } else {
                    None
                }
            })
            .max_by_key(|(version, _)| *version)
            .map(|(_, path)| path)
    }

    fn find_pwsh_in_msix(find_preview: bool) -> Option<PathBuf> {
        let msix_app_dir =
            PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Microsoft\\WindowsApps");
        let package_family_name = if find_preview {
            "Microsoft.PowerShellPreview_8wekyb3d8bbwe"
        } else {
            "Microsoft.PowerShell_8wekyb3d8bbwe"
        };
        let pwsh_exe = msix_app_dir.join(package_family_name).join("pwsh.exe");
        pwsh_exe.exists().then_some(pwsh_exe)
    }

    fn find_pwsh_in_scoop() -> Option<PathBuf> {
        let pwsh_exe =
            PathBuf::from(std::env::var_os("USERPROFILE")?).join("scoop\\shims\\pwsh.exe");
        pwsh_exe.is_file().then_some(pwsh_exe)
    }

    fn find_pwsh_in_dotnet_tools() -> Option<PathBuf> {
        let pwsh_exe =
            PathBuf::from(std::env::var_os("USERPROFILE")?).join(".dotnet\\tools\\pwsh.exe");
        pwsh_exe.is_file().then_some(pwsh_exe)
    }

    fn find_windows_powershell() -> Option<PathBuf> {
        let system_root = PathBuf::from(std::env::var_os("SystemRoot")?);
        let powershell = system_root.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        powershell.is_file().then_some(powershell)
    }

    static POWERSHELL: LazyLock<Option<String>> = LazyLock::new(|| {
        let locations = [
            || find_pwsh_in_programfiles(false, false),
            || find_pwsh_in_programfiles(true, false),
            || find_pwsh_in_msix(false),
            || find_pwsh_in_programfiles(false, true),
            || find_pwsh_in_msix(true),
            || find_pwsh_in_programfiles(true, true),
            || find_pwsh_in_scoop(),
            || find_pwsh_in_dotnet_tools(),
            || which::which_global("pwsh.exe").ok(),
            || which::which_global("powershell.exe").ok(),
            || find_windows_powershell(),
        ];

        locations
            .into_iter()
            .find_map(|f| f())
            .map(|p| p.to_string_lossy().trim().to_owned())
            .inspect(|shell| log::info!("Found powershell in: {}", shell))
    });

    (*POWERSHELL).clone()
}

#[cfg(windows)]
pub fn get_windows_system_shell() -> String {
    static CMD: LazyLock<String> = LazyLock::new(|| {
        log::warn!("Powershell not found, falling back to `cmd`");
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(system_root)
            .join("System32\\cmd.exe")
            .to_string_lossy()
            .into_owned()
    });
    get_powershell().unwrap_or_else(|| (*CMD).clone())
}

#[cfg(not(windows))]
pub fn get_windows_system_shell() -> String {
    "cmd.exe".to_string()
}

impl fmt::Display for ShellKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellKind::Posix => write!(f, "sh"),
            ShellKind::Csh => write!(f, "csh"),
            ShellKind::Tcsh => write!(f, "tcsh"),
            ShellKind::Fish => write!(f, "fish"),
            ShellKind::PowerShell => write!(f, "powershell"),
            ShellKind::Nushell => write!(f, "nu"),
            ShellKind::Cmd => write!(f, "cmd"),
            ShellKind::Rc => write!(f, "rc"),
            ShellKind::Xonsh => write!(f, "xonsh"),
        }
    }
}

impl ShellKind {
    pub fn system() -> Self {
        Self::new(&get_system_shell(), cfg!(windows))
    }

    pub fn new(program: impl AsRef<Path>, is_windows: bool) -> Self {
        let program = program.as_ref();
        let program = program
            .file_stem()
            .unwrap_or_else(|| program.as_os_str())
            .to_string_lossy();

        if program == "powershell" || program == "pwsh" {
            ShellKind::PowerShell
        } else if program == "cmd" {
            ShellKind::Cmd
        } else if program == "nu" {
            ShellKind::Nushell
        } else if program == "fish" {
            ShellKind::Fish
        } else if program == "csh" {
            ShellKind::Csh
        } else if program == "tcsh" {
            ShellKind::Tcsh
        } else if program == "rc" {
            ShellKind::Rc
        } else if program == "xonsh" {
            ShellKind::Xonsh
        } else if program == "sh" || program == "bash" {
            ShellKind::Posix
        } else {
            if is_windows {
                ShellKind::PowerShell
            } else {
                // Some other shell detected, the user might install and use a
                // unix-like shell.
                ShellKind::Posix
            }
        }
    }

    pub fn to_shell_variable(self, input: &str) -> String {
        match self {
            Self::PowerShell => Self::to_powershell_variable(input),
            Self::Cmd => Self::to_cmd_variable(input),
            Self::Posix => input.to_owned(),
            Self::Fish => input.to_owned(),
            Self::Csh => input.to_owned(),
            Self::Tcsh => input.to_owned(),
            Self::Rc => input.to_owned(),
            Self::Nushell => Self::to_nushell_variable(input),
            Self::Xonsh => input.to_owned(),
        }
    }

    fn to_cmd_variable(input: &str) -> String {
        if let Some(var_str) = input.strip_prefix("${") {
            match var_str.strip_suffix('}') {
                Some(var_name) if !var_name.is_empty() && !var_name.contains(':') => {
                    format!("%{var_name}%")
                }
                // `${SOME_VAR:-SOME_DEFAULT}`, we currently do not handle this situation,
                // which will result in the task failing to run in such cases.
                _ => input.into(),
            }
        } else if let Some(var_str) = input.strip_prefix('$') {
            // If the input starts with "$", directly append to "$env:"
            format!("%{}%", var_str)
        } else {
            // If no prefix is found, return the input as is
            input.into()
        }
    }

    fn to_powershell_variable(input: &str) -> String {
        if let Some(var_str) = input.strip_prefix("${") {
            match var_str.strip_suffix('}') {
                Some(var_name) if !var_name.is_empty() && !var_name.contains(':') => {
                    format!("$env:{var_name}")
                }
                // `${SOME_VAR:-SOME_DEFAULT}`, we currently do not handle this situation,
                // which will result in the task failing to run in such cases.
                _ => input.into(),
            }
        } else if let Some(var_str) = input.strip_prefix('$') {
            // If the input starts with "$", directly append to "$env:"
            format!("$env:{}", var_str)
        } else {
            // If no prefix is found, return the input as is
            input.into()
        }
    }

    fn to_nushell_variable(input: &str) -> String {
        let mut result = String::new();
        let mut source = input;
        let mut is_start = true;

        loop {
            match source.chars().next() {
                None => return result,
                Some('$') => {
                    source = Self::parse_nushell_var(&source[1..], &mut result, is_start);
                    is_start = false;
                }
                Some(_) => {
                    is_start = false;
                    let chunk_end = source.find('$').unwrap_or(source.len());
                    let (chunk, rest) = source.split_at(chunk_end);
                    result.push_str(chunk);
                    source = rest;
                }
            }
        }
    }

    fn parse_nushell_var<'a>(source: &'a str, text: &mut String, is_start: bool) -> &'a str {
        if source.starts_with("env.") {
            text.push('$');
            return source;
        }

        match source.chars().next() {
            Some('{') => {
                let source = &source[1..];
                if let Some(end) = source.find('}') {
                    let var_name = &source[..end];
                    if !var_name.is_empty() {
                        if !is_start {
                            text.push_str("(");
                        }
                        text.push_str("$env.");
                        text.push_str(var_name);
                        if !is_start {
                            text.push_str(")");
                        }
                        &source[end + 1..]
                    } else {
                        text.push_str("${}");
                        &source[end + 1..]
                    }
                } else {
                    text.push_str("${");
                    source
                }
            }
            Some(c) if c.is_alphabetic() || c == '_' => {
                let end = source
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(source.len());
                let var_name = &source[..end];
                if !is_start {
                    text.push_str("(");
                }
                text.push_str("$env.");
                text.push_str(var_name);
                if !is_start {
                    text.push_str(")");
                }
                &source[end..]
            }
            _ => {
                text.push('$');
                source
            }
        }
    }

    pub fn args_for_shell(&self, interactive: bool, combined_command: String) -> Vec<String> {
        match self {
            ShellKind::PowerShell => vec!["-C".to_owned(), combined_command],
            ShellKind::Cmd => vec!["/C".to_owned(), combined_command],
            ShellKind::Posix
            | ShellKind::Nushell
            | ShellKind::Fish
            | ShellKind::Csh
            | ShellKind::Tcsh
            | ShellKind::Rc
            | ShellKind::Xonsh => interactive
                .then(|| "-i".to_owned())
                .into_iter()
                .chain(["-c".to_owned(), combined_command])
                .collect(),
        }
    }

    pub const fn command_prefix(&self) -> Option<char> {
        match self {
            ShellKind::PowerShell => Some('&'),
            ShellKind::Nushell => Some('^'),
            _ => None,
        }
    }

    pub const fn sequential_commands_separator(&self) -> char {
        match self {
            ShellKind::Cmd => '&',
            _ => ';',
        }
    }

    pub fn try_quote<'a>(&self, arg: &'a str) -> Option<Cow<'a, str>> {
        shlex::try_quote(arg).ok().map(|arg| match self {
            // If we are running in PowerShell, we want to take extra care when escaping strings.
            // In particular, we want to escape strings with a backtick (`) rather than a backslash (\).
            // TODO double escaping backslashes is not necessary in PowerShell and probably CMD
            ShellKind::PowerShell => Cow::Owned(arg.replace("\\\"", "`\"")),
            _ => arg,
        })
    }

    pub const fn activate_keyword(&self) -> &'static str {
        match self {
            ShellKind::Cmd => "",
            ShellKind::Nushell => "overlay use",
            ShellKind::PowerShell => ".",
            ShellKind::Fish => "source",
            ShellKind::Csh => "source",
            ShellKind::Tcsh => "source",
            ShellKind::Posix | ShellKind::Rc => "source",
            ShellKind::Xonsh => "source",
        }
    }

    pub const fn clear_screen_command(&self) -> &'static str {
        match self {
            ShellKind::Cmd => "cls",
            _ => "clear",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_for_windows_layout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(dir.path().join("git-bash.exe"), []).unwrap();
        std::fs::write(bin.join("bash.exe"), []).unwrap();
        dir
    }

    #[test]
    fn test_to_shell_variable() {
        assert_eq!(
            ShellKind::PowerShell.to_shell_variable("${FOO}"),
            "$env:FOO"
        );
        assert_eq!(ShellKind::Cmd.to_shell_variable("${FOO}"), "%FOO%");
        assert_eq!(ShellKind::Nushell.to_shell_variable("${FOO}"), "$env.FOO");
        assert_eq!(ShellKind::Posix.to_shell_variable("${FOO}"), "${FOO}");

        assert_eq!(ShellKind::PowerShell.to_shell_variable("$FOO"), "$env:FOO");
        assert_eq!(
            ShellKind::PowerShell.to_shell_variable("${日本}"),
            "$env:日本"
        );
        assert_eq!(
            ShellKind::PowerShell.to_shell_variable("${FOO:-bar}"),
            "${FOO:-bar}"
        );
    }

    #[test]
    fn test_to_shell_variable_malformed_is_passed_through() {
        for input in ["${", "${FOO", "${café", "${}", "${日本"] {
            for shell_kind in [ShellKind::PowerShell, ShellKind::Cmd, ShellKind::Nushell] {
                assert_eq!(shell_kind.to_shell_variable(input), input);
            }
        }
    }

    #[test]
    fn git_bash_requires_git_bash_exe_and_bin_bash() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("bash.exe"), []).unwrap();
        assert!(find_bash_in_installation(dir.path()).is_none());

        std::fs::write(dir.path().join("git-bash.exe"), []).unwrap();
        assert_eq!(
            find_bash_in_installation(dir.path()).as_deref(),
            Some(bin.join("bash.exe").as_path())
        );
    }

    #[test]
    fn git_bash_from_cmd_git_uses_install_root() {
        let install = git_for_windows_layout();
        let cmd = install.path().join("cmd");
        std::fs::create_dir_all(&cmd).unwrap();
        let git = cmd.join("git.exe");
        std::fs::write(&git, []).unwrap();
        assert_eq!(
            find_bash_from_git_binary(&git).unwrap(),
            install.path().join("bin").join("bash.exe")
        );
    }

    #[test]
    fn git_bash_from_mingw64_bin_walks_up_to_install_root() {
        let install = git_for_windows_layout();
        let mingw_bin = install.path().join("mingw64").join("bin");
        std::fs::create_dir_all(&mingw_bin).unwrap();
        let git = mingw_bin.join("git.exe");
        std::fs::write(&git, []).unwrap();
        assert_eq!(
            find_bash_from_git_binary(&git).unwrap(),
            install.path().join("bin").join("bash.exe")
        );
    }

    #[test]
    fn git_install_root_takes_precedence_over_git_binary() {
        let preferred = git_for_windows_layout();
        let other = git_for_windows_layout();
        let git = other.path().join("cmd").join("git.exe");
        std::fs::create_dir_all(git.parent().unwrap()).unwrap();
        std::fs::write(&git, []).unwrap();
        assert_eq!(
            find_bash_using_git_install(Some(preferred.path().to_path_buf()), Some(&git)).unwrap(),
            preferred.path().join("bin").join("bash.exe")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_stubs_for_windows_shell_discovery() {
        assert_eq!(get_windows_system_shell(), "cmd.exe");
        assert_eq!(get_default_system_shell_preferring_bash(), "/bin/sh");
        assert_eq!(get_default_system_shell(), "/bin/sh");
    }
}
