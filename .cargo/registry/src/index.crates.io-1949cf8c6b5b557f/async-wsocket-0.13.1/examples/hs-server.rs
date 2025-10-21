// Copyright (c) 2022-2024 Yuki Kishimoto
// Distributed under the MIT software license

use std::net::SocketAddr;

use async_wsocket::prelude::*;
use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    // Bind
    let listener = TcpListener::bind("127.0.0.1:55889").await.unwrap();

    // Launch hidden service
    let local_addr = listener.local_addr().unwrap();
    let service = tor::launch_onion_service("async-wsocket-hs-server-test", local_addr, 80, None)
        .await
        .unwrap();
    println!("{}", service.onion_name().unwrap().to_string());

    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(async move {
            handle_connection(stream, addr).await;
        });
    }
}

async fn handle_connection(raw_stream: TcpStream, _addr: SocketAddr) {
    let stream = async_wsocket::native::accept(raw_stream).await.unwrap();

    let (_tx, mut rx) = stream.split();

    while let Some(msg) = rx.next().await {
        println!("Received message: {msg:?}");
    }
}
