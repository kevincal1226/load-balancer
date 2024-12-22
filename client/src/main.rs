use log::{debug, error, info, warn};
use rand::prelude::*;
use serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::net::TcpStream;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
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

async fn tcp_client(client_host: String, client_port: String, signals: ThreadSafeSignals) {
    let listener = TcpListener::bind(format!("{client_host}:{client_port}"))
        .await
        .unwrap();

    debug!("Starting tcp_client thread");
    info!("Client started listening on {client_host}:{client_port}");

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

        "redirect" => {
            let redirect_host = message
                .get("host")
                .expect("Host argment not found")
                .to_string();
            let redirect_port = message
                .get("port")
                .expect("Port argument not found")
                .to_string();
            info!(
                "Received a message from the manager. Redirecting to {}:{}...",
                redirect_host, redirect_port
            );
        }

        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid} received");
        }
    }
}

async fn send_connect_and_disconnect_messages(
    manager_host: String,
    manager_port: String,
    client_host: String,
    client_port: String,
    signals: ThreadSafeSignals,
) {
    debug!("Starting send_connect_and_disconnect_messages thread");

    let connect_message = Value::from_str(
        &(r#"{"message_type": "client_connect", "host": ""#.to_owned()
            + &client_host
            + r#"", "port": ""#
            + &client_port
            + r#""}"#),
    )
    .unwrap();

    let disconnect_message = Value::from_str(
        &(r#"{"message_type": "client_disconnect", "host": ""#.to_owned()
            + &client_host
            + r#"", "port": ""#
            + &client_port
            + r#""}"#),
    )
    .unwrap();

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_secs(random::<u64>() % 2 + 1)).await;

        match send_message(
            connect_message.clone(),
            manager_host.clone(),
            manager_port.clone(),
        ) {
            Ok(()) => info!("Sent client_connect message"),
            Err(e) => {
                error!("Error occurred while sending client_conenct message: {e}. Assuming manager died. Setting shutdown flag");
                send_message(
                    Value::from_str(r#"{"message_type": "shutdown"}"#).unwrap(),
                    client_host.clone(),
                    client_port.clone(),
                )
                .unwrap_or_default();
            }
        }

        sleep(Duration::from_secs(random::<u64>() % 9 + 1)).await;
        match send_message(
            disconnect_message.clone(),
            manager_host.clone(),
            manager_port.clone(),
        ) {
            Ok(()) => info!("Sent client_disconnect message"),
            Err(e) => {
                error!("Error occurred while ending client_disconenct message: {e}. Assuming manager died. Setting shutdown flag");
                send_message(
                    Value::from_str(r#"{"message_type": "shutdown"}"#).unwrap(),
                    client_host.clone(),
                    client_port.clone(),
                )
                .unwrap_or_default();
            }
        }
    }

    debug!("Shutting down send_connect_and_disconnect_messages thread");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 5 {
        println!("Usage: RUST_LOG=[debug|info|warn|error] target/release/client [manager_host] [manager_port] [client_host] [client_port]");
        return;
    }

    let log_file = std::fs::File::create(format!("client_{}_{}.log", args[3], args[4]))
        .expect("Could not create client log file");
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

    let signals = Arc::new(Mutex::new(HashMap::from([(
        "shutdown".to_owned(),
        Box::new(false) as Box<dyn Any + Send + Sync>,
    )])));

    let tcp_thread = tokio::spawn(tcp_client(
        args[3].clone(),
        args[4].clone(),
        signals.clone(),
    ));

    let messages_thread = tokio::spawn(send_connect_and_disconnect_messages(
        args[1].clone(),
        args[2].clone(),
        args[3].clone(),
        args[4].clone(),
        signals.clone(),
    ));

    tcp_thread.await.unwrap();
    messages_thread.await.unwrap();
}
