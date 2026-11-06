//! "Open SSH" launcher: runs the user's terminal emulator with an interactive `ssh` session.
//! Uses the plain alias so every OpenSSH setting applies; never embeds credentials.
use crate::config::Server;
use anyhow::{Result, bail};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn known_terminal_args(bin: &str) -> Option<&'static [&'static str]> {
    let name = std::path::Path::new(bin)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(bin);
    match name {
        "xdg-terminal-exec" => Some(&["--"]),
        "x-terminal-emulator" => Some(&["-e"]),
        "kitty" => Some(&[]),
        "alacritty" => Some(&["-e"]),
        "foot" => Some(&[]),
        "wezterm" => Some(&["start", "--"]),
        "gnome-terminal" => Some(&["--"]),
        "kgx" => Some(&["--"]),
        "konsole" => Some(&["-e"]),
        "xfce4-terminal" => Some(&["-x"]),
        "tilix" => Some(&["-e"]),
        "terminator" => Some(&["-x"]),
        "lxterminal" => Some(&["-e"]),
        "mate-terminal" => Some(&["-x"]),
        "ptyxis" => Some(&["--"]),
        "xterm" => Some(&["-e"]),
        _ => None,
    }
}

pub fn find_in_path(bin: &str) -> Option<std::path::PathBuf> {
    if bin.contains('/') {
        let p = std::path::PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| std::path::Path::new(d).join(bin))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

fn parse_command_line(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quote: Option<char> = None;
    let mut has_token = false;

    while let Some(c) = chars.next() {
        match in_quote {
            Some(quote) => {
                if c == quote {
                    in_quote = None;
                } else if c == '\\' && quote == '"' {
                    if let Some(&next) = chars.peek() {
                        if next == '"' || next == '\\' || next == '$' || next == '`' || next == '\n'
                        {
                            chars.next();
                            current.push(next);
                        } else {
                            current.push('\\');
                        }
                    } else {
                        bail!("trailing backslash in terminal command: {input:?}");
                    }
                } else {
                    current.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    in_quote = Some(c);
                    has_token = true;
                } else if c == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                        has_token = true;
                    } else {
                        bail!("trailing backslash in terminal command: {input:?}");
                    }
                } else if c.is_whitespace() {
                    if has_token {
                        args.push(current);
                        current = String::new();
                        has_token = false;
                    }
                } else {
                    current.push(c);
                    has_token = true;
                }
            }
        }
    }

    if in_quote.is_some() {
        bail!("unclosed quote in terminal command: {input:?}");
    }
    if has_token {
        args.push(current);
    }
    Ok(args)
}

