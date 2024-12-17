use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const BUFFER_SIZE: usize = 2048;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Server listening on 127.0.0.1:8080");

    loop {
        let (mut socket, addr) = listener.accept().await?;
        println!("New connection from: {}", addr);

        tokio::spawn(async move {
            let mut buffer = [0; BUFFER_SIZE];

            match socket.read(&mut buffer).await {
                Ok(bytes_read) => {
                    let received = String::from_utf8_lossy(&buffer[..bytes_read]);
                    println!("Received: {received}");
                    match serde_json::from_str::<Value>(&received) {
                        Ok(json) => {
                            println!("Deserialized JSON: {:#}", json);
                            socket
                                .write_all(r#"{"message": "DIE"}"#.to_string().as_bytes())
                                .await
                                .unwrap();
                        }
                        Err(e) => {
                            eprintln!("Failed to deserialize into JSON: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to read from socket: {:?}", e);
                }
            }
        });
    }
}
