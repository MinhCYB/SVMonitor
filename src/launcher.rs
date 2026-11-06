//! "Open SSH" launcher: runs the user's terminal emulator with an interactive `ssh` session.
//! Uses the plain alias so every OpenSSH setting applies; never embeds credentials.
use crate::config::Server;
use anyhow::{Result, bail};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// (binary, args-before-command) for common terminals; the ssh command follows.
const TERMINALS: &[(&str, &[&str])] = &[
    ("xdg-terminal-exec", &[]),
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("foot", &[]),
    ("wezterm", &["start", "--"]),
    ("gnome-terminal", &["--"]),
    ("kgx", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("tilix", &["-e"]),
    ("terminator", &["-x"]),
    ("lxterminal", &["-e"]),
    ("mate-terminal", &["-x"]),
    ("ptyxis", &["--"]),
    ("xterm", &["-e"]),
];

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

/// Terminal command prefix for `configured` (a user supplied command line, may contain args) or auto-detected.
pub fn terminal_prefix(configured: &str, have: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let parts: Vec<String> = configured.split_whitespace().map(String::from).collect();
        // Known terminal given by bare name: add its exec flag.
        if parts.len() == 1 {
            if let Some((_, args)) = TERMINALS.iter().find(|(b, _)| *b == parts[0]) {
                let mut v = parts.clone();
                v.extend(args.iter().map(|s| s.to_string()));
                return Some(v);
            }
        }
        return Some(parts);
    }
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.trim().is_empty() && have(t.split_whitespace().next().unwrap_or("")) {
            return terminal_prefix(&t, have);
        }
    }
    TERMINALS.iter().find(|(b, _)| have(b)).map(|(b, args)| {
        let mut v = vec![b.to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        v
    })
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
    let Some(mut pre) = terminal_prefix(terminal, have) else {
        bail!("no terminal emulator found; set general.terminal in the config");
    };
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
    let mut child = Command::new(opener)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
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

    #[test]
    fn auto_detect_picks_first_available() {
        let have = |b: &str| b == "alacritty" || b == "xterm";
        assert_eq!(terminal_prefix("", &have).unwrap(), vec!["alacritty", "-e"]);
        assert!(terminal_prefix("", &|_| false).is_none());
    }

    #[test]
    fn configured_terminal() {
        let have = |_: &str| true;
        assert_eq!(
            terminal_prefix("konsole", &have).unwrap(),
            vec!["konsole", "-e"]
        );
        assert_eq!(
            terminal_prefix("myterm --run", &have).unwrap(),
            vec!["myterm", "--run"]
        );
    }

    #[test]
    fn ssh_alias_vs_manual() {
        let a = ssh_command("ssh", &Server::alias("catplus"));
        assert_eq!(a, vec!["ssh", "--", "catplus"]);
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

    #[test]
    fn build_errors_without_terminal() {
        assert!(build("", "ssh", &Server::alias("a"), &|_| false).is_err());
        let v = build("", "ssh", &Server::alias("a"), &|b| b == "kitty").unwrap();
        assert_eq!(v, vec!["kitty", "ssh", "--", "a"]);
    }
}
