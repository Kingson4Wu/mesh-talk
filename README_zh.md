# Mesh-Talk

Mesh-Talk 是一个使用 Rust 编写的点对点网状网络聊天应用程序，允许用户以去中心化的方式进行通信。

## 功能特点

- 无需中央服务器的点对点通信
- 使用 UDP 广播自动发现对等节点
- 连接节点间的实时消息传递
- 命令行界面
- 使用 Tokio 构建的异步 Rust 应用

## 环境要求

- Rust 2021 版本或更高
- Cargo 包管理器

## 安装方法

克隆仓库并构建项目：

```bash
git clone https://github.com/yourusername/mesh-talk.git
cd mesh-talk
cargo build --release
```

## 使用方法

使用您想要的名称和端口运行应用程序：

```bash
cargo run -- --name 您的名字 --port 8000
```

### 命令行参数

- `--name` 或 `-n`：您在聊天中显示的名称
- `--port` 或 `-p`：监听传入连接的 TCP 端口

## 工作原理

1. 应用程序创建一个网状网络，每个节点都可以直接与其他节点通信
2. 使用 UDP 广播在端口 8888 上进行节点发现
3. 节点之间建立 TCP 连接以确保消息可靠传递
4. 消息会广播到网络中所有已连接的节点

## 技术细节

- 使用 Tokio 实现异步运行时和网络通信
- 实现 UDP 广播用于节点发现
- 使用 TCP 实现可靠的点对点通信
- 使用 JSON 进行消息编码
- 使用 Arc 和 Mutex 实现线程安全的节点管理

## 依赖项

- tokio：异步运行时和网络功能
- serde：序列化框架
- serde_json：JSON 序列化
- clap：命令行参数解析

## 许可证

本项目采用 MIT 许可证开源。
