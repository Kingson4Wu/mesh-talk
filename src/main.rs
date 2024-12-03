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

        println!("开始UDP广播...");
        loop {
            let json = serde_json::to_string(&message).unwrap();
            if let Err(e) = socket.send_to(json.as_bytes(), broadcast_addr).await {
                eprintln!("广播消息失败: {}", e);
            }
            tokio::time::sleep(BROADCAST_INTERVAL).await;
        }
    }

    async fn start_udp_discovery(&self) -> std::io::Result<()> {
        let socket = UdpSocket::bind(("0.0.0.0", BROADCAST_PORT)).await?;
        println!("开始监听UDP发现消息在端口 {}...", BROADCAST_PORT);
        
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
                                        eprintln!("连接到节点 {} 失败: {}", peer_addr, e);
                                    }
                                });
                            }
                        }
                    }
                }
                Err(e) => eprintln!("接收UDP消息失败: {}", e),
            }
        }
    }

    async fn connect_to_peer(&self, addr: SocketAddr) -> std::io::Result<()> {
        if self.peers.lock().unwrap().contains_key(&addr) {
            println!("已经连接到节点 {}", addr);
            return Ok(());
        }

        let stream = TcpStream::connect(addr).await?;
        println!("TCP连接已建立到 {}", addr);
        let (reader, writer) = stream.into_split();

        let (tx, mut rx) = mpsc::channel(32);
        self.peers.lock().unwrap().insert(addr, tx);
        println!("已连接到节点: {}", addr);

        let mut writer = BufWriter::new(writer);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                println!("正在发送消息: {}", msg.trim());
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    eprintln!("发送消息失败: {}", e);
                    break;
                }
                if let Err(e) = writer.flush().await {
                    eprintln!("刷新缓冲区失败: {}", e);
                    break;
                }
                println!("消息发送完成");
            }
            println!("消息发送任务结束: {}", addr);
        });

        let node = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        println!("连接关闭: {}", addr);
                        break;
                    }
                    Ok(n) => {
                        println!("收到 {} 字节的原始消息: {}", n, line);
                        if let Err(e) = node.handle_message(line.trim()).await {
                            eprintln!("处理消息失败: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("读取消息失败: {}", e);
                        break;
                    }
                }
            }

            println!("连接断开: {}", addr);
            node.peers.lock().unwrap().remove(&addr);
        });

        Ok(())
    }

    async fn handle_incoming_connection(&self, stream: TcpStream, addr: SocketAddr) -> std::io::Result<()> {
        let (reader, writer) = stream.into_split();
        let (tx, mut rx) = mpsc::channel(32);
        
        self.peers.lock().unwrap().insert(addr, tx);
        println!("接受新连接: {}", addr);

        let mut writer = BufWriter::new(writer);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                println!("正在发送消息: {}", msg.trim());
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    eprintln!("发送消息失败: {}", e);
                    break;
                }
                if let Err(e) = writer.flush().await {
                    eprintln!("刷新缓冲区失败: {}", e);
                    break;
                }
                println!("消息发送完成");
            }
            println!("消息发送任务结束: {}", addr);
        });

        let node = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        println!("连接关闭: {}", addr);
                        break;
                    }
                    Ok(n) => {
                        println!("收到 {} 字节的原始消息: {}", n, line);
                        if let Err(e) = node.handle_message(line.trim()).await {
                            eprintln!("处理消息失败: {}", e);
                        }
                    }
                    Err(e) => {
                        eprintln!("读取消息失败: {}", e);
                        break;
                    }
                }
            }

            println!("连接断开: {}", addr);
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
        println!("准备发送JSON消息: {}", json.trim());

        let peers = {
            let guard = self.peers.lock().unwrap();
            guard.clone()
        };

        if peers.is_empty() {
            println!("当前没有连接的节点");
            return Ok(());
        }

        println!("正在向 {} 个节点发送消息", peers.len());
        for (addr, sender) in peers.iter() {
            match sender.send(json.clone()).await {
                Ok(_) => println!("消息已发送到节点 {}", addr),
                Err(e) => {
                    eprintln!("发送消息到 {} 失败: {}", addr, e);
                    self.peers.lock().unwrap().remove(addr);
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&self, line: &str) -> Result<(), serde_json::Error> {
        println!("尝试解析消息: {}", line);
        match serde_json::from_str(line) {
            Ok(Message::Chat { from, content }) => {
                println!("\n收到来自 {} 的消息: {}", from, content);
                print!("> ");
                std::io::stdout().flush().unwrap();
                Ok(())
            }
            Ok(Message::Discovery { name, port }) => {
                println!("收到来自 {} 的Discovery消息，端口: {}", name, port);
                Ok(())
            }
            Err(e) => {
                eprintln!("解析消息失败: {} (原始消息: {})", e, line);
                Err(e)
            }
        }
    }

    pub async fn run(&self) -> std::io::Result<()> {
        println!("启动节点服务...");
        println!("本地节点: {} (TCP端口: {})", self.name, self.port);

        let listener = TcpListener::bind(("0.0.0.0", self.port)).await?;
        println!("TCP服务已启动在端口 {}", self.port);
        println!("\n开始聊天 (输入 'quit' 退出):");
        print!("> ");
        std::io::stdout().flush().unwrap();

        let node = self.clone();
        tokio::spawn(async move {
            while let Ok((stream, addr)) = listener.accept().await {
                if let Err(e) = node.handle_incoming_connection(stream, addr).await {
                    eprintln!("处理连接失败: {}", e);
                }
            }
        });

        let broadcast_socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
        broadcast_socket.set_broadcast(true)?;
        println!("开始监听UDP发现消息在端口 8888...");

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
                                if peer_addr.port() != node.port {  // 避免连接自己
                                    if !node.peers.lock().unwrap().contains_key(&peer_addr) {  // 只连接新节点
                                        if let Err(e) = node.connect_to_peer(peer_addr).await {
                                            eprintln!("连接节点失败: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("UDP发现服务失败: {}", e);
            }
        }

        println!("开始UDP广播...");
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
                    eprintln!("UDP广播失败: {}", e);
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
                eprintln!("发送消息失败: {}", e);
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
