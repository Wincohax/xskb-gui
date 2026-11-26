//! Single-instance enforcement over a Unix domain socket in `$XDG_RUNTIME_DIR`
//! (falling back to the system temp directory). When a second instance starts,
//! it signals the running instance to show its window and exits cleanly.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

const SOCKET_IO_TIMEOUT: Duration = Duration::from_millis(500);
const REPLY_OK: &str = "OK\n";

/// Commands exchanged over the single-instance Unix socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    /// Bring up or focus the main window, optionally carrying a Wayland activation token.
    Show { activation_token: Option<String> },
    /// Check liveness without surfacing a hidden window (used by `--background`).
    Ping,
}

/// Encode an [`IpcCommand`] as a single newline-terminated wire line.
pub fn encode_command(cmd: &IpcCommand) -> String {
    match cmd {
        IpcCommand::Show {
            activation_token: Some(tok),
        } => {
            let clean: String = tok
                .chars()
                .filter(|c| *c != '\r' && *c != '\n')
                .collect();
            if clean.is_empty() {
                "SHOW\n".to_string()
            } else {
                format!("SHOW {clean}\n")
            }
        }
        IpcCommand::Show {
            activation_token: None,
        } => "SHOW\n".to_string(),
        IpcCommand::Ping => "PING\n".to_string(),
    }
}

/// Decode a single wire line into an [`IpcCommand`].
pub fn decode_command(line: &str) -> Option<IpcCommand> {
    let trimmed = line.trim();
    if trimmed == "SHOW" {
        return Some(IpcCommand::Show {
            activation_token: None,
        });
    }
    if let Some(rest) = trimmed.strip_prefix("SHOW ") {
        let token = rest.trim();
        return Some(IpcCommand::Show {
            activation_token: if token.is_empty() {
                None
            } else {
                Some(token.to_string())
            },
        });
    }
    if trimmed == "PING" {
        return Some(IpcCommand::Ping);
    }
    None
}

const MAX_UNIX_SOCKET_PATH_LEN: usize = 100;

/// Return the default single-instance socket path (`$XDG_RUNTIME_DIR/xskb-gui.sock`,
/// falling back to the temp dir or `/tmp/xskb-gui-<uid>.sock` while staying within
/// Linux's `SUN_LEN` path limit).
pub fn default_socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        let candidate = PathBuf::from(dir).join("xskb-gui.sock");
        if candidate.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_LEN {
            return candidate;
        }
    }
    let uid = unsafe { libc::geteuid() };
    let filename = format!("xskb-gui-{uid}.sock");
    let temp_candidate = std::env::temp_dir().join(&filename);
    if temp_candidate.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_LEN {
        temp_candidate
    } else {
        PathBuf::from("/tmp").join(filename)
    }
}

/// Outcome of attempting to acquire the single-instance lock.
pub enum AcquireOutcome {
    /// This process is the primary instance; keep the guard alive until exit.
    Primary(SingleInstanceGuard),
    /// Another instance is already running and has been notified.
    ActivatedExisting,
}

/// Removes the Unix domain socket and stops the listener thread when dropped.
pub struct SingleInstanceGuard {
    socket_path: Arc<PathBuf>,
    stop: Arc<AtomicBool>,
}

impl SingleInstanceGuard {
    /// Cloneable path handle so signal handlers can clean up the socket before exiting.
    pub fn socket_path(&self) -> Arc<PathBuf> {
        Arc::clone(&self.socket_path)
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Poke the listener so it wakes out of `accept()` promptly.
        let _ = UnixStream::connect(&*self.socket_path);
        let _ = fs::remove_file(&*self.socket_path);
    }
}

fn send_command(path: &Path, cmd: &IpcCommand) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT))?;
    stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT))?;

    let wire = encode_command(cmd);
    stream.write_all(wire.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    let _ = reader.read_line(&mut reply);
    Ok(())
}

fn bind_listener(path: &Path) -> Result<Option<UnixListener>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket dir {}", parent.display()))?;
    }

    match UnixListener::bind(path) {
        Ok(listener) => Ok(Some(listener)),
        Err(err) if err.kind() == ErrorKind::AddrInUse => Ok(None),
        Err(err) => Err(err)
            .with_context(|| format!("failed to bind single-instance socket {}", path.display())),
    }
}

