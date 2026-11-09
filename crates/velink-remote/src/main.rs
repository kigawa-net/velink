mod k8s_manager;
mod vnc_server;

use futures_util::{SinkExt, StreamExt};
use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{Request, Response},
    http::StatusCode,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

struct SessionGuard(Arc<AtomicBool>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

// Inspect the request line without consuming bytes needed by the WebSocket handshake.
// Kubernetes probes are plain HTTP and must be handled before tungstenite validates
// the Upgrade headers. Bound both request-line length and time for slow peers.
async fn health_request(stream: &mut TcpStream) -> Result<bool> {
    let mut buffer = [0; 1024];
    let health = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count = stream.peek(&mut buffer).await?;
            if count == 0 {
                return Err("peer closed before HTTP request".into());
            }
            if let Some(end) = buffer[..count].iter().position(|&byte| byte == b'\n') {
                let line = &buffer[..=end];
                return Result::Ok(
                    line == b"GET /healthz HTTP/1.1\r\n" || line == b"GET /healthz HTTP/1.0\r\n",
                );
            }
            if count == buffer.len() {
                return Err("HTTP request line too long".into());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    if health {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await?);
                if header.len() > 16 * 1024 {
                    return Err("HTTP headers too long".into());
                }
            }
            Result::Ok(())
        })
        .await??;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nok\n")
            .await?;
        stream.shutdown().await?;
    }
    Ok(health)
}

async fn connection(mut stream: TcpStream, port: u16, active: Arc<AtomicBool>) -> Result<()> {
    if health_request(&mut stream).await? {
        return Ok(());
    }
    stream.set_nodelay(true)?;
    let mut guard = None;
    let websocket = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
            let status = match request.uri().path() {
                "/vnc"
                    if active
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok() =>
                {
                    guard = Some(SessionGuard(active.clone()));
                    return Ok(response);
                }
                "/vnc" => StatusCode::CONFLICT,
                _ => StatusCode::NOT_FOUND,
            };
            let mut response =
                tokio_tungstenite::tungstenite::http::Response::new(Some(format!("{status}\n")));
            *response.status_mut() = status;
            Err(response)
        }),
    )
    .await??;
    let _guard = guard;
    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    tcp.set_nodelay(true)?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut ws_write, mut ws_read) = websocket.split();
    let upstream = async {
        while let Some(message) = ws_read.next().await {
            match message? {
                Message::Binary(data) => tcp_write.write_all(&data).await?,
                Message::Close(_) => break,
                Message::Text(_) => return Err("VNC requires binary WebSocket messages".into()),
                _ => {}
            }
        }
        Result::Ok(())
    };
    let downstream = async {
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = tcp_read.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            ws_write
                .send(Message::Binary(buffer[..count].to_vec().into()))
                .await?;
        }
        Result::Ok(())
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = k8s_manager::config_from_env()?;
    let mut desktop = vnc_server::VncServer::start(&config)?;
    let listener = TcpListener::bind(config.listen_address).await?;
    eprintln!(
        "velink-remote: {} / {:?}, ws://{}/vnc",
        config.namespace, config.pod_name, config.listen_address
    );
    let active = Arc::new(AtomicBool::new(false));
    let mut tasks = tokio::task::JoinSet::new();
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = health.tick() => desktop.check()?,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            result = listener.accept() => {
                let (stream, _) = result?;
                let active = active.clone();
                let port = config.vnc_port;
                tasks.spawn(async move {
                    if let Err(error) = connection(stream, port, active).await {
                        eprintln!("connection ended: {error}");
                    }
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_http_probe_does_not_reserve_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let active = Arc::new(AtomicBool::new(false));
        let server_active = active.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            connection(stream, 1, server_active).await.unwrap();
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        // Also exercise a fragmented HTTP request line.
        client.write_all(b"GET /heal").await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        client
            .write_all(b"thz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(!active.load(Ordering::Acquire));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn websocket_bridges_bidirectionally_and_rejects_second_viewer() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let vnc = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = vnc.local_addr().unwrap().port();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let active = Arc::new(AtomicBool::new(false));
            let server_active = active.clone();
            let (finished, done) = tokio::sync::oneshot::channel();
            let acceptor = tokio::spawn(async move {
                let (first, _) = listener.accept().await.unwrap();
                let first_active = server_active.clone();
                let first = tokio::spawn(async move {
                    let _ = connection(first, port, first_active).await;
                    let _ = finished.send(());
                });
                let (second, _) = listener.accept().await.unwrap();
                assert!(connection(second, port, server_active).await.is_err());
                first.await.unwrap();
            });
            let (mut viewer, _) = tokio_tungstenite::connect_async(format!("ws://{address}/vnc"))
                .await
                .unwrap();
            let (mut backend, _) = vnc.accept().await.unwrap();
            backend.write_all(b"RFB 003.008\n").await.unwrap();
            assert_eq!(
                viewer.next().await.unwrap().unwrap().into_data(),
                b"RFB 003.008\n"[..]
            );
            viewer
                .send(Message::Binary(vec![4, 1, 0, 0, 0, 0, 0, 65].into()))
                .await
                .unwrap();
            let mut input = [0; 8];
            backend.read_exact(&mut input).await.unwrap();
            assert_eq!(input, [4, 1, 0, 0, 0, 0, 0, 65]);
            let second = tokio_tungstenite::connect_async(format!("ws://{address}/vnc"))
                .await
                .unwrap_err();
            match second {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    assert_eq!(response.status(), StatusCode::CONFLICT)
                }
                error => panic!("expected HTTP conflict, got {error}"),
            }
            viewer.close(None).await.unwrap();
            done.await.unwrap();
            assert!(!active.load(Ordering::Acquire));
            acceptor.await.unwrap();
        })
        .await
        .unwrap();
    }
}
