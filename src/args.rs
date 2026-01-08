use std::net::SocketAddr;
use std::path::PathBuf;
use clap::Args;

#[derive(Args)]
pub struct ClientArgs {
    /// The url to connect to
    #[arg()]
    pub url: String,
    /// The remote address to connect to.
    /// If not provided the IP and port is resolved from the url.
    /// The default port is 4433.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<SocketAddr>,
    /// Don't verify server's certificate
    #[arg(long)]
    pub no_verify: bool,
    /// Max UDP payload to send and receive in bytes
    #[arg(long, value_name="BYTES", default_value_t=1500-44)]
    pub max_udp_payload: usize,
    /// Disable Generic Receive Offload
    #[arg(long)]
    pub disable_gro: bool,
    /// Disable Generic Send Offload
    #[arg(long)]
    pub disable_gso: bool,
    /// A file path of TLS certificate to trust
    #[arg(long, value_name="PATH")]
    pub cert: Option<PathBuf>,
    /// Number of streams to simultaneously do the same request
    #[arg(long, value_name="STREAMS", default_value_t=1)]
    pub streams: u64,
    #[arg(long, default_value_t=false)]
    pub silent_close: bool,
    #[arg(long, value_name="MS", default_value_t=30_000)]
    pub idle_timeout: u64,
}

#[derive(Args)]
pub struct ServerArgs {
    /// TLS certificate path. Generated if not specified
    #[arg(long, value_name="PATH")]
    pub cert: Option<PathBuf>,
    /// TLS certificate key path. Generated if not specified
    #[arg(long, value_name="PATH")]
    pub key: Option<PathBuf>,
    /// Max UDP payload to send and receive in bytes
    #[arg(long, value_name="BYTES", default_value_t=1500-44)]
    pub max_udp_payload: usize,
    #[arg(long)]
    pub disable_gro: bool,
    /// Disable Generic Send Offload
    #[arg(long)]
    pub disable_gso: bool,
    /// Address to bind socket to
    #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:4433")]
    pub bind: SocketAddr,
    /// Number of concurrently allowed remotely-initiated bidirectional streams per connection
    #[arg(long, value_name="STREAMS", default_value_t=100)]
    pub max_streams_bidi: u64,
    /// Number of concurrently allowed remotely-initiated unidirectional streams per connection
    #[arg(long, value_name="STREAMS", default_value_t=100)]
    pub max_streams_uni: u64,
    #[arg(long, value_name="MS", default_value_t=30_000)]
    pub idle_timeout: u64,
}