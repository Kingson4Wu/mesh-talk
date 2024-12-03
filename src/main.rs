use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::io::{self, Write};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tokio::time::Duration;
use serde::{Serialize, Deserialize};
use clap::Parser;

const BROADCAST_PORT: u16 = 8888;
const BROADCAST_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    name: String,

    #[arg(short, long)]
    port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
enum Message {
    Discovery {
        name: String,
        port: u16,
    },
    Chat {
        from: String,
        content: String,
    },
}

struct Node {
    name: String,
    port: u16,
    peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
}

impl Node {
    fn new(name: String, port: u16) -> Self {
        Self {
            name,
            port,
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[allow(dead_code)]
    fn _use_unused_methods(&self) {
        // This method will never be called, but prevents dead code warnings
        let _ = self.start_udp_broadcast();
        let _ = self.start_udp_discovery();
    }

    async fn start_udp_broadcast(&self) -> std::io::Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.set_broadcast(true)?;

        let broadcast_addr = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
            BROADCAST_PORT,
        );

        let message = Message::Discovery {
            name: self.name.clone(),
            port: self.port,
        };

        println!("Starting UDP broadcast...");
        loop {
            let json = serde_json::to_string(&message).unwrap();
            if let Err(e) = socket.send_to(json.as_bytes(), broadcast_addr).await {
                eprintln!("Broadcast message failed: {}", e);
            }
            tokio::time::sleep(BROADCAST_INTERVAL).await;
        }
    }

