//! The control socket.
//!
//! A listener thread accepts connections and hands each one its own thread;
//! requests are forwarded to the render loop over a channel and answered from
//! there. The render loop therefore never blocks on a slow or dead client,
//! and all state stays on one thread with no locks.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryIter, channel};

use anyhow::{Context, Result, bail};
use wallflux_core::ipc::{Request, Response};
use wallflux_core::paths;

use crate::wake::Waker;

/// A request waiting to be answered. Dropping the sender without replying
/// makes the client see a closed connection, which its error message covers.
pub struct Pending {
    pub request: Request,
    reply: Sender<Response>,
}

impl Pending {
    pub fn answer(self, response: Response) {
        // The client may have hung up; that is not our problem to report.
        let _ = self.reply.send(response);
    }
}

pub struct IpcServer {
    requests: Receiver<Pending>,
    socket_path: PathBuf,
}

impl IpcServer {
    /// Binds the socket and starts accepting. Fails when another daemon is
    /// already listening, which is how wallflux enforces one instance.
    pub fn start(waker: Waker) -> Result<Self> {
        let socket_path = paths::socket_path()?;
        if let Some(parent) = socket_path.parent() {
            paths::ensure_dir(parent)?;
        }
        Self::start_at(&socket_path, waker)
    }

    pub fn start_at(socket_path: &Path, waker: Waker) -> Result<Self> {
        if socket_path.exists() {
            // A socket file left behind by a crash is not a running daemon.
            // Probing it is the only way to tell the difference.
            match UnixStream::connect(socket_path) {
                Ok(_) => bail!(
                    "another wallflux daemon is already running on {} \
                     (stop it with `wallfluxctl shutdown`)",
                    socket_path.display()
                ),
                Err(_) => {
                    tracing::info!(path = %socket_path.display(), "removing a stale socket");
                    std::fs::remove_file(socket_path)
                        .with_context(|| format!("cannot remove {}", socket_path.display()))?;
                }
            }
        }

        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("cannot bind {}", socket_path.display()))?;
        // The socket lives in the user's runtime directory, but tighten the
        // mode anyway: anything that can write here can change the wallpaper.
        restrict_permissions(socket_path)?;

        let (tx, requests) = channel();
        let path_for_log = socket_path.to_path_buf();
        std::thread::Builder::new()
            .name("wallflux-ipc".to_string())
            .spawn(move || {
                tracing::debug!(path = %path_for_log.display(), "accepting IPC connections");
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let tx = tx.clone();
                            let waker = waker.clone();
                            // One thread per connection. Front-ends connect,
                            // ask and disconnect, so these are short-lived.
                            if let Err(err) = std::thread::Builder::new()
                                .name("wallflux-ipc-conn".to_string())
                                .spawn(move || serve(stream, tx, waker))
                            {
                                tracing::warn!(%err, "cannot spawn an IPC connection thread");
                            }
                        }
                        Err(err) => {
                            tracing::warn!(%err, "IPC accept failed");
                            break;
                        }
                    }
                }
            })
            .context("cannot start the IPC listener thread")?;

        Ok(Self {
            requests,
            socket_path: socket_path.to_path_buf(),
        })
    }

    /// Requests that have arrived since the last call. Never blocks.
    pub fn pending(&self) -> TryIter<'_, Pending> {
        self.requests.try_iter()
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Leave no stale socket behind on a clean exit.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn serve(stream: UnixStream, tx: Sender<Pending>, waker: Waker) {
    let mut writer = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%err, "cannot clone an IPC stream");
            return;
        }
    };
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                tracing::debug!(%err, "IPC read failed");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let (reply_tx, reply_rx) = channel();
                if tx
                    .send(Pending {
                        request,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    // The render loop is gone, so the daemon is shutting down.
                    return;
                }
                waker.wake();
                match reply_rx.recv() {
                    Ok(response) => response,
                    Err(_) => Response::Error {
                        message: "the daemon stopped before answering".to_string(),
                    },
                }
            }
            Err(err) => Response::Error {
                message: format!("malformed request: {err}"),
            },
        };

        let mut encoded = match serde_json::to_string(&response) {
            Ok(encoded) => encoded,
            Err(err) => {
                tracing::error!(%err, "cannot encode an IPC response");
                return;
            }
        };
        encoded.push('\n');
        if writer.write_all(encoded.as_bytes()).is_err() || writer.flush().is_err() {
            tracing::debug!("IPC client hung up before reading the reply");
            return;
        }
    }
}

/// `0600`: only the owner may talk to the daemon.
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("cannot set permissions on {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallflux_core::ipc::Client;

    fn socket_in_temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wallflux-ipc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("daemon.sock")
    }

    #[test]
    fn a_request_reaches_the_loop_and_the_reply_reaches_the_client() {
        let path = socket_in_temp("roundtrip");
        let (waker, receiver) = crate::wake::pipe().unwrap();
        let server = IpcServer::start_at(&path, waker).unwrap();

        let client_path = path.clone();
        let client = std::thread::spawn(move || {
            let mut client = Client::connect_to(&client_path).unwrap();
            client.request(&Request::Ping).unwrap()
        });

        // Stand in for the render loop: wait for the request, answer it.
        let pending = loop {
            if let Some(pending) = server.pending().next() {
                break pending;
            }
            std::thread::yield_now();
        };
        assert!(matches!(pending.request, Request::Ping));
        pending.answer(Response::Pong {
            version: "0.1.0".into(),
            ipc_version: 1,
        });

        match client.join().unwrap() {
            Response::Pong { version, .. } => assert_eq!(version, "0.1.0"),
            other => panic!("got {other:?}"),
        }
        // The request also woke the loop.
        receiver.drain();
    }

    #[test]
    fn a_malformed_request_is_answered_not_dropped() {
        let path = socket_in_temp("malformed");
        let (waker, _receiver) = crate::wake::pipe().unwrap();
        let _server = IpcServer::start_at(&path, waker).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        stream.write_all(b"{\"kind\":\"nonsense\"}\n").unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        let response: Response = serde_json::from_str(reply.trim()).unwrap();
        match response {
            Response::Error { message } => assert!(message.contains("malformed"), "{message}"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_stale_socket_file_is_replaced() {
        let path = socket_in_temp("stale");
        // A plain file where the socket should be, as a crash would leave.
        std::fs::write(&path, b"").unwrap();
        let (waker, _receiver) = crate::wake::pipe().unwrap();
        let server = IpcServer::start_at(&path, waker);
        assert!(server.is_ok(), "{:?}", server.err());
    }

    #[test]
    fn a_second_daemon_refuses_to_start() {
        let path = socket_in_temp("single");
        let (waker, _receiver) = crate::wake::pipe().unwrap();
        let _first = IpcServer::start_at(&path, waker.clone()).unwrap();
        let err = match IpcServer::start_at(&path, waker) {
            Ok(_) => panic!("a second daemon was allowed to bind the socket"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    fn the_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = socket_in_temp("perms");
        let (waker, _receiver) = crate::wake::pipe().unwrap();
        let _server = IpcServer::start_at(&path, waker).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn dropping_the_server_removes_the_socket() {
        let path = socket_in_temp("cleanup");
        let (waker, _receiver) = crate::wake::pipe().unwrap();
        let server = IpcServer::start_at(&path, waker).unwrap();
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }
}
