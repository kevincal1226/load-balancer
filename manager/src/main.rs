use env_logger;
use log::{debug, error, info, max_level, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, UdpSocket};
use tokio::time::{sleep, Duration};

type ThreadSafeSignals = Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>;
type ThreadSafeServers = Arc<Mutex<HashMap<String, ServerInfo>>>;
type ThreadSafeClientsQueue = Arc<Mutex<VecDeque<String>>>;
type ThreadSafeClientsSet = Arc<Mutex<HashSet<String>>>;

const BUFFER_SIZE: usize = 8192;

#[derive(Clone, Eq, PartialEq)]
enum Status {
    ALIVE,
    DEAD,
}

#[derive(Clone, Eq, PartialEq)]
struct ServerInfo {
    host: String,
    port: String,
    time_since_last_heartbeat: Instant,
    num_clients: u64,
    max_clients: u64,
    status: Status,
}

async fn tcp_client(
    host: String,
    port: String,
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients: ThreadSafeClientsQueue,
    //handler: impl Fn(ThreadSafeSignals, ThreadSafeServers, ThreadSafeClients, &Value),
) {
    let listener = TcpListener::bind(format!("{host}:{port}")).await.unwrap();
    info!(target:"manager", "Manager started listening on {host}:{port}");

    loop {
        sleep(Duration::from_micros(1)).await;
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
                        handle_tcp(signals.clone(), servers.clone(), clients.clone(), &json);
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

fn send_message(message: Value, host: String, port: String) -> Result<(), std::io::Error> {
    let addr = format!("{host}:{port}");
    match TcpStream::connect(addr) {
        Ok(mut socket) => socket.write_all(message.to_string().as_bytes()),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    }
}

fn handle_tcp(
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients: ThreadSafeClientsQueue,
    message: &Value,
) {
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
            debug!("Shutdown message received");
            *signals.lock().unwrap().get_mut("shutdown").unwrap() = Box::new(true);
        }
        "register" => {
            debug!("Register message received");
            let host = message
                .get("host")
                .expect("Host argument not found")
                .as_str()
                .expect("Host argument is not String")
                .to_owned();

            let port = message
                .get("port")
                .expect("Port argument not found")
                .as_str()
                .expect("Port argument is not String")
                .to_owned();

            let max_clients = message
                .get("max_clients")
                .expect("Max_clients argument not found")
                .as_u64()
                .expect("Max_clients is not u64");
            if max_clients < 1 {
                error!("Invalid max_clients: {max_clients} must be greater than 0");
                return;
            }

            let address = format!("{host}:{port}");
            if servers.lock().unwrap().get(&address).is_some()
                && servers.lock().unwrap().get(&address).unwrap().status == Status::ALIVE
            {
                warn!("Addres {address} already registered");
                return;
            }
            //TODO: handle fault tolerance
            servers.lock().unwrap().insert(
                address.clone(),
                ServerInfo {
                    host: host.clone(),
                    port: port.clone(),
                    num_clients: 0,
                    max_clients,
                    status: Status::ALIVE,
                    time_since_last_heartbeat: Instant::now(),
                },
            );

            match send_message(Value::from(r#"{"message_type": "ack"}"#), host, port) {
                Ok(()) => info!("Successfully registered server at address {address}"),
                Err(e) => {
                    warn!("Error: {e} on address {address}");
                    servers
                        .lock()
                        .unwrap()
                        .entry(address.clone())
                        .and_modify(|e| e.status = Status::DEAD);
                }
            }
        }
        "client_connect" => {
            debug!("Client connection message received");
        }
        "client_disconnect" => {
            debug!("Client disconnect message received");
        }
        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid} received");
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        panic!("Usage: RUST_LOG=[debug|info|warn|error] target/release/manager [host] [port]");
    }
    let mut signals: HashMap<String, Box<dyn Any + Send + Sync>> = HashMap::new();
    signals.insert("shutdown".to_owned(), Box::new(false));
    let mut servers: ThreadSafeServers = Arc::new(Mutex::new(HashMap::new()));
    let mut clients_queue: ThreadSafeClientsQueue = Arc::new(Mutex::new(VecDeque::new()));
    let tcp_thread = tokio::spawn(tcp_client(
        args[1].clone(),
        args[2].clone(),
        Arc::new(Mutex::new(signals)),
        servers.clone(),
        clients_queue.clone(),
    ));

    tcp_thread.await.unwrap();
}
