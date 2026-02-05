use crate::args::ServerArgs;
use crate::server::ResponseBody::{Static, Zeros};
use boring::ssl::{SslContextBuilder, SslMethod};
use log::{debug, error, info};
use mio::unix::pipe::Receiver;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::quiche::h3::NameValue;
use quiche_mio_runner::quiche_endpoint::quiche::{h3, PathStats, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::ServerConfig;
use quiche_mio_runner::quiche_endpoint::{quiche, Conn, MAX_UDP_PAYLOAD};
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig};
use quiche_mio_runner::Socket;
use regex::Regex;
use std::collections::HashMap;
use std::str::{from_utf8, FromStr};
use crate::cert::load_or_generate_keys;

const H3_BUF_SIZE: usize = MAX_UDP_PAYLOAD * 8;

type Runner = runner::Runner<ConnAppData, AppData<H3_BUF_SIZE>, ()>;

struct AppData<const BUF_SIZE: usize> {
    h3_config: h3::Config,
    h3_buf: [u8; BUF_SIZE],
}

#[derive(Default)]
struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    partial_responses: HashMap<u64, PartialResponse>,
}

struct PartialResponse {
    written: usize,
    // some if send pending
    headers: Option<Vec<h3::Header>>,
    body: ResponseBody,
}

enum ResponseBody {
    /// body with static content
    Static(&'static [u8]),
    /// body with zero bytes of specified length, served from memory
    Zeros(usize),
}

impl ResponseBody {
    fn len(&self) -> usize {
        match self {
            Static(v) => v.len(),
            Zeros(l) => *l
        }
    }
}

#[allow(clippy::field_reassign_with_default)]
pub fn server(args: &ServerArgs, close_pipe_rx: Option<&mut Receiver>) {
    let (cert, key) = load_or_generate_keys(&args.cert, &args.key);

    let socket = Socket::bind(args.bind, args.disable_gro, false, args.disable_gso).unwrap();
    assert_eq!(socket.enable_gro, !args.disable_gro);
    assert!(socket.enable_pacing);
    assert_eq!(socket.enable_gso, !args.disable_gso);
    let local_addr = socket.local_addr;
    info!("Server listening on https://{}", local_addr);

    let client_config = {
        let mut c = quiche::Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
            let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
            b.set_private_key(&key).unwrap();
            b.set_certificate(&cert).unwrap();
            b
        }).unwrap();
        c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL).unwrap();
        c.set_max_idle_timeout(args.idle_timeout);
        c.set_initial_max_streams_bidi(args.max_streams_bidi);
        c.set_initial_max_streams_uni(args.max_streams_uni);
        c.set_initial_max_data(25_165_824);
        c.set_initial_max_stream_data_bidi_remote(16_777_216);
        c.set_initial_max_stream_data_bidi_local(16_777_216);
        c.set_initial_max_stream_data_uni(16_777_216);
        c.set_max_send_udp_payload_size(args.max_udp_payload);
        c.set_active_connection_id_limit(2);
        c.set_initial_congestion_window_packets(10);
        c.set_max_connection_window(25_165_824);
        c.set_max_stream_window(16_777_216);
        c.enable_pacing(true);
        c.grease(false);
        c
    };


    let endpoint = Endpoint::new(
        Some({
            let mut c = ServerConfig::default();
            c.client_config = client_config;
            c
        }),
        {
            let mut c = EndpointConfig::default();
            c.ignore_pacing = true;
            c.ignore_quantum = true;
            c
        },
        AppData {
            h3_config: h3::Config::new().unwrap(),
            h3_buf: [0; H3_BUF_SIZE],
        },
    );

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c.on_close = Some(on_close);
            c
        },
        endpoint,
        close_pipe_rx,
    );

    runner.register_socket(socket);

    runner.run();
}

fn on_close(c: &Conn<ConnAppData>, _: &mut AppData<H3_BUF_SIZE>) {
    info!(
        "{} connection collected {:?} {:?}",
        c.conn.trace_id(),
        c.conn.stats(),
        c.conn.path_stats().collect::<Vec<PathStats>>()
    );
}

fn post_handle_recvs(runner: &mut Runner) {
    let endpoint = &mut runner.endpoint;
    for i in endpoint.conn_index_iter() {
        let Some(conn) = endpoint.conn(i) else {
            continue
        };
        let (conn, app_data) = if conn.app_data.h3_conn.is_none() {
            if !conn.conn.is_established() && !conn.conn.is_in_early_data() {
                continue; // not ready for h3 yet
            }
            let (conn, app_data) = endpoint.conn_with_app_data_mut(i);
            let conn = conn.unwrap();
            conn.app_data.h3_conn = Some(h3::Connection::with_transport(
                &mut conn.conn,
                &app_data.h3_config,
            ).expect("Unable to create HTTP/3 connection, check the server's uni stream limit and window size"));
            (conn, app_data)
        } else {
            let (conn, app_data) = endpoint.conn_with_app_data_mut(i);
            let conn = conn.unwrap();
            (conn, app_data)
        };

        let quic = &mut conn.conn;
        let h3 = conn.app_data.h3_conn.as_mut().unwrap();
        let partial_responses = &mut conn.app_data.partial_responses;

        handle_h3_requests(h3, quic, partial_responses).expect("TODO: panic message");

        handle_h3_writable(quic, h3, partial_responses, &app_data.h3_buf);
    }
}

