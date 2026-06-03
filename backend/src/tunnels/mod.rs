use crate::errors::{AppError, AppResult};
use crate::ssh::SshSession;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

/// Registry of running port-forwards keyed by vault tunnel_id.
/// Stopping a tunnel aborts the listener task and triggers a shutdown signal
/// that in-flight connections check inside their bidi-copy select loops.
pub struct TunnelRegistry {
    inner: Mutex<HashMap<String, RunningTunnel>>,
}

struct RunningTunnel {
    #[allow(dead_code)] // useful for future status/debug surfacing
    kind: String, // "local" | "remote" | "dynamic"
    task: JoinHandle<()>,
    shutdown: Arc<Notify>,
    /// Session this tunnel is bound to — stop() needs it for remote-forward cancel.
    session_id: String,
    /// For remote: stored so we can unregister on stop.
    remote_bind: Option<(String, u32)>,
}

impl TunnelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub async fn active_ids(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }

    pub async fn stop(
        &self,
        tunnel_id: &str,
        sessions: &crate::sessions::SessionRegistry,
    ) -> AppResult<()> {
        let removed = self.inner.lock().await.remove(tunnel_id);
        if let Some(t) = removed {
            t.shutdown.notify_waiters();
            t.task.abort();
            if let Some((bind_host, bind_port)) = t.remote_bind {
                if let Ok(session) = sessions.get(&t.session_id).await {
                    let _ = session.cancel_remote_forward(&bind_host, bind_port).await;
                }
            }
        }
        Ok(())
    }

    pub async fn stop_all_for_session(&self, session_id: &str) {
        let mut guard = self.inner.lock().await;
        let ids: Vec<String> = guard
            .iter()
            .filter(|(_, t)| t.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(t) = guard.remove(&id) {
                t.shutdown.notify_waiters();
                t.task.abort();
            }
        }
    }

    pub async fn start_local(
        &self,
        tunnel_id: String,
        session: Arc<SshSession>,
        local_port: u16,
        remote_host: String,
        remote_port: u16,
    ) -> AppResult<()> {
        {
            let guard = self.inner.lock().await;
            if guard.contains_key(&tunnel_id) {
                return Err(AppError::Invalid("tunnel already running".into()));
            }
        }
        let addr: SocketAddr = ([127, 0, 0, 1], local_port).into();
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| AppError::Internal(format!("bind {addr}: {e}")))?;
        let shutdown = Arc::new(Notify::new());
        let shutdown_for_task = shutdown.clone();
        let session_id = session.id.clone();
        let session_clone = session.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_for_task.notified() => break,
                    accepted = listener.accept() => {
                        let (mut sock, _peer) = match accepted {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("local-forward accept: {e}");
                                continue;
                            }
                        };
                        let session = session_clone.clone();
                        let rhost = remote_host.clone();
                        let shutdown = shutdown_for_task.clone();
                        tokio::spawn(async move {
                            match session.open_direct_tcpip(&rhost, remote_port as u32).await {
                                Ok(channel) => {
                                    let mut remote = channel.into_stream();
                                    tokio::select! {
                                        _ = shutdown.notified() => {},
                                        _ = tokio::io::copy_bidirectional(&mut sock, &mut remote) => {},
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("direct-tcpip {}:{}: {e}", rhost, remote_port);
                                    let _ = sock.shutdown().await;
                                }
                            }
                        });
                    }
                }
            }
        });
        self.inner.lock().await.insert(
            tunnel_id,
            RunningTunnel {
                kind: "local".into(),
                task,
                shutdown,
                session_id,
                remote_bind: None,
            },
        );
        Ok(())
    }

    pub async fn start_dynamic(
        &self,
        tunnel_id: String,
        session: Arc<SshSession>,
        local_port: u16,
    ) -> AppResult<()> {
        {
            let guard = self.inner.lock().await;
            if guard.contains_key(&tunnel_id) {
                return Err(AppError::Invalid("tunnel already running".into()));
            }
        }
        let addr: SocketAddr = ([127, 0, 0, 1], local_port).into();
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| AppError::Internal(format!("bind {addr}: {e}")))?;
        let shutdown = Arc::new(Notify::new());
        let shutdown_for_task = shutdown.clone();
        let session_id = session.id.clone();
        let session_clone = session.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_for_task.notified() => break,
                    accepted = listener.accept() => {
                        let (sock, _) = match accepted {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("dynamic accept: {e}");
                                continue;
                            }
                        };
                        let session = session_clone.clone();
                        let shutdown = shutdown_for_task.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_socks5(sock, session, shutdown).await {
                                tracing::debug!("socks5: {e}");
                            }
                        });
                    }
                }
            }
        });
        self.inner.lock().await.insert(
            tunnel_id,
            RunningTunnel {
                kind: "dynamic".into(),
                task,
                shutdown,
                session_id,
                remote_bind: None,
            },
        );
        Ok(())
    }

    pub async fn start_remote(
        &self,
        tunnel_id: String,
        session: Arc<SshSession>,
        remote_bind_host: String,
        remote_bind_port: u16,
        local_host: String,
        local_port: u16,
    ) -> AppResult<()> {
        {
            let guard = self.inner.lock().await;
            if guard.contains_key(&tunnel_id) {
                return Err(AppError::Invalid("tunnel already running".into()));
            }
        }
        let session_id = session.id.clone();
        let remote_bound_port = session
            .register_remote_forward(
                remote_bind_host.clone(),
                remote_bind_port as u32,
                local_host,
                local_port,
            )
            .await?;
        // No task is needed — incoming forwarded-tcpip channels are dispatched by
        // the SSH handler. We just keep a placeholder JoinHandle so stop() works.
        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(async {});
        self.inner.lock().await.insert(
            tunnel_id,
            RunningTunnel {
                kind: "remote".into(),
                task,
                shutdown,
                session_id,
                remote_bind: Some((remote_bind_host, remote_bound_port)),
            },
        );
        Ok(())
    }
}

