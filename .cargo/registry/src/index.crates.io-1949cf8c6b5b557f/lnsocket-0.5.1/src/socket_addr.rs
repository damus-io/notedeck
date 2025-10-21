use crate::ln::msgs::DecodeError;
use crate::util::{
    base32,
    ser::{Hostname, Readable, Writeable, Writer},
};
use std::fmt::Display;
use std::io::{self, Read};
use std::str::FromStr;

/// An address which can be used to connect to a remote peer.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum SocketAddress {
    /// An IPv4 address and port on which the peer is listening.
    TcpIpV4 {
        /// The 4-byte IPv4 address
        addr: [u8; 4],
        /// The port on which the node is listening
        port: u16,
    },
    /// An IPv6 address and port on which the peer is listening.
    TcpIpV6 {
        /// The 16-byte IPv6 address
        addr: [u8; 16],
        /// The port on which the node is listening
        port: u16,
    },
    /// An old-style Tor onion address/port on which the peer is listening.
    ///
    /// This field is deprecated and the Tor network generally no longer supports V2 Onion
    /// addresses. Thus, the details are not parsed here.
    OnionV2([u8; 12]),
    /// A new-style Tor onion address/port on which the peer is listening.
    ///
    /// To create the human-readable "hostname", concatenate the ED25519 pubkey, checksum, and version,
    /// wrap as base32 and append ".onion".
    OnionV3 {
        /// The ed25519 long-term public key of the peer
        ed25519_pubkey: [u8; 32],
        /// The checksum of the pubkey and version, as included in the onion address
        checksum: u16,
        /// The version byte, as defined by the Tor Onion v3 spec.
        version: u8,
        /// The port on which the node is listening
        port: u16,
    },
    /// A hostname/port on which the peer is listening.
    Hostname {
        /// The hostname on which the node is listening.
        hostname: Hostname,
        /// The port on which the node is listening.
        port: u16,
    },
}
impl SocketAddress {
    /// The maximum length of any address descriptor, not including the 1-byte type.
    /// This maximum length is reached by a hostname address descriptor:
    /// a hostname with a maximum length of 255, its 1-byte length and a 2-byte port.
    pub const MAX_LEN: u16 = 258;

    pub fn is_tor(&self) -> bool {
        match self {
            SocketAddress::TcpIpV4 { .. } => false,
            SocketAddress::TcpIpV6 { .. } => false,
            SocketAddress::OnionV2(_) => true,
            SocketAddress::OnionV3 { .. } => true,
            SocketAddress::Hostname { .. } => false,
        }
    }
}

impl Writeable for SocketAddress {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        match self {
            SocketAddress::TcpIpV4 { addr, port } => {
                1u8.write(writer)?;
                addr.write(writer)?;
                port.write(writer)?;
            }
            SocketAddress::TcpIpV6 { addr, port } => {
                2u8.write(writer)?;
                addr.write(writer)?;
                port.write(writer)?;
            }
            SocketAddress::OnionV2(bytes) => {
                3u8.write(writer)?;
                bytes.write(writer)?;
            }
            SocketAddress::OnionV3 {
                ed25519_pubkey,
                checksum,
                version,
                port,
            } => {
                4u8.write(writer)?;
                ed25519_pubkey.write(writer)?;
                checksum.write(writer)?;
                version.write(writer)?;
                port.write(writer)?;
            }
            SocketAddress::Hostname { hostname, port } => {
                5u8.write(writer)?;
                hostname.write(writer)?;
                port.write(writer)?;
            }
        }
        Ok(())
    }
}

impl Readable for Result<SocketAddress, u8> {
    fn read<R: Read>(
        reader: &mut R,
    ) -> Result<Result<SocketAddress, u8>, crate::ln::msgs::DecodeError> {
        let byte = <u8 as Readable>::read(reader)?;
        match byte {
            1 => Ok(Ok(SocketAddress::TcpIpV4 {
                addr: Readable::read(reader)?,
                port: Readable::read(reader)?,
            })),
            2 => Ok(Ok(SocketAddress::TcpIpV6 {
                addr: Readable::read(reader)?,
                port: Readable::read(reader)?,
            })),
            3 => Ok(Ok(SocketAddress::OnionV2(Readable::read(reader)?))),
            4 => Ok(Ok(SocketAddress::OnionV3 {
                ed25519_pubkey: Readable::read(reader)?,
                checksum: Readable::read(reader)?,
                version: Readable::read(reader)?,
                port: Readable::read(reader)?,
            })),
            5 => Ok(Ok(SocketAddress::Hostname {
                hostname: Readable::read(reader)?,
                port: Readable::read(reader)?,
            })),
            _ => Ok(Err(byte)),
        }
    }
}

