# Security review (self-review, V1)

Scope: the `svmonitor` binary, its probe script, packaging units. Method: code review of every process spawn, file write,
parser of untrusted input, and D-Bus call; targeted tests for each fix. Not an external audit.

| # | Area | Finding | Status |
|---|---|---|---|
| 1 | Argument injection | Server names/hosts/users flow into `ssh` argv. A name like `-oProxyCommand=…` would execute commands. | Fixed: `config::validate_name` + host/user/identity validation reject leading `-`, whitespace, control and shell metacharacters; every ssh invocation uses `--` before the destination. Tests: `rejects_dangerous_or_invalid`, `args_are_safe_and_noninteractive`. |
| 2 | Terminal escape injection | Remote-supplied strings (hostname, OS, mount points, ssh stderr) reach the terminal, tray menu, notifications. | Fixed: `util::sanitize_text` strips control/bidi chars and caps length at the parse boundary; notification bodies are markup-escaped. Tests: `remote_strings_cannot_inject_terminal_escapes`, `stderr_detail_is_sanitised`. |
| 3 | Signalling wrong process | `reload`/`status` read a PID from `state.json`; after a crash + PID reuse an unrelated process could receive SIGHUP. | Fixed: PID accepted only if `/proc/<pid>/comm == svmonitor`. Test: `stale_pid_is_not_a_monitor`. |
| 4 | /tmp runtime dir fallback | Without `XDG_RUNTIME_DIR` the dir lives in `/tmp`; a pre-created symlink/foreign dir could redirect writes. | Fixed: `ensure_private_dir` refuses symlinks and dirs not owned by the user, then sets 0700. Test: `private_dir_rejects_symlink_and_sets_mode`. |
| 5 | Credentials | None stored. Config holds alias names and optionally an identity *path*. Files are written 0600 via temp+rename. | By design |
| 6 | Host key verification | Not touched: no `StrictHostKeyChecking`/`UserKnownHostsFile` overrides (asserted in tests). Unknown/changed keys surface as **needs attention**, never auto-accepted. `BatchMode=yes` prevents prompts. | By design |
| 7 | Server-side effects | Probe is a fixed read-only POSIX script piped to `sh -s` (reads /proc, `df`, /etc/os-release). No sudo, no writes, no persistent remote process. `RemoteCommand=none`, no forwarding, no TTY, `PermitLocalCommand=no`. | By design |
| 8 | Resource exhaustion | Remote output capped (256 KiB stdout / 32 KiB stderr), disk rows capped (8), hard deadline kills the whole ssh process group, parallelism bounded (default 4), history rotates at 2 MiB, tailscale response capped 8 MiB. | Verified by tests/bench |
| 9 | `unsafe` | Only libc syscalls (`kill`, `flock`, `clock_gettime`, `getuid`, `localtime_r`, `sigwait`), each with a SAFETY note. | Reviewed |
| 10 | Terminal launcher | `general.terminal` is split on whitespace, never passed through a shell; alias is a separate argv element. The config file is trusted (user-owned, 0600). | Accepted |
| 11 | systemd unit | Only `NoNewPrivileges`, `RestrictSUIDSGID`, `LockPersonality`. A stricter variant (`ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`) was **tried and rejected**: in a user manager it uses a user namespace, root-owned `/etc/ssh/ssh_config.d/*` then appears owned by `nobody` and OpenSSH refuses it, so every probe failed. | Tested on this machine |

## v0.1.1 additions
| # | Area | Finding | Status |
|---|---|---|---|
| 12 | Signal handling | Notifier/D-Bus threads were spawned before signals were blocked, so a process-directed `SIGHUP` terminated the daemon (`svmonitor reload` killed the service). | Fixed: signals are blocked first thing in `run_daemon`, before any thread; one `sigwait` thread handles them. Regression: `tests/daemon_lifecycle.rs` (5 of 6 fail with the old ordering, verified). |
| 13 | Signal mask inheritance | Blocked-signal masks survive `exec`; ssh probes and launched terminals would ignore SIGINT/SIGTERM/SIGHUP. | Fixed: `pre_exec` clears the mask in probe, `open_ssh`, `open_path`. Verified: launched `ssh` has `SigBlk=0`. |
| 14 | Reload races | A probe in flight during reload could write stale data into a changed or disabled server. | Fixed with per-slot generation bump on definition change/disable. |
| 15 | Terminal override | `general.terminal` is split into argv by a small quote-aware parser; no shell is involved, unclosed quotes are rejected, the ssh destination is a separate argv element after `--`. | Reviewed, unit-tested |

## Residual risks
- A malicious *server* controls text we display (mitigated by #2) and can lie about metrics; it cannot make us execute anything.
- A user with write access to `~/.config/svmonitor/config.toml` already controls the account.
- `ssh_binary` is taken from config; a hostile config can run any binary, equivalent to editing `~/.bashrc`.