// ── SOCKS5 (no-auth, CONNECT only) ────────────────────────────────────────────
//
// Minimal RFC 1928 server. ATYP IPv4/IPv6/domain, CMD must be CONNECT.

async fn handle_socks5(
    mut client: TcpStream,
    session: Arc<SshSession>,
    shutdown: Arc<Notify>,
) -> Result<(), String> {
    // Greeting: VER NMETHODS [METHODS]
    let mut greet = [0u8; 2];
    read_exact_handshake(&mut client, &mut greet).await?;
    if greet[0] != 0x05 {
        return Err(format!("bad socks version: {}", greet[0]));
    }
    let nmethods = greet[1] as usize;
    let mut methods = vec![0u8; nmethods];
    if nmethods > 0 {
        read_exact_handshake(&mut client, &mut methods).await?;
    }
    // Reply: VER METHOD (0x00 = no auth)
    client
        .write_all(&[0x05, 0x00])
        .await
        .map_err(|e| e.to_string())?;

    // Request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let mut req_head = [0u8; 4];
    read_exact_handshake(&mut client, &mut req_head).await?;
    if req_head[0] != 0x05 {
        return Err("bad request version".into());
    }
    if req_head[1] != 0x01 {
        // 0x07 command not supported
        let _ = client
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        return Err("only CONNECT supported".into());
    }
    let target_host: String = match req_head[3] {
        0x01 => {
            let mut b = [0u8; 4];
            read_exact_handshake(&mut client, &mut b).await?;
            format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
        }
        0x03 => {
            let mut len = [0u8; 1];
            read_exact_handshake(&mut client, &mut len).await?;
            if len[0] == 0 {
                return Err("empty socks5 domain".into());
            }
            let mut buf = vec![0u8; len[0] as usize];
            read_exact_handshake(&mut client, &mut buf).await?;
            String::from_utf8(buf).map_err(|e| e.to_string())?
        }
        0x04 => {
            let mut b = [0u8; 16];
            read_exact_handshake(&mut client, &mut b).await?;
            let parts: Vec<String> = b
                .chunks(2)
                .map(|p| format!("{:x}", u16::from_be_bytes([p[0], p[1]])))
                .collect();
            parts.join(":")
        }
        other => {
            let _ = client
                .write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await;
            return Err(format!("ATYP {other} not supported"));
        }
    };
    let mut port_buf = [0u8; 2];
    read_exact_handshake(&mut client, &mut port_buf).await?;
    let target_port = u16::from_be_bytes(port_buf);
    if target_port == 0 {
        return Err("socks5 target port 0 is invalid".into());
    }

    match session
        .open_direct_tcpip(&target_host, target_port as u32)
        .await
    {
        Ok(channel) => {
            // 0x00 success, then echo a dummy bound addr (0.0.0.0:0).
            client
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .map_err(|e| e.to_string())?;
            let mut remote = channel.into_stream();
            tokio::select! {
                _ = shutdown.notified() => {},
                _ = tokio::io::copy_bidirectional(&mut client, &mut remote) => {},
            }
            Ok(())
        }
        Err(e) => {
            let _ = client
                .write_all(&[0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await; // host unreachable
            Err(format!("direct-tcpip {target_host}:{target_port}: {e}"))
        }
    }
}

async fn read_exact_handshake(client: &mut TcpStream, buf: &mut [u8]) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(10), client.read_exact(buf))
        .await
        .map_err(|_| "socks5 handshake timed out".to_string())?
        .map(|_| ())
        .map_err(|e| e.to_string())
}