fn dequote_gvariant_str(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('\'') || !trimmed.ends_with('\'') || trimmed.len() < 2 {
        return None;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\'') => out.push('\''),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn is_gnome_or_unity_desktop() -> bool {
    let Ok(de) = std::env::var("XDG_CURRENT_DESKTOP") else {
        return false;
    };
    de.split(':').any(|tok| {
        let t = tok.trim();
        t.eq_ignore_ascii_case("gnome") || t.eq_ignore_ascii_case("unity")
    })
}

fn detect_gnome_default_terminal() -> Option<(String, Option<String>)> {
    if !is_gnome_or_unity_desktop() || find_in_path("gsettings").is_none() {
        return None;
    }
    let output = Command::new("gsettings")
        .args([
            "get",
            "org.gnome.desktop.default-applications.terminal",
            "exec",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let exec_raw = String::from_utf8_lossy(&output.stdout);
    let exec = dequote_gvariant_str(&exec_raw)?;
    if exec.is_empty() || exec == "@ms nothing" {
        return None;
    }

    let arg_output = Command::new("gsettings")
        .args([
            "get",
            "org.gnome.desktop.default-applications.terminal",
            "exec-arg",
        ])
        .output()
        .ok();
    let exec_arg = arg_output.and_then(|o| {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            dequote_gvariant_str(&s).filter(|a| !a.is_empty() && a != "@ms nothing")
        } else {
            None
        }
    });

    Some((exec, exec_arg))
}

fn resolve_terminal(configured: &str, have: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        let mut parts = parse_command_line(trimmed)?;
        if parts.is_empty() || parts[0].trim().is_empty() {
            bail!("invalid terminal command: empty binary name");
        }
        let bin = &parts[0];
        let bin_name = std::path::Path::new(bin)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(bin);
        if parts.len() == 1 {
            if let Some(args) = known_terminal_args(bin_name) {
                parts.extend(args.iter().map(|s| s.to_string()));
            }
        } else if bin_name == "xdg-terminal-exec" && parts.last().map(String::as_str) != Some("--")
        {
            parts.push("--".to_string());
        }
        return Ok(parts);
    }

    // 1. Preferred automatic resolver: xdg-terminal-exec
    if have("xdg-terminal-exec") {
        return Ok(vec!["xdg-terminal-exec".to_string(), "--".to_string()]);
    }

    // 2. Native GNOME / Unity desktop preference
    let mut gnome_missing_dispatcher = false;
    if let Some((exec, arg)) = detect_gnome_default_terminal() {
        if have(&exec) {
            let mut cmd = vec![exec.clone()];
            if let Some(a) = arg {
                cmd.push(a);
            } else if let Some(args) = known_terminal_args(&exec) {
                cmd.extend(args.iter().map(|s| s.to_string()));
            }
            return Ok(cmd);
        } else if exec == "xdg-terminal-exec" {
            gnome_missing_dispatcher = true;
        }
    }

    // 3. Explicit user TERMINAL environment variable
    if let Ok(t) = std::env::var("TERMINAL") {
        let trimmed_t = t.trim();
        if !trimmed_t.is_empty() {
            let mut parts = parse_command_line(trimmed_t)?;
            if !parts.is_empty() && !parts[0].trim().is_empty() && have(&parts[0]) {
                if parts.len() == 1 {
                    let bin_name = std::path::Path::new(&parts[0])
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or(&parts[0]);
                    if let Some(args) = known_terminal_args(bin_name) {
                        parts.extend(args.iter().map(|s| s.to_string()));
                    }
                }
                return Ok(parts);
            }
        }
    }

    // 4. Distribution fallback: Debian / Ubuntu alternatives
    if have("x-terminal-emulator") {
        return Ok(vec!["x-terminal-emulator".to_string(), "-e".to_string()]);
    }

    // 5. No resolver found
    if gnome_missing_dispatcher {
        bail!(
            "no terminal emulator found: desktop default dispatcher 'xdg-terminal-exec' is configured in GNOME settings but not found in PATH; install xdg-terminal-exec or configure general.terminal in SVMonitor config"
        );
    } else {
        bail!(
            "no terminal emulator found; install xdg-terminal-exec or configure general.terminal in SVMonitor config"
        );
    }
}

/// Terminal command prefix for `configured` (a user supplied command line, may contain args) or auto-detected.
pub fn terminal_prefix(configured: &str, have: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    resolve_terminal(configured, have).ok()
}

pub fn ssh_command(ssh_bin: &str, server: &Server) -> Vec<String> {
    let mut c = vec![ssh_bin.to_string()];
    c.extend(server.extra_args());
    c.push("--".into());
    c.push(server.destination());
    c
}

pub fn build(
    terminal: &str,
    ssh_bin: &str,
    server: &Server,
    have: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>> {
    let mut pre = resolve_terminal(terminal, have)?;
    pre.extend(ssh_command(ssh_bin, server));
    Ok(pre)
}

/// Spawn detached (own session, stdio closed). The child is reaped by a tiny waiter thread.
pub fn open_ssh(terminal: &str, ssh_bin: &str, server: &Server) -> Result<()> {
    let argv = build(terminal, ssh_bin, server, &|b| find_in_path(b).is_some())?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    unsafe {
        cmd.pre_exec(|| {
            let mut set = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting {}: {e}", argv[0]))?;
    std::thread::Builder::new()
        .name("svm-reap".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

pub fn open_path(path: &std::path::Path) -> Result<()> {
    let Some(opener) = find_in_path("xdg-open") else {
        bail!("xdg-open not found")
    };
    let mut cmd = Command::new(opener);
    cmd.arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            let mut set = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::Builder::new()
        .name("svm-reap".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn xdg_terminal_exec_argv_separator() {
        let _lock = ENV_LOCK.lock().unwrap();
        let have = |b: &str| b == "xdg-terminal-exec";
        assert_eq!(
            terminal_prefix("", &have).unwrap(),
            vec!["xdg-terminal-exec", "--"]
        );

        let have_all = |_: &str| true;
        assert_eq!(
            terminal_prefix("xdg-terminal-exec", &have_all).unwrap(),
            vec!["xdg-terminal-exec", "--"]
        );

        assert_eq!(
            terminal_prefix("xdg-terminal-exec --app-id=custom", &have_all).unwrap(),
            vec!["xdg-terminal-exec", "--app-id=custom", "--"]
        );

        assert_eq!(
            terminal_prefix("xdg-terminal-exec --app-id=custom --", &have_all).unwrap(),
            vec!["xdg-terminal-exec", "--app-id=custom", "--"]
        );
    }

    #[test]
    fn precedence_xdg_terminal_exec_over_env_and_emulators() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("TERMINAL", "kitty");
        }
        let have = |b: &str| b == "xdg-terminal-exec" || b == "kitty" || b == "kgx";
        let resolved = terminal_prefix("", &have).unwrap();
        unsafe {
            std::env::remove_var("TERMINAL");
        }
        assert_eq!(resolved, vec!["xdg-terminal-exec", "--"]);
    }

    #[test]
    fn no_hardcoded_emulator_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TERMINAL");
        }
        let have = |b: &str| b == "kitty" || b == "alacritty";
        assert!(terminal_prefix("", &have).is_none());
    }

    #[test]
    fn foreign_de_does_not_consult_gnome_preferences() {
        let _lock = ENV_LOCK.lock().unwrap();
        let old_de = std::env::var("XDG_CURRENT_DESKTOP").ok();
        unsafe {
            std::env::set_var("XDG_CURRENT_DESKTOP", "XFCE");
            std::env::remove_var("TERMINAL");
        }
        let have = |b: &str| b == "kitty" || b == "alacritty" || b == "kgx";
        let res = terminal_prefix("", &have);
        if let Some(de) = old_de {
            unsafe {
                std::env::set_var("XDG_CURRENT_DESKTOP", de);
            }
        } else {
            unsafe {
                std::env::remove_var("XDG_CURRENT_DESKTOP");
            }
        }
        assert!(res.is_none());
    }

    #[test]
    fn empty_quoted_override_errors() {
        let _lock = ENV_LOCK.lock().unwrap();
        assert!(build("''", "ssh", &Server::alias("a"), &|_| true).is_err());
        assert!(build("\"\"", "ssh", &Server::alias("a"), &|_| true).is_err());
        assert!(build("'   '", "ssh", &Server::alias("a"), &|_| true).is_err());
    }

    #[test]
    fn terminal_env_precedence_over_debian_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("TERMINAL", "kitty");
        }
        let have = |b: &str| b == "kitty" || b == "x-terminal-emulator";
        let res = terminal_prefix("", &have).unwrap();
        unsafe {
            std::env::remove_var("TERMINAL");
        }
        assert_eq!(res, vec!["kitty"]);
    }

    #[test]
    fn debian_x_terminal_emulator_fallback() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TERMINAL");
        }
        let have = |b: &str| b == "x-terminal-emulator" || b == "kitty";
        assert_eq!(
            terminal_prefix("", &have).unwrap(),
            vec!["x-terminal-emulator", "-e"]
        );
    }

    #[test]
    fn gvariant_dequote_test() {
        assert_eq!(
            dequote_gvariant_str("'xdg-terminal-exec'").as_deref(),
            Some("xdg-terminal-exec")
        );
        assert_eq!(dequote_gvariant_str("'--'").as_deref(), Some("--"));
        assert_eq!(
            dequote_gvariant_str("'/opt/My Term/term'").as_deref(),
            Some("/opt/My Term/term")
        );
        assert_eq!(dequote_gvariant_str("'it\\'s'").as_deref(), Some("it's"));
        assert_eq!(dequote_gvariant_str("''").as_deref(), Some(""));
        assert_eq!(dequote_gvariant_str("'unclosed"), None);
        assert_eq!(dequote_gvariant_str("bare_string"), None);
    }

    #[test]
    fn configured_explicit_override_wins() {
        let _lock = ENV_LOCK.lock().unwrap();
        let have = |_: &str| true;
        assert_eq!(
            terminal_prefix("konsole", &have).unwrap(),
            vec!["konsole", "-e"]
        );
        assert_eq!(
            terminal_prefix("alacritty", &have).unwrap(),
            vec!["alacritty", "-e"]
        );
        assert_eq!(terminal_prefix("kitty", &have).unwrap(), vec!["kitty"]);
        assert_eq!(
            terminal_prefix("myterm --run", &have).unwrap(),
            vec!["myterm", "--run"]
        );
    }

    #[test]
    fn quoted_override_and_paths_with_spaces() {
        let _lock = ENV_LOCK.lock().unwrap();
        let have = |_: &str| true;
        assert_eq!(
            terminal_prefix("\"/opt/Custom Terminal/bin/term\" --run", &have).unwrap(),
            vec!["/opt/Custom Terminal/bin/term", "--run"]
        );
        assert_eq!(
            terminal_prefix("kitty -o \"font_size = 12\" --title 'My SSH'", &have).unwrap(),
            vec!["kitty", "-o", "font_size = 12", "--title", "My SSH"]
        );
        assert_eq!(
            terminal_prefix("\"/opt/My Apps/alacritty\"", &have).unwrap(),
            vec!["/opt/My Apps/alacritty", "-e"]
        );
        assert_eq!(
            terminal_prefix("/usr/bin/my\\ term --run", &have).unwrap(),
            vec!["/usr/bin/my term", "--run"]
        );
    }

    #[test]
    fn parse_command_line_errors_on_unclosed_quotes() {
        assert!(parse_command_line("myterm \"unclosed quote").is_err());
        assert!(parse_command_line("myterm 'unclosed single").is_err());
        assert!(parse_command_line("myterm trailing\\").is_err());
    }

    #[test]
    fn build_errors_on_malformed_quotes() {
        let _lock = ENV_LOCK.lock().unwrap();
        let res = build("myterm \"unclosed", "ssh", &Server::alias("a"), &|_| true);
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("unclosed") || err.contains("quote"),
            "err: {err}"
        );
    }

    #[test]
    fn build_errors_without_resolver() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TERMINAL");
        }
        let res = build("", "ssh", &Server::alias("a"), &|_| false);
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(err.contains("terminal emulator"), "err: {err}");

        let res_kitty = build("", "ssh", &Server::alias("a"), &|b| b == "kitty");
        assert!(res_kitty.is_err());

        let v = build("", "ssh", &Server::alias("a"), &|b| {
            b == "xdg-terminal-exec"
        })
        .unwrap();
        assert_eq!(v, vec!["xdg-terminal-exec", "--", "ssh", "--", "a"]);
    }

    #[test]
    fn ssh_alias_vs_manual() {
        let a = ssh_command("ssh", &Server::alias("my-server"));
        assert_eq!(a, vec!["ssh", "--", "my-server"]);
        let m = Server {
            host: Some("h".into()),
            user: Some("u".into()),
            port: Some(2200),
            ..Server::alias("m")
        };
        assert_eq!(
            ssh_command("ssh", &m),
            vec!["ssh", "-p", "2200", "--", "u@h"]
        );
    }
}
