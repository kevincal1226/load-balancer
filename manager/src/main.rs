use log::{debug, error, info, warn};
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
type ThreadSafeServers = Arc<Mutex<HashMap<(String, String), ServerInfo>>>;
type ThreadSafeClientsQueue = Arc<Mutex<VecDeque<(String, String)>>>;
type ThreadSafeClientsMap = Arc<Mutex<HashMap<(String, String), (String, String)>>>;

const BUFFER_SIZE: usize = 8192;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Status {
    Alive,
    Dead,
}

#[derive(Clone, Debug)]
struct ServerInfo {
    host: String,
    port: String,
    time_since_last_heartbeat: Instant,
    num_clients: u64,
    max_clients: u64,
    status: Status,
}

fn send_message(message: Value, host: String, port: String) -> Result<(), std::io::Error> {
    let addr = format!("{host}:{port}");
    match TcpStream::connect(addr) {
        Ok(mut socket) => socket.write_all(message.to_string().as_bytes()),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
    }
}

async fn tcp_client(
    host: String,
    port: String,
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients_queue: ThreadSafeClientsQueue,
    all_clients: ThreadSafeClientsMap,
) {
    let listener = TcpListener::bind(format!("{host}:{port}")).await.unwrap();
    info!(target:"manager", "Manager started listening on {host}:{port}");

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_micros(1)).await;

        let (mut socket, addr) = listener.accept().await.unwrap();
        debug!("New connection from: {}", addr);

        let mut buffer = [0; BUFFER_SIZE];

        match socket.read(&mut buffer).await {
            Ok(bytes_read) => {
                let received = String::from_utf8_lossy(&buffer[..bytes_read]);
                info!("Received message: {received}");
                match serde_json::from_str::<Value>(&received) {
                    Ok(json) => {
                        debug!("Deserialized JSON: {:#}", json);
                        handle_tcp(
                            signals.clone(),
                            servers.clone(),
                            clients_queue.clone(),
                            all_clients.clone(),
                            &json,
                        );
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
    debug!("Shutting down tcp_client thread");
}

fn handle_tcp(
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients_queue: ThreadSafeClientsQueue,
    all_clients: ThreadSafeClientsMap,
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
            if servers
                .lock()
                .unwrap()
                .get(&(host.clone(), port.clone()))
                .is_some()
                && servers
                    .lock()
                    .unwrap()
                    .get(&(host.clone(), port.clone()))
                    .unwrap()
                    .status
                    == Status::Alive
            {
                warn!("Addres {address} already registered");
                return;
            }
            //TODO: handle fault tolerance
            servers.lock().unwrap().insert(
                (host.clone(), port.clone()),
                ServerInfo {
                    host: host.clone(),
                    port: port.clone(),
                    num_clients: 0,
                    max_clients,
                    status: Status::Alive,
                    time_since_last_heartbeat: Instant::now(),
                },
            );

            match send_message(
                Value::from(r#"{"message_type": "ack"}"#),
                host.clone(),
                port.clone(),
            ) {
                Ok(()) => info!("Successfully registered server at address {address}"),
                Err(e) => {
                    warn!("Error: {e} on address {address}");
                    servers
                        .lock()
                        .unwrap()
                        .entry((host.clone(), port.clone()))
                        .and_modify(|e| e.status = Status::Dead);
                }
            }
        }
        "client_connect" => {
            debug!("Client connection message received");

            let client_host = message
                .get("host")
                .expect("Host argument not found")
                .as_str()
                .expect("Host argument is not a string")
                .to_owned();

            let client_port = message
                .get("port")
                .expect("Port argument not found")
                .as_str()
                .expect("Port argument is not a string")
                .to_owned();

            match all_clients
                .lock()
                .unwrap()
                .contains_key(&(client_host.clone(), client_port.clone()))
            {
                true => warn!("Client at {client_host}:{client_port} is already in use"),
                false => clients_queue
                    .lock()
                    .unwrap()
                    .push_back((client_host.clone(), client_port.clone())),
            }
        }
        "client_disconnect" => {
            debug!("Client disconnect message received");

            let client_host = message
                .get("host")
                .expect("Host argument not found")
                .as_str()
                .expect("Host argument is not a string")
                .to_owned();

            let client_port = message
                .get("port")
                .expect("Port argument not found")
                .as_str()
                .expect("Port argument is not a string")
                .to_owned();

            let res = all_clients
                .lock()
                .unwrap()
                .remove_entry(&(client_host.clone(), client_port.clone()));

            match res {
                Some((_, server_addr)) => {
                    servers
                        .lock()
                        .unwrap()
                        .entry(server_addr.clone())
                        .and_modify(|s| s.num_clients -= 1);
                    info!(
                        "Removed client at {client_host}:{client_port} from server address {}:{}",
                        server_addr.0, server_addr.1
                    )
                }

                None => warn!("Error: {client_host}:{client_port} is not a registered address"),
            }
        }
        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid} received");
        }
    }
}

async fn assign_clients_to_server(
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients_queue: ThreadSafeClientsQueue,
    all_clients: ThreadSafeClientsMap,
) {
    debug!("Starting assign_clients_to_server thread");

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(1)).await;

        if clients_queue.lock().unwrap().is_empty() {
            continue;
        }
    }
    debug!("Shutting down assign_clients_to_server thread");
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        panic!("Usage: RUST_LOG=[debug|info|warn|error] target/release/manager [host] [port]");
    }
    let signals: ThreadSafeSignals = Arc::new(Mutex::new(HashMap::from([(
        "shutdown".to_owned(),
        Box::new(false) as Box<dyn Any + Send + Sync>,
    )])));
    let servers: ThreadSafeServers = Arc::new(Mutex::new(HashMap::new()));

    let clients_queue: ThreadSafeClientsQueue = Arc::new(Mutex::new(VecDeque::new()));

    // All the clients currently connected to a server.
    // Does not include those waiting in the clients_queue.
    let all_clients: ThreadSafeClientsMap = Arc::new(Mutex::new(HashMap::new()));

    let tcp_thread = tokio::spawn(tcp_client(
        args[1].clone(),
        args[2].clone(),
        signals.clone(),
        servers.clone(),
        clients_queue.clone(),
        all_clients.clone(),
    ));

    let assign_clients_thread = tokio::spawn(assign_clients_to_server(
        signals.clone(),
        servers.clone(),
        clients_queue.clone(),
        all_clients.clone(),
    ));
    //TODO: Add UDP socket for heatbeats

    tcp_thread.await.unwrap();
    assign_clients_thread.await.unwrap();
}