    async fn start_udp_discovery(&self) -> std::io::Result<()> {
        let socket = UdpSocket::bind(("0.0.0.0", BROADCAST_PORT)).await?;
        println!("Starting to listen for UDP discovery messages on port {}...", BROADCAST_PORT);
        
        let mut buf = [0u8; 1024];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((n, addr)) => {
                    if let Ok(json) = String::from_utf8(buf[..n].to_vec()) {
                        if let Ok(Message::Discovery { name: _, port }) = serde_json::from_str(&json) {
                            if port != self.port {  
                                let peer_addr = SocketAddr::new(addr.ip(), port);
                                let node = self.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = node.connect_to_peer(peer_addr).await {
                                        eprintln!("Failed to connect to node {}: {}", peer_addr, e);
                                    }
                                });
                            }
                        }
                    }
                }
                Err(e) => eprintln!("Failed to receive UDP message: {}", e),
            }
        }
    }

    async fn connect_to_peer(&self, addr: SocketAddr) -> std::io::Result<()> {
        if self.peers.lock().unwrap().contains_key(&addr) {
            println!("Already connected to node {}", addr);
            return Ok(());
        }

        let stream = TcpStream::connect(addr).await?;
        println!("TCP connection established to {}", addr);
        let (reader, writer) = stream.into_split();

        let (tx, mut rx) = mpsc::channel(32);
        self.peers.lock().unwrap().insert(addr, tx);
        println!("Connected to node: {}", addr);

        let mut writer = BufWriter::new(writer);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                println!("Sending message: {}", msg.trim());
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    eprintln!("Failed to send message: {}", e);
                    break;
                }
                if let Err(e) = writer.flush().await {
                    eprintln!("Failed to flush buffer: {}", e);
                    break;
                }
                println!("Message sent successfully");
            }
            println!("Message sending task ended: {}", addr);
        });

        let node = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        println!("Connection closed: {}", addr);
                        break;
                    }
                    Ok(n) => {
                        println!("Received {} bytes of raw message: {}", n, line);
                        if let Err(e) = node.handle_message(line.trim()).await {
                            eprintln!("Failed to process message: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read message: {}", e);
                        break;
                    }
                }
            }

            println!("Connection disconnected: {}", addr);
            node.peers.lock().unwrap().remove(&addr);
        });

        Ok(())
    }

    async fn handle_incoming_connection(&self, stream: TcpStream, addr: SocketAddr) -> std::io::Result<()> {
        let (reader, writer) = stream.into_split();
        let (tx, mut rx) = mpsc::channel(32);
        
        self.peers.lock().unwrap().insert(addr, tx);
        println!("Accepting new connection: {}", addr);

        let mut writer = BufWriter::new(writer);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                println!("Sending message: {}", msg.trim());
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    eprintln!("Failed to send message: {}", e);
                    break;
                }
                if let Err(e) = writer.flush().await {
                    eprintln!("Failed to flush buffer: {}", e);
                    break;
                }
                println!("Message sent successfully");
            }
            println!("Message sending task ended: {}", addr);
        });

        let node = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        println!("Connection closed: {}", addr);
                        break;
                    }
                    Ok(n) => {
                        println!("Received {} bytes of raw message: {}", n, line);
                        if let Err(e) = node.handle_message(line.trim()).await {
                            eprintln!("Failed to process message: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read message: {}", e);
                        break;
                    }
                }
            }

            println!("Connection disconnected: {}", addr);
            node.peers.lock().unwrap().remove(&addr);
        });

        Ok(())
    }

    async fn broadcast_message(&self, content: String) -> std::io::Result<()> {
        let message = Message::Chat {
            from: self.name.clone(),
            content,
        };
        let json = serde_json::to_string(&message).unwrap() + "\n";  
        println!("Preparing to send JSON message: {}", json.trim());

        let peers = {
            let guard = self.peers.lock().unwrap();
            guard.clone()
        };

        if peers.is_empty() {
            println!("No connected nodes currently");
            return Ok(());
        }

        println!("Sending message to {} nodes", peers.len());
        for (addr, sender) in peers.iter() {
            match sender.send(json.clone()).await {
                Ok(_) => println!("Message sent to node {}", addr),
                Err(e) => {
                    eprintln!("Failed to send message to {}: {}", addr, e);
                    self.peers.lock().unwrap().remove(addr);
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&self, line: &str) -> Result<(), serde_json::Error> {
        println!("Trying to parse message: {}", line);
        match serde_json::from_str(line) {
            Ok(Message::Chat { from, content }) => {
                println!("\nReceived message from {}: {}", from, content);
                print!("> ");
                std::io::stdout().flush().unwrap();
                Ok(())
            }
            Ok(Message::Discovery { name, port }) => {
                println!("Received Discovery message from {}, port: {}", name, port);
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse message: {} (raw message: {})", e, line);
                Err(e)
            }
        }
    }

    pub async fn run(&self) -> std::io::Result<()> {
        println!("Starting node service...");
        println!("Local node: {} (TCP port: {})", self.name, self.port);

        let listener = TcpListener::bind(("0.0.0.0", self.port)).await?;
        println!("TCP service started on port {}", self.port);
        println!("\nStart chatting (type 'quit' to exit):");
        print!("> ");
        std::io::stdout().flush().unwrap();

        let node = self.clone();
        tokio::spawn(async move {
            while let Ok((stream, addr)) = listener.accept().await {
                if let Err(e) = node.handle_incoming_connection(stream, addr).await {
                    eprintln!("Failed to handle connection: {}", e);
                }
            }
        });

        let broadcast_socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
        broadcast_socket.set_broadcast(true)?;
        println!("Starting to listen for UDP discovery messages on port 8888...");

        let discovery_socket = UdpSocket::bind(("0.0.0.0", 8888)).await;
        match discovery_socket {
            Ok(socket) => {
                let node = self.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok((len, addr)) = socket.recv_from(&mut buf).await {
                        if let Ok(msg) = String::from_utf8(buf[..len].to_vec()) {
                            if let Ok(Message::Discovery { port, name: _ }) = serde_json::from_str(&msg) {
                                let peer_addr = SocketAddr::new(addr.ip(), port);
                                if peer_addr.port() != node.port {  // Avoid connecting to self
                                    if !node.peers.lock().unwrap().contains_key(&peer_addr) {  // Only connect to new nodes
                                        if let Err(e) = node.connect_to_peer(peer_addr).await {
                                            eprintln!("Failed to connect to node: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("UDP discovery service failed: {}", e);
            }
        }

        println!("Starting UDP broadcast...");
        let node = self.clone();
        tokio::spawn(async move {
            let message = Message::Discovery {
                name: node.name.clone(),
                port: node.port,
            };
            let json = serde_json::to_string(&message).unwrap();
            let broadcast_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)), BROADCAST_PORT);
            
            loop {
                if let Err(e) = broadcast_socket.send_to(json.as_bytes(), broadcast_addr).await {
                    eprintln!("UDP broadcast failed: {}", e);
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        let mut input = String::new();
        let stdin = io::stdin();
        loop {
            input.clear();
            stdin.read_line(&mut input)?;
            let message = input.trim().to_string();
            
            if message == "quit" {
                break;
            }

            if let Err(e) = self.broadcast_message(message).await {
                eprintln!("Failed to send message: {}", e);
            }
            print!("> ");
            std::io::stdout().flush().unwrap();
        }

        Ok(())
    }
}

impl Clone for Node {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            port: self.port,
            peers: Arc::clone(&self.peers),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let node = Node::new(args.name, args.port);
    node.run().await
}
