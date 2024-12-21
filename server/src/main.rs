use log::{debug, error, info, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, UdpSocket};
use tokio::time::{sleep, timeout, Duration};

type ThreadSafeSignals = Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>;

const BUFFER_SIZE: usize = 8192;

fn send_message(message: Value, host: String, port: String) -> Result<(), std::io::Error> {
    let addr = format!("{host}:{port}");
    match TcpStream::connect(addr) {
        Ok(mut socket) => socket.write_all(message.to_string().as_bytes()),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    }
}

async fn tcp_client(
    manager_host: String,
    manager_port: String,
    server_host: String,
    server_port: String,
    max_clients: String,
    signals: ThreadSafeSignals,
) {
    let listener = TcpListener::bind(format!("{server_host}:{server_port}"))
        .await
        .unwrap();

    let register_message = r#"{"message_type": "register", "host": ""#.to_owned()
        + &server_host
        + r#"", "port": ""#
        + &server_port
        + r#"", "max_clients": "#
        + &max_clients
        + r#"}"#;

    send_message(
        serde_json::from_str::<Value>(&register_message).unwrap(),
        manager_host.clone(),
        manager_port.clone(),
    )
    .unwrap();

    debug!("Starting tcp_client thread");
    info!("Server started listening on {server_host}:{server_port}");

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(100)).await;

        let (mut socket, addr) = listener.accept().await.unwrap();
        debug!("New connection from: {}", addr);

        let mut buffer = [0; BUFFER_SIZE];

        match timeout(Duration::from_secs(2), socket.read(&mut buffer)).await {
            Ok(Ok(bytes_read)) => {
                let received = String::from_utf8_lossy(&buffer[..bytes_read]);
                info!("Received TCP message:\n{received}");
                match serde_json::from_str::<Value>(&received) {
                    Ok(json) => {
                        debug!("Deserialized TCP JSON:\n{:#}", json);
                        handle_tcp(signals.clone(), &json);
                    }
                    Err(e) => {
                        error!("Failed to deserialize TCP message into JSON: {:?}", e);
                    }
                }
            }
            Err(_) => (),
            Ok(Err(e)) => {
                error!("Failed to read from socket: {:?}", e);
            }
        }
    }
    debug!("Shutting down tcp_client thread");
}

fn handle_tcp(signals: ThreadSafeSignals, message: &Value) {
    if message.get("message_type").is_none() {
        warn!("Message {message} has no property message_type");
        return;
    }
    let message_type = message["message_type"].clone();
    if message_type.as_str().is_none() {
        warn!("Message type {message_type} is not of type String");
        return;
    }
    match message_type.as_str().unwrap() {
        "shutdown" => {
            signals
                .lock()
                .unwrap()
                .entry("shutdown".to_owned())
                .and_modify(|e| *e.downcast_mut::<bool>().unwrap() = true);
        }

        "register_ack" => {
            signals.lock().unwrap().insert(
                "started_udp".to_owned(),
                Box::new(true) as Box<dyn Any + Sync + Send>,
            );

            info!("Received register_ack message. Starting UDP heartbeat thread");
        }
        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid} received");
        }
    }
}

async fn udp_heartbeat(
    manager_host: String,
    manager_port: String,
    server_host: String,
    server_port: String,
    signals: ThreadSafeSignals,
) {
    debug!("Starting udp_heartbeat thread");

    let socket = UdpSocket::bind(format!("{server_host}:{server_port}"))
        .await
        .unwrap();

    let message = r#"{"message_type": "heartbeat", "host": ""#.to_owned()
        + &server_host
        + r#"", "port": ""#
        + &server_port
        + r#""}"#;

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(1500)).await;

        socket
            .send_to(message.as_bytes(), format!("{manager_host}:{manager_port}"))
            .await
            .unwrap();
    }

    debug!("Shutting down udp_heartbeat thread");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 || args.len() > 6 {
        println!("Usage: RUST_LOG=[debug|info|warn|error] target/release/server [manager_host] [manager_port] [server_host] [server_port] [max_clients=1]");
        return;
    }

    let max_clients = match args.len() {
        5 => "1".to_owned(),
        _ => args[5].clone(),
    };

    let log_file = std::fs::File::create(format!("server_{}_{}.log", args[3], args[4]))
        .expect("Could not create server log file");
    let log_file = Mutex::new(log_file);
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(move |_, record| {
            let mut log_file = log_file.lock().unwrap();
            writeln!(
                log_file,
                "{} [{}] - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        })
        .init();

    let signals: ThreadSafeSignals = Arc::new(Mutex::new(HashMap::from([
        (
            "shutdown".to_owned(),
            Box::new(false) as Box<dyn Any + Send + Sync>,
        ),
        (
            "started_udp".to_owned(),
            Box::new(false) as Box<dyn Any + Send + Sync>,
        ),
    ])));

    let tcp_thread = tokio::spawn(tcp_client(
        args[1].clone(),
        args[2].clone(),
        args[3].clone(),
        args[4].clone(),
        max_clients,
        signals.clone(),
    ));

    while !signals
        .lock()
        .unwrap()
        .get("started_udp")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(100)).await;
    }

    let udp_heartbeat_thread = tokio::spawn(udp_heartbeat(
        args[1].clone(),
        args[2].clone(),
        args[3].clone(),
        args[4].clone(),
        signals.clone(),
    ));
    tcp_thread.await.unwrap();
    udp_heartbeat_thread.await.unwrap();
}
