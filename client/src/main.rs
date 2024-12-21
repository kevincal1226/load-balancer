use log::{debug, error, info, warn};
use serde_json::Value;
use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, UdpSocket};
use tokio::time::{sleep, timeout, Duration};

type ThreadSafeSignals = Arc<Mutex<HashMap<String, Box<dyn Any + Send + Sync>>>>;

fn main() {
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
}
