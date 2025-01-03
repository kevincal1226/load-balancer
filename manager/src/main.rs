use log::{debug, error, info, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io::Write;
use std::net::TcpStream;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::{sleep, timeout, Duration};

type ThreadSafeSignals = Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>;
type ThreadSafeServers = Arc<Mutex<HashMap<(String, String), ServerInfo>>>;
type ThreadSafeClientsQueue = Arc<Mutex<VecDeque<(String, String)>>>;
type ThreadSafeClientsMap = Arc<Mutex<HashMap<(String, String), (String, String)>>>;
type ThreadSafeClientsSet = Arc<Mutex<HashSet<(String, String)>>>;

const BUFFER_SIZE: usize = 8192;
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Status {
    Alive,
    Dead,
}

#[derive(Clone, Debug)]
struct ServerInfo {
    time_since_last_heartbeat: Instant,
    curr_clients: ThreadSafeClientsSet,
    max_clients: usize,
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
    debug!("Starting tcp_client thread");
    info!(target:"manager", "Manager started listening on {host}:{port}");

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
                        handle_tcp(
                            signals.clone(),
                            servers.clone(),
                            clients_queue.clone(),
                            all_clients.clone(),
                            &json,
                        );
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
            debug!("Shutdown message received. Sending message to clients and servers");
            signals
                .lock()
                .unwrap()
                .entry("shutdown".to_owned())
                .and_modify(|e| *e.downcast_mut::<bool>().unwrap() = true);

            all_clients
                .lock()
                .unwrap()
                .iter()
                .for_each(|(client_addr, _)| {
                    send_message(
                        Value::from_str(r#"{"message_type": "shutdown"}"#).unwrap(),
                        client_addr.0.clone(),
                        client_addr.1.clone(),
                    )
                    .unwrap_or_default();
                });

            all_clients.lock().unwrap().clear();

            clients_queue
                .lock()
                .unwrap()
                .iter()
                .for_each(|client_addr| {
                    send_message(
                        Value::from_str(r#"{"message_type": "shutdown"}"#).unwrap(),
                        client_addr.0.clone(),
                        client_addr.1.clone(),
                    )
                    .unwrap_or_default();
                });

            clients_queue.lock().unwrap().clear();

            servers.lock().unwrap().iter().for_each(|(server_addr, _)| {
                send_message(
                    Value::from_str(r#"{"message_type": "shutdown"}"#).unwrap(),
                    server_addr.0.clone(),
                    server_addr.1.clone(),
                )
                .unwrap_or_default();
            });
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

            let mut servers_binding = servers.lock().unwrap();
            let server = servers_binding.get_mut(&(host.clone(), port.clone()));

            if server.as_ref().is_some_and(|s| s.status == Status::Alive) {
                // Fault tolerance instance where server re-registers before manager marks as dead
                // Must send clients at the re-registered server to the clients_queue to be
                // redirected

                server
                    .as_ref()
                    .unwrap()
                    .curr_clients
                    .lock()
                    .unwrap()
                    .iter()
                    .for_each(|i| {
                        clients_queue.lock().unwrap().push_back(i.clone());
                        all_clients.lock().unwrap().remove_entry(i);
                    });

                server
                    .as_ref()
                    .unwrap()
                    .curr_clients
                    .lock()
                    .unwrap()
                    .clear();

                info!("Server at port {}:{} re-registered before manager could mark as dead. Adding old clients to clients_queue", host, port);

                return;
            }
            servers_binding.insert(
                (host.clone(), port.clone()),
                ServerInfo {
                    curr_clients: Arc::new(Mutex::new(HashSet::new())),
                    max_clients: max_clients as usize,
                    status: Status::Alive,
                    time_since_last_heartbeat: Instant::now(),
                },
            );

            match send_message(
                Value::from_str(r#"{"message_type": "register_ack"}"#).unwrap(),
                host.clone(),
                port.clone(),
            ) {
                Ok(()) => info!("Successfully registered server at address {address}"),
                Err(e) => {
                    warn!("Warning: {e} on address {address}");
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

            clients_queue
                .lock()
                .unwrap()
                .push_back((client_host.clone(), client_port.clone()));

            info!("Added client at {client_host}:{client_port} to the clients queue");
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
                        .and_modify(|s| {
                            s.curr_clients
                                .lock()
                                .unwrap()
                                .remove(&(client_host.clone(), client_port.clone()));
                        });

                    all_clients
                        .lock()
                        .unwrap()
                        .remove_entry(&(client_host.clone(), client_port.clone()));
                    info!(
                        "Removed client at {client_host}:{client_port} from server address {}:{}",
                        server_addr.0, server_addr.1
                    )
                }

                None => warn!("Warning: {client_host}:{client_port} is not a registered address"),
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
        sleep(Duration::from_millis(100)).await;

        if clients_queue.lock().unwrap().is_empty() || servers.lock().unwrap().is_empty() {
            continue;
        }

        let client = clients_queue.lock().unwrap().front().unwrap().clone();

        if all_clients.lock().unwrap().contains_key(&client) {
            warn!(
                "Client at address {}:{} was processed in queue twice. Second instance ignored",
                client.0, client.1
            );
            clients_queue.lock().unwrap().pop_front();
            continue;
        }

        let mut servers_binding = servers.lock().unwrap();
        let (server_addr, server) = servers_binding
            .iter_mut()
            .min_by(|x, y| {
                let x_val = x.1.curr_clients.lock().unwrap().len() as f64 / x.1.max_clients as f64
                    + (x.1.status == Status::Dead) as i64 as f64;

                let y_val = y.1.curr_clients.lock().unwrap().len() as f64 / y.1.max_clients as f64
                    + (y.1.status == Status::Dead) as i64 as f64;

                x_val.partial_cmp(&y_val).unwrap()
            })
            .unwrap();

        if server.status == Status::Dead
            || server.curr_clients.lock().unwrap().len() >= server.max_clients
        {
            continue;
        }

        let client = clients_queue.lock().unwrap().pop_front().unwrap().clone();
        all_clients
            .lock()
            .unwrap()
            .insert(client.clone(), server_addr.clone());

        server.curr_clients.lock().unwrap().insert(client.clone());

        let message = r#"{"message_type": "redirect", "host": ""#.to_owned()
            + &server_addr.0
            + r#"", "port": ""#
            + &server_addr.1
            + r#""}"#;

        match send_message(
            Value::from_str(&message).unwrap(),
            client.0.clone(),
            client.1.clone(),
        ) {
            Ok(()) => info!(
                "Sent redirect message to client at {}:{}",
                client.0, client.1
            ),
            Err(e) => {
                error!(
                    "Error: {e} while trying to send message to client at {}:{}",
                    client.0.clone(),
                    client.1.clone()
                );
                server.curr_clients.lock().unwrap().remove(&client);
            }
        }
    }
    debug!("Shutting down assign_clients_to_server thread");
}

async fn udp_client(
    host: String,
    port: String,
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
) {
    debug!("Starting udp_client thread");

    let addr = format!("{host}:{port}");
    let sock = UdpSocket::bind(addr).await.unwrap();

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(100)).await;

        let mut buffer = [0; BUFFER_SIZE];

        match timeout(Duration::from_secs(2), sock.recv_from(&mut buffer)).await {
            Ok(Ok((bytes_read, _))) => {
                let received = String::from_utf8_lossy(&buffer[..bytes_read]);
                info!("Received UDP message:\n{received}");

                match serde_json::from_str::<Value>(&received) {
                    Ok(json) => {
                        handle_udp(&json, servers.clone());
                    }
                    Err(e) => error!(
                        "Failed to deserialize UDP messag UDP message into JSON: {:?}",
                        e
                    ),
                }
            }
            Err(_) => (), // This is the case where a timeout occurs, doesn't really matter. But,
            // we don't want to block the TCP thread from listening indefinitely if we never
            // receive a UDP message
            Ok(Err(e)) => error!("Failed to read from udp socket: {:?}", e),
        }
    }

    debug!("Shutting down udp_client thread");
}

fn handle_udp(message: &Value, servers: ThreadSafeServers) {
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
        "heartbeat" => {
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

            let mut servers_binding = servers.lock().unwrap();
            let server = servers_binding.get_mut(&(host.clone(), port.clone()));

            match server.is_some() && server.as_ref().unwrap().status == Status::Alive {
                true => {
                    server.unwrap().time_since_last_heartbeat = Instant::now();
                }
                false => warn!(
                    "Heartbeat received from unregistered/dead worker at address {host}:{port}"
                ),
            }
        }
        _ => {
            let invalid = message_type.as_str().unwrap();
            warn!("Unrecognized message type {invalid} received");
        }
    }
}

async fn fault_tolerance(
    signals: ThreadSafeSignals,
    servers: ThreadSafeServers,
    clients_queue: ThreadSafeClientsQueue,
    all_clients: ThreadSafeClientsMap,
) {
    debug!("Starting fault_tolerance thread");

    while !signals
        .lock()
        .unwrap()
        .get("shutdown")
        .unwrap()
        .downcast_ref::<bool>()
        .unwrap()
    {
        sleep(Duration::from_millis(100)).await;

        servers
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|(_, v)| {
                v.status == Status::Alive
                    && v.time_since_last_heartbeat.elapsed() >= HEARTBEAT_TIMEOUT
            })
            .for_each(|(server_addr, server)| {
                server.status = Status::Dead;

                server.curr_clients.lock().unwrap().iter().for_each(|i| {
                    clients_queue.lock().unwrap().push_back(i.clone());
                    all_clients.lock().unwrap().remove_entry(i);
                });

                server.curr_clients.lock().unwrap().clear();

                info!(
                    "Server at address {}:{} died. Moving clients into clients_queue",
                    server_addr.0, server_addr.1
                );
            });
    }
    debug!("Shutting down fault_tolerance thread");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        println!("Usage: RUST_LOG=[debug|info|warn|error] target/release/manager [host] [port]");
        return;
    }

    let log_file = std::fs::File::create("manager.log").expect("Could not create manager log file");
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

    let udp_client_thread = tokio::spawn(udp_client(
        args[1].clone(),
        args[2].clone(),
        signals.clone(),
        servers.clone(),
    ));

    let fault_tolerance_thread = tokio::spawn(fault_tolerance(
        signals.clone(),
        servers.clone(),
        clients_queue.clone(),
        all_clients.clone(),
    ));

    tcp_thread.await.unwrap();
    assign_clients_thread.await.unwrap();
    udp_client_thread.await.unwrap();
    fault_tolerance_thread.await.unwrap();
}
