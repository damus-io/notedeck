// Copyright (c) 2022-2024 Yuki Kishimoto
// Distributed under the MIT software license

//! Socks

use std::net::SocketAddr;

use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;
use tokio_socks::IntoTargetAddr;

pub(crate) struct TcpSocks5Stream;

impl TcpSocks5Stream {
    #[inline]
    pub async fn connect<'a>(
        proxy: SocketAddr,
        dest: impl IntoTargetAddr<'a>,
    ) -> Result<TcpStream, tokio_socks::Error> {
        Ok(Socks5Stream::connect(proxy, dest).await?.into_inner())
    }
}