impl Readable for SocketAddress {
    fn read<R: Read>(reader: &mut R) -> Result<SocketAddress, DecodeError> {
        match Readable::read(reader) {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(_)) => Err(DecodeError::UnknownVersion),
            Err(e) => Err(e),
        }
    }
}

/// [`SocketAddress`] error variants
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum SocketAddressParseError {
    /// Socket address (IPv4/IPv6) parsing error
    SocketAddrParse,
    /// Invalid input format
    InvalidInput,
    /// Invalid port
    InvalidPort,
    /// Invalid onion v3 address
    InvalidOnionV3,
}

impl std::fmt::Display for SocketAddressParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SocketAddressParseError::SocketAddrParse => {
                write!(f, "Socket address (IPv4/IPv6) parsing error")
            }
            SocketAddressParseError::InvalidInput => write!(
                f,
                "Invalid input format. \
				Expected: \"<ipv4>:<port>\", \"[<ipv6>]:<port>\", \"<onion address>.onion:<port>\" or \"<hostname>:<port>\""
            ),
            SocketAddressParseError::InvalidPort => write!(f, "Invalid port"),
            SocketAddressParseError::InvalidOnionV3 => write!(f, "Invalid onion v3 address"),
        }
    }
}

impl From<std::net::SocketAddrV4> for SocketAddress {
    fn from(addr: std::net::SocketAddrV4) -> Self {
        SocketAddress::TcpIpV4 {
            addr: addr.ip().octets(),
            port: addr.port(),
        }
    }
}

impl From<std::net::SocketAddrV6> for SocketAddress {
    fn from(addr: std::net::SocketAddrV6) -> Self {
        SocketAddress::TcpIpV6 {
            addr: addr.ip().octets(),
            port: addr.port(),
        }
    }
}

impl From<std::net::SocketAddr> for SocketAddress {
    fn from(addr: std::net::SocketAddr) -> Self {
        match addr {
            std::net::SocketAddr::V4(addr) => addr.into(),
            std::net::SocketAddr::V6(addr) => addr.into(),
        }
    }
}

impl std::net::ToSocketAddrs for SocketAddress {
    type Iter = std::vec::IntoIter<std::net::SocketAddr>;

    fn to_socket_addrs(&self) -> std::io::Result<Self::Iter> {
        use std::net::SocketAddr;
        match self {
            SocketAddress::TcpIpV4 { addr, port } => {
                let ip_addr = std::net::Ipv4Addr::from(*addr);
                let socket_addr = SocketAddr::new(ip_addr.into(), *port);
                Ok(vec![socket_addr].into_iter())
            }
            SocketAddress::TcpIpV6 { addr, port } => {
                let ip_addr = std::net::Ipv6Addr::from(*addr);
                let socket_addr = SocketAddr::new(ip_addr.into(), *port);
                Ok(vec![socket_addr].into_iter())
            }
            SocketAddress::Hostname { hostname, port } => {
                (hostname.as_str(), *port).to_socket_addrs()
            }
            SocketAddress::OnionV2(..) => Err(std::io::Error::other(
                "Resolution of OnionV2 addresses is currently unsupported.",
            )),
            SocketAddress::OnionV3 { .. } => Err(std::io::Error::other(
                "Resolution of OnionV3 addresses is currently unsupported.",
            )),
        }
    }
}

/*
/// Parses an OnionV3 host and port into a [`SocketAddress::OnionV3`].
///
/// The host part must end with ".onion".
pub fn parse_onion_address(
    host: &str,
    port: u16,
) -> Result<SocketAddress, SocketAddressParseError> {
    if host.ends_with(".onion") {
        let domain = &host[..host.len() - ".onion".len()];
        if domain.len() != 56 {
            return Err(SocketAddressParseError::InvalidOnionV3);
        }
        let onion = base32::Alphabet::RFC4648 { padding: false }
            .decode(&domain)
            .map_err(|_| SocketAddressParseError::InvalidOnionV3)?;
        if onion.len() != 35 {
            return Err(SocketAddressParseError::InvalidOnionV3);
        }
        let version = onion[0];
        let first_checksum_flag = onion[1];
        let second_checksum_flag = onion[2];
        let mut ed25519_pubkey = [0; 32];
        ed25519_pubkey.copy_from_slice(&onion[3..35]);
        let checksum = u16::from_be_bytes([first_checksum_flag, second_checksum_flag]);
        return Ok(SocketAddress::OnionV3 {
            ed25519_pubkey,
            checksum,
            version,
            port,
        });
    } else {
        return Err(SocketAddressParseError::InvalidInput);
    }
}
*/

impl Display for SocketAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SocketAddress::TcpIpV4 { addr, port } => write!(
                f,
                "{}.{}.{}.{}:{}",
                addr[0], addr[1], addr[2], addr[3], port
            )?,
            SocketAddress::TcpIpV6 { addr, port } => write!(
                f,
                "[{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}]:{}",
                addr[0],
                addr[1],
                addr[2],
                addr[3],
                addr[4],
                addr[5],
                addr[6],
                addr[7],
                addr[8],
                addr[9],
                addr[10],
                addr[11],
                addr[12],
                addr[13],
                addr[14],
                addr[15],
                port
            )?,
            SocketAddress::OnionV2(bytes) => write!(f, "OnionV2({:?})", bytes)?,
            SocketAddress::OnionV3 {
                ed25519_pubkey,
                checksum,
                version,
                port,
            } => {
                let [first_checksum_flag, second_checksum_flag] = checksum.to_be_bytes();
                let mut addr = vec![*version, first_checksum_flag, second_checksum_flag];
                addr.extend_from_slice(ed25519_pubkey);
                let onion = base32::Alphabet::RFC4648 { padding: false }.encode(&addr);
                write!(f, "{}.onion:{}", onion, port)?
            }
            SocketAddress::Hostname { hostname, port } => write!(f, "{}:{}", hostname, port)?,
        }
        Ok(())
    }
}

impl FromStr for SocketAddress {
    type Err = SocketAddressParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match std::net::SocketAddr::from_str(s) {
            Ok(addr) => Ok(addr.into()),
            Err(_) => {
                let trimmed_input = match s.rfind(":") {
                    Some(pos) => pos,
                    None => return Err(SocketAddressParseError::InvalidInput),
                };
                let host = &s[..trimmed_input];
                let port: u16 = s[trimmed_input + 1..]
                    .parse()
                    .map_err(|_| SocketAddressParseError::InvalidPort)?;
                if host.ends_with(".onion") {
                    return parse_onion_address(host, port);
                };
                if let Ok(hostname) = Hostname::try_from(s[..trimmed_input].to_string()) {
                    return Ok(SocketAddress::Hostname { hostname, port });
                };
                Err(SocketAddressParseError::SocketAddrParse)
            }
        }
    }
}

/// Parses an OnionV3 host and port into a [`SocketAddress::OnionV3`].
///
/// The host part must end with ".onion".
pub fn parse_onion_address(
    host: &str,
    port: u16,
) -> Result<SocketAddress, SocketAddressParseError> {
    if host.ends_with(".onion") {
        let domain = if let Some(domain) = host.strip_suffix(".onion") {
            if domain.len() != 56 {
                return Err(SocketAddressParseError::InvalidOnionV3);
            }
            domain
        } else {
            return Err(SocketAddressParseError::InvalidOnionV3);
        };
        let onion = base32::Alphabet::RFC4648 { padding: false }
            .decode(domain)
            .map_err(|_| SocketAddressParseError::InvalidOnionV3)?;
        if onion.len() != 35 {
            return Err(SocketAddressParseError::InvalidOnionV3);
        }
        let version = onion[0];
        let first_checksum_flag = onion[1];
        let second_checksum_flag = onion[2];
        let mut ed25519_pubkey = [0; 32];
        ed25519_pubkey.copy_from_slice(&onion[3..35]);
        let checksum = u16::from_be_bytes([first_checksum_flag, second_checksum_flag]);
        Ok(SocketAddress::OnionV3 {
            ed25519_pubkey,
            checksum,
            version,
            port,
        })
    } else {
        Err(SocketAddressParseError::InvalidInput)
    }
}