/// Try to notify an already-running instance at `path`. If none is listening,
/// remove any stale socket file, bind a new listener, and spawn the IPC thread.
pub fn acquire_or_notify<F>(
    path: PathBuf,
    cmd: &IpcCommand,
    on_command: F,
) -> Result<AcquireOutcome>
where
    F: Fn(IpcCommand) + Send + 'static,
{
    // First check whether an existing instance is alive.
    match send_command(&path, cmd) {
        Ok(()) => return Ok(AcquireOutcome::ActivatedExisting),
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }
        Err(_) => {
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }
    }

    let listener = match bind_listener(&path)? {
        Some(listener) => listener,
        None => {
            // Lost a startup race with another instance — notify it instead.
            if send_command(&path, cmd).is_ok() {
                return Ok(AcquireOutcome::ActivatedExisting);
            }
            // Still unreachable: clear stale file and try one final bind.
            let _ = fs::remove_file(&path);
            bind_listener(&path)?
                .context("single-instance socket remained busy after retry")?
        }
    };

    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let socket_path = Arc::new(path);

    thread::Builder::new()
        .name("xskb-ipc".into())
        .spawn(move || {
            for incoming in listener.incoming() {
                if stop_bg.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = incoming else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut line).is_ok()
                    && let Some(cmd) = decode_command(&line)
                {
                    on_command(cmd);
                    let _ = stream.write_all(REPLY_OK.as_bytes());
                    let _ = stream.flush();
                }
            }
        })
        .context("failed to spawn xskb-ipc listener thread")?;

    Ok(AcquireOutcome::Primary(SingleInstanceGuard {
        socket_path,
        stop,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn unique_socket_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let name = format!("xskb-{label}-{}-{nanos}.sock", std::process::id());
        let candidate = std::env::temp_dir().join(&name);
        if candidate.as_os_str().len() <= MAX_UNIX_SOCKET_PATH_LEN {
            candidate
        } else {
            PathBuf::from("/tmp").join(name)
        }
    }

    #[test]
    fn encode_decode_ipc_commands_roundtrip() {
        assert_eq!(
            decode_command(&encode_command(&IpcCommand::Show {
                activation_token: None
            })),
            Some(IpcCommand::Show {
                activation_token: None
            })
        );
        assert_eq!(
            decode_command(&encode_command(&IpcCommand::Show {
                activation_token: Some("wayland-token-42".into())
            })),
            Some(IpcCommand::Show {
                activation_token: Some("wayland-token-42".into())
            })
        );
        assert_eq!(
            decode_command(&encode_command(&IpcCommand::Show {
                activation_token: Some("tok\r\ninjected".into())
            })),
            Some(IpcCommand::Show {
                activation_token: Some("tokinjected".into())
            })
        );
        assert_eq!(
            decode_command(&encode_command(&IpcCommand::Ping)),
            Some(IpcCommand::Ping)
        );
        assert_eq!(decode_command("UNKNOWN\n"), None);
        assert_eq!(decode_command("   "), None);
    }

    #[test]
    fn single_instance_recovers_stale_socket_and_notifies_primary() {
        let path = unique_socket_path("stale-and-notify");

        // Create a real stale socket file (bound then dropped without unlinking).
        {
            let _stale = UnixListener::bind(&path).expect("bind stale socket");
        }
        assert!(path.exists(), "stale socket file should exist on disk");

        let (tx, rx) = mpsc::channel();
        let primary = acquire_or_notify(
            path.clone(),
            &IpcCommand::Show {
                activation_token: None,
            },
            move |cmd| {
                let _ = tx.send(cmd);
            },
        )
        .expect("primary should recover stale socket and bind");

        let guard = match primary {
            AcquireOutcome::Primary(g) => g,
            AcquireOutcome::ActivatedExisting => panic!("expected primary ownership"),
        };

        // Second instance should notify the primary and NOT take over the socket.
        let second = acquire_or_notify(
            path.clone(),
            &IpcCommand::Show {
                activation_token: Some("xdg-token-abc".into()),
            },
            |_| panic!("secondary should not start listener"),
        )
        .expect("secondary notification should succeed");
        assert!(matches!(second, AcquireOutcome::ActivatedExisting));

        let received = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("primary should receive SHOW command");
        assert_eq!(
            received,
            IpcCommand::Show {
                activation_token: Some("xdg-token-abc".into())
            }
        );

        // Background second instance sends PING and also exits cleanly.
        let third = acquire_or_notify(path.clone(), &IpcCommand::Ping, |_| {})
            .expect("ping notification should succeed");
        assert!(matches!(third, AcquireOutcome::ActivatedExisting));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("primary should receive PING"),
            IpcCommand::Ping
        );

        drop(guard);
        assert!(!path.exists(), "socket file should be removed on drop");
    }
}
