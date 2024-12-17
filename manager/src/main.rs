use env_logger;
use log::{debug, error, info, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::{sleep, Duration};

const BUFFER_SIZE: usize = 8192;

#[derive(Default)]
struct Manager {
    signals: Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>,
}

async fn tcp_client(signals: Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>) {
    let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
    info!(target:"manager", "Manager started listening on 127.0.0.1:8080");

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
                info!("Received: {received}");
                match serde_json::from_str::<Value>(&received) {
                    Ok(json) => {
                        debug!("Deserialized JSON: {:#}", json);
                        socket
                            .write_all(r#"{"message": "DIE"}"#.to_string().as_bytes())
                            .await
                            .unwrap();
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

impl Manager {
    pub fn new() -> Manager {
        let mut signals: HashMap<String, Box<dyn Any + Send + Sync>> = HashMap::new();
        signals.insert("shutdown".to_owned(), Box::new(false));
        Manager {
            signals: Arc::new(Mutex::new(signals)),
        }
    }
    async fn start(&self) {
        let e = tokio::spawn(tcp_client(self.signals.clone()));
        sleep(Duration::from_secs(2)).await;
        *self.signals.lock().unwrap().get_mut("shutdown").unwrap() = Box::new(true);
        println!("Set to true");
        e.await.unwrap();
        println!("Done");
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let manager: Manager = Manager::new();
    manager.start().await;
}
