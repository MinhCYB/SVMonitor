//! Read-only discovery of SSH host aliases from `~/.ssh/config` (with `Include`),
//! and resolution of effective settings through `ssh -G` (no connection made).
//! This module never writes to any SSH file.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_FILES: usize = 256;

pub fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh/config"))
}

/// Discover concrete host aliases (no wildcards / negations) in order of first appearance.
pub fn discover(config: &Path) -> Vec<String> {
    let base = config.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut visited = 0usize;
    walk(config, &base, 0, &mut visited, &mut seen, &mut out);
    out
}

fn walk(
    file: &Path,
    base: &Path,
    depth: usize,
    visited: &mut usize,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) {
    if depth > MAX_INCLUDE_DEPTH || *visited >= MAX_FILES {
        return;
    }
    *visited += 1;
    let Ok(text) = fs::read_to_string(file) else {
        return;
    };
    for line in text.lines() {
        let Some((key, rest)) = split_directive(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("host") {
            for pat in tokenize(rest) {
                if is_concrete(&pat) && seen.insert(pat.clone()) {
                    out.push(pat);
                }
            }
        } else if key.eq_ignore_ascii_case("include") {
            for pat in tokenize(rest) {
                for inc in expand_include(&pat, base) {
                    walk(&inc, base, depth + 1, visited, seen, out);
                }
            }
        }
    }
}

/// Split `Key value`, `Key=value` or `Key = value`; skip blanks and comments.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = &line[..idx];
    let rest = line[idx..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    Some((key, rest))
}

/// Split into whitespace separated words honouring double quotes; stop at an unquoted `#`.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            '#' if !in_q => break,
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

fn is_concrete(p: &str) -> bool {
    !p.is_empty() && !p.contains(['*', '?', '!', '%'])
}

fn expand_include(pat: &str, base: &Path) -> Vec<PathBuf> {
    let mut p = if let Some(r) = pat.strip_prefix("~/") {
        match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(r),
            None => return vec![],
        }
    } else {
        PathBuf::from(pat)
    };
    if p.is_relative() {
        p = base.join(p);
    }
    glob_path(&p)
}

/// Minimal glob supporting `*` and `?` in any path component. Results sorted lexically.
pub fn glob_path(pattern: &Path) -> Vec<PathBuf> {
    let mut results = vec![PathBuf::new()];
    for comp in pattern.components() {
        let name = comp.as_os_str().to_string_lossy().into_owned();
        let mut next = Vec::new();
        if name.contains(['*', '?']) {
            for dir in &results {
                let Ok(rd) = fs::read_dir(if dir.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    dir
                }) else {
                    continue;
                };
                let mut names: Vec<String> = rd
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| !n.starts_with('.') && wildcard_match(&name, n))
                    .collect();
                names.sort();
                next.extend(names.into_iter().map(|n| dir.join(n)));
            }
        } else {
            for dir in &results {
                next.push(dir.join(&name));
            }
        }
        results = next;
    }
    results.retain(|p| p.is_file());
    results
}

fn wildcard_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub proxy_jump: Option<String>,
    pub identity_files: Vec<String>,
}

/// Parse the output of `ssh -G`.
pub fn parse_ssh_g(out: &str) -> Resolved {
    let mut r = Resolved {
        port: 22,
        ..Default::default()
    };
    for line in out.lines() {
        let Some((k, v)) = line.split_once(' ') else {
            continue;
        };
        match k {
            "hostname" => r.hostname = v.trim().to_string(),
            "user" => r.user = v.trim().to_string(),
            "port" => r.port = v.trim().parse().unwrap_or(22),
            "proxyjump" if v.trim() != "none" => r.proxy_jump = Some(v.trim().to_string()),
            "identityfile" => r.identity_files.push(v.trim().to_string()),
            _ => {}
        }
    }
    r
}

/// Resolve an alias with `ssh -G`. Pure config evaluation; does not connect.
pub fn resolve(ssh_bin: &str, alias: &str) -> Option<Resolved> {
    if alias.starts_with('-') {
        return None;
    }
    let out = Command::new(ssh_bin)
        .args(["-G", "--", alias])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_ssh_g(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_and_directives() {
        assert_eq!(split_directive("Host a b"), Some(("Host", "a b")));
        assert_eq!(split_directive("  Host=a"), Some(("Host", "a")));
        assert_eq!(split_directive("Host = a"), Some(("Host", "a")));
        assert_eq!(split_directive("# Host a"), None);
        assert_eq!(tokenize("a \"b c\" d # x"), vec!["a", "b c", "d"]);
    }

    #[test]
    fn wildcard() {
        assert!(wildcard_match("*.conf", "a.conf"));
        assert!(!wildcard_match("*.conf", "a.cfg"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn discovers_with_include_and_skips_wildcards() {
        let d = tempfile::tempdir().unwrap();
        let ssh = d.path().join(".ssh");
        fs::create_dir_all(ssh.join("conf.d")).unwrap();
        fs::write(
            ssh.join("config"),
            "Include conf.d/*.conf\nHost web1 web2\n  HostName x\nHost *\n  User a\nHost !bad ok\nHost *.corp\nHost=eq\n",
        )
        .unwrap();
        fs::write(
            ssh.join("conf.d/a.conf"),
            "Host inc1\n  HostName y\nHost web1\n",
        )
        .unwrap();
        fs::write(ssh.join("conf.d/skip.txt"), "Host nope\n").unwrap();
        let hosts = discover(&ssh.join("config"));
        assert_eq!(hosts, vec!["inc1", "web1", "web2", "ok", "eq"]);
    }

    #[test]
    fn include_loop_terminates() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("config");
        fs::write(&c, format!("Include {}\nHost a\n", c.display())).unwrap();
        assert_eq!(discover(&c), vec!["a"]);
    }

    #[test]
    fn missing_config_is_empty() {
        assert!(discover(Path::new("/nonexistent/config")).is_empty());
    }

    #[test]
    fn parses_ssh_g() {
        let r = parse_ssh_g(
            "host a\nhostname h.example\nuser bob\nport 2222\nproxyjump none\nidentityfile ~/.ssh/k1\nidentityfile ~/.ssh/k2\n",
        );
        assert_eq!(r.hostname, "h.example");
        assert_eq!(r.user, "bob");
        assert_eq!(r.port, 2222);
        assert_eq!(r.proxy_jump, None);
        assert_eq!(r.identity_files.len(), 2);
        let r = parse_ssh_g("proxyjump bastion\n");
        assert_eq!(r.proxy_jump.as_deref(), Some("bastion"));
    }
}