fn handle_h3_requests(h3_conn: &mut h3::Connection, quic_conn: &mut quiche::Connection, partial_responses: &mut HashMap<u64, PartialResponse>) -> h3::Result<()> {
    loop {
        match h3_conn.poll(quic_conn) {
            Ok((stream_id, h3::Event::Headers { list, more_frames: _ })) => {
                info!(
                        "{} got request {:?} on stream id {}",
                        quic_conn.trace_id(),
                        list,
                        stream_id
                    );
                let partial_response = build_h3_response(list.as_slice(), stream_id, quic_conn);
                partial_responses.insert(stream_id, partial_response);
            }
            Ok((_stream_id, h3::Event::Finished)) => (),
            Ok((prioritized_element_id, h3::Event::PriorityUpdate)) => {
                info!(
                    "{} PRIORITY_UPDATE triggered for element ID={}",
                    quic_conn.trace_id(),
                    prioritized_element_id
                );
            }
            Ok((stream_id, e)) => {
                info!("{:?} on stream {}", e, stream_id);
                unimplemented!()
            }
            Err(h3::Error::Done) => {
                break;
            }
            Err(e) => {
                error!("{} HTTP/3 error {:?}", quic_conn.trace_id(), e);
                return Err(e);
            }
        }
    }
    Ok(())
}

fn handle_h3_writable(quic_conn: &mut quiche::Connection, h3_conn: &mut h3::Connection, partial_responses: &mut HashMap<u64, PartialResponse>, buf: &[u8]) {
    'streamLoop: for stream_id in quic_conn.writable() {
        let resp = match partial_responses.get_mut(&stream_id) {
            None => continue, // no such key
            Some(v) => v
        };

        if let Some(h) = &resp.headers {
            match h3_conn.send_response(
                quic_conn,
                stream_id,
                h,
                false,
            ) {
                Ok(_) => {
                    resp.headers = None;
                }
                Err(h3::Error::StreamBlocked) => continue 'streamLoop, // try again next time
                Err(e) => {
                    error!("{} error sending response {:?}", quic_conn.trace_id(), e);
                    continue 'streamLoop;
                }
            }
        }

        loop {
            let (buf, fin) = match resp.body {
                Static(body) => {
                    (&body[resp.written..], true)
                }
                Zeros(len) => {
                    let remaining = len - resp.written;
                    if remaining > buf.len() {
                        (buf, false)
                    } else {
                        (&buf[..remaining], true)
                    }
                }
            };

            let written = match h3_conn.send_body(quic_conn, stream_id, buf, fin) {
                Ok(v) => v,
                Err(h3::Error::Done) => continue 'streamLoop,
                Err(e) => {
                    partial_responses.remove(&stream_id);
                    error!("{} stream send failed {:?}", quic_conn.trace_id(), e);
                    continue 'streamLoop;
                }
            };

            resp.written += written;
            if resp.written == resp.body.len() {
                partial_responses.remove(&stream_id);
                continue 'streamLoop;
            }
        }
    }
}

fn build_h3_response(request: &[h3::Header], stream_id: u64, quic_conn: &mut quiche::Connection) -> PartialResponse {
    let mut path = None;

    for hdr in request {
        match hdr.name() {
            b":path" => {
                if path.is_some() {
                    quic_conn.stream_shutdown(
                        stream_id,
                        quiche::Shutdown::Write,
                        h3::WireErrorCode::ConnectError as u64,
                    ).unwrap();
                    break;
                }
                path = Some(from_utf8(hdr.value()).unwrap())
            }
            b":method" => {
                assert_eq!(from_utf8(hdr.value()).unwrap(), "GET")
            }
            b":scheme" => {
                assert_eq!(from_utf8(hdr.value()).unwrap(), "https")
            }
            b":authority" => {
                //TODO
            }
            b"user-agent" => {
                //ignore
            }
            b => {
                debug!("{} header not supported", from_utf8(b).unwrap());
            }
        }
    }

    let mem_request = MemRequest::from_str(path.unwrap_or("")).ok();

    const BODY_404: &[u8] = b"404 Not Found; try e.g. /mem/1MB instead";
    let mem_request = match mem_request {
        None => return PartialResponse{
            written: 0,
            headers: Some(Vec::from([
                h3::Header::new(b":status", b"404"),
                h3::Header::new(b"server", b"quiche"),
                h3::Header::new(b"content-length", BODY_404.len().to_string().as_bytes()),
            ])),
            body: Static(BODY_404),
        },
        Some(v) => v,
    };

    PartialResponse {
        written: 0,
        headers: Some(Vec::from([
            h3::Header::new(b":status", b"200"),
            h3::Header::new(b"server", b"quiche"),
            h3::Header::new(b"content-length", mem_request.0.to_string().as_bytes()),
        ])),
        body: Zeros(mem_request.0),
    }
}

/// Represents a request path in the form `/mem/<bytes>[<unit>]`;
/// supported units are none, `B`, `kB`, `MB`, and `GB`
struct MemRequest(usize);

impl FromStr for MemRequest {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let r = Regex::new(r"/mem/([0-9]+)([a-zA-Z]*)").unwrap();
        let c = r.captures(s).ok_or(())?;
        let number = c.get(1).unwrap().as_str().parse::<usize>().map_err(|_| ())?;
        let unit = c.get(2).unwrap().as_str();

        let number = if unit.is_empty() | unit.eq_ignore_ascii_case("B") {
            number
        } else if unit.eq_ignore_ascii_case("kB") {
            number * 1E3 as usize
        } else if unit.eq_ignore_ascii_case("MB") {
            number * 1E6 as usize
        } else if unit.eq_ignore_ascii_case("GB") {
            number * 1E9 as usize
        } else {
            return Err(());
        };
        Ok(Self(number))
    }
}
