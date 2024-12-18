use env_logger;
use log::{debug, error, info, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, UdpSocket};
use tokio::time::{sleep, Duration};

type ThreadSafeSignals = Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>;

const BUFFER_SIZE: usize = 8192;

#[derive(Default)]
struct Manager {
    signals: ThreadSafeSignals,
}

async fn tcp_client(signals: ThreadSafeSignals, handler: impl Fn(ThreadSafeSignals, &Value)) {
    let listener = TcpListener::bind("localhost:8080").await.unwrap();
    info!(target:"manager", "Manager started listening on localhost:8080");

    loop {
        let shutdown = {
            let sig = signals.lock().unwrap();
            sig.get("shutdown")
                .and_then(|s| s.downcast_ref::<bool>())
                .copied()
                .unwrap()
        };
        if shutdown {
            println!("Exiting");
            return;
        }
        let (mut socket, addr) = listener.accept().await.unwrap();
        info!("New connection from: {}", addr);

        let mut buffer = [0; BUFFER_SIZE];

        match socket.read(&mut buffer).await {
            Ok(bytes_read) => {
                let received = String::from_utf8_lossy(&buffer[..bytes_read]);
                info!("Received message: {received}");
                match serde_json::from_str::<Value>(&received) {
                    Ok(json) => {
                        debug!("Deserialized JSON: {:#}", json);
                        handler(signals.clone(), &json);
                    }
                    Err(e) => {
                        error!("Failed to deserialize into JSON: {:?}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to read from socket: {:?}", e);
            }
        }
    }
}

fn send_message(message: Value, host: String, port: String) {
    let addr = format!("{host}:{port}");
    let mut socket = TcpStream::connect(addr).unwrap();
    socket.write_all(message.to_string().as_bytes()).unwrap();
}

fn handle_tcp(signals: Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>, message: &Value) {
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
            debug!("Shutdown message received.");
            *signals.lock().unwrap().get_mut("shutdown").unwrap() = Box::new(true);
        }
        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid}");
        }
    }
}

impl Manager {
    pub fn new() -> Manager {
        let mut signals: HashMap<String, Box<dyn Any + Send + Sync>> = HashMap::new();
        signals.insert("shutdown".to_owned(), Box::new(false));
        Manager {
            signals: Arc::new(Mutex::new(signals)),
        }
    }
    async fn start(&self) {
        let tcp_thread = tokio::spawn(tcp_client(self.signals.clone(), handle_tcp));
        tcp_thread.await.unwrap();
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let manager: Manager = Manager::new();
    manager.start().await;
}
