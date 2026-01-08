use crate::args::ClientArgs;
use crate::H3_NO_ERROR;
use log::Level::Info;
use log::{debug, error, info};
use quiche_mio_runner::quiche_endpoint::quiche::h3::NameValue;
use quiche_mio_runner::quiche_endpoint::quiche::{h3, ConnectionError, PathStats, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::{quiche, Conn};
use quiche_mio_runner::quiche_endpoint::{Endpoint, EndpointConfig, INSTANT_MAX, INSTANT_ZERO};
use quiche_mio_runner::Socket;
use std::cmp::{max, min};
use std::fmt::{Debug, Formatter};
use std::time::Instant;

use quiche_mio_runner as runner;
use crate::h3::hdrs_to_strings;

type Runner = runner::Runner<ConnAppData, AppData, ()>;

pub struct AppData {
    conn_id: usize,
    h3_config: h3::Config,
    silent_close: bool,
    pub reqs_complete: usize,
}

struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    reqs: Vec<PartialRequest>,
    reqs_hdrs_sent: usize,
    reqs_complete: usize,
}

#[derive(Clone)]
struct PartialRequest {
    hdrs: Vec<h3::Header>,
    stream_id: Option<u64>,
    received_header_instant: Option<Instant>,
    received_body_instant: Option<Instant>,
    received_body_bytes: usize,
}

pub fn client(args: &ClientArgs) -> AppData {
    let socket = Socket::bind("0.0.0.0:0".parse().unwrap(), args.disable_gro, false, args.disable_gso).unwrap();
    assert_eq!(socket.enable_gro, !args.disable_gro);
    assert!(socket.enable_pacing);
    assert_eq!(socket.enable_gso, !args.disable_gso);
    let local_addr = socket.local_addr;

    let mut quic_config = {
        let mut c = quiche::Config::new(PROTOCOL_VERSION).unwrap();
        c.verify_peer(!args.no_verify);
        c.set_application_protos(quiche::h3::APPLICATION_PROTOCOL).unwrap();
        c.set_max_idle_timeout(args.idle_timeout);
        c.set_initial_max_streams_bidi(100);
        c.set_initial_max_streams_uni(100);
        c.set_initial_max_data(10000000);
        c.set_initial_max_stream_data_bidi_remote(1000000);
        c.set_initial_max_stream_data_bidi_local(1000000);
        c.set_initial_max_stream_data_uni(1000000);
        c.set_max_recv_udp_payload_size(args.max_udp_payload);
        c.set_max_send_udp_payload_size(args.max_udp_payload);
        c.set_active_connection_id_limit(2);
        c.set_initial_congestion_window_packets(10);
        c.set_max_connection_window(25165824);
        c.set_max_stream_window(16777216);
        c.enable_pacing(true);
        c.grease(false);
        if let Some(cert) = &args.cert {
            c.load_verify_locations_from_file(cert.to_str().expect("Invalid certificate path")).expect("Failed to load certificate");
        }
        c
    };

    let h3_config = h3::Config::new().unwrap();

    let url = url::Url::parse(&args.url).unwrap();

    let peer_addr = match args.addr {
        Some(v) => v,
        None => { // resolve from url
            url.socket_addrs(|| Some(4433)).unwrap()[0]
        }
    };

    let mut endpoint = Endpoint::new(
        None,
        {
            let mut c = EndpointConfig::default();
            c.ignore_pacing = true;
            c.ignore_quantum = true;
            c
        },
        AppData {
            conn_id: 0,
            h3_config,
            silent_close: args.silent_close,
            reqs_complete: 0,
        },
    );

    endpoint.connect(
        url.domain(),
        local_addr,
        peer_addr,
        &mut quic_config,
        ConnAppData {
            h3_conn: None,
            reqs: vec![PartialRequest {
                hdrs: vec![
                    h3::Header::new(b":method", b"GET"),
                    h3::Header::new(b":scheme", b"https"),
                    h3::Header::new(b":authority", match url.port() {
                        None => url.host_str().unwrap().to_string(),
                        Some(port) => format!("{}:{}", url.host_str().unwrap(), port)
                    }.as_bytes()),
                    h3::Header::new(b":path", url[url::Position::BeforePath..].as_bytes()),
                    h3::Header::new(b"user-agent", b"quiche"),
                ],
                stream_id: None,
                received_header_instant: None,
                received_body_instant: None,
                received_body_bytes: 0,
            }; args.streams as usize],
            reqs_hdrs_sent: 0,
            reqs_complete: 0,
        },
        None,
        None,
    );

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c.on_close = Some(on_close);
            c
        },
        endpoint,
        None,
    );

    runner.register_socket(socket);

    runner.run();
    runner.endpoint.take_app_data()
}

fn post_handle_recvs(runner: &mut Runner) {
    let mut endpoint = &mut runner.endpoint;
    let (conn, app_data) = endpoint.conn_with_app_data_mut(endpoint.app_data().conn_id);
    let conn = conn.unwrap();
    if !conn.conn.is_established() && !conn.conn.is_in_early_data() {
        return; // not ready for h3 yet
    }
    if conn.app_data.h3_conn.is_none() {
        conn.app_data.h3_conn = Some(h3::Connection::with_transport(
            &mut conn.conn,
            &app_data.h3_config,
        ).expect("Unable to create HTTP/3 connection, check the server's uni stream limit and window size"));
    }
    let closed = handle_h3_responses(conn, &mut runner.buf, app_data);
    if closed && app_data.silent_close {
        endpoint.remove_conn(endpoint.app_data().conn_id);
    }

    send_requests(&mut endpoint);
}

fn on_close(c: &Conn<ConnAppData>, _: &mut AppData) {
    if let Some(err) = c.conn.peer_error() {
        error!(
            "{} peer connection error: {:?}",
            c.conn.trace_id(),
            PrettyConnectionError(err)
        );
    } else if let Some(err) = c.conn.local_error() {
        if !err.is_app || err.error_code != H3_NO_ERROR {
            error!(
                "{} local connection error: {:?}",
                c.conn.trace_id(),
                PrettyConnectionError(err)
            );
        }
    } else if c.conn.is_timed_out() {
        panic!("connection timed out");
    }
    info!(
        "{} connection collected {:?} {:?}",
        c.conn.trace_id(),
        c.conn.stats(),
        c.conn.path_stats().collect::<Vec<PathStats>>()
    );
}

/// return true if connection closed
fn handle_h3_responses(conn: &mut Conn<ConnAppData>, buf: &mut [u8], app_data: &mut AppData) -> bool {
    let h3_conn = conn.app_data.h3_conn.as_mut().unwrap();
    loop {
        match h3_conn.poll(&mut conn.conn) {
            Ok((stream_id, h3::Event::Headers { list, .. })) => {
                info!(
                    "recv h3 resp hdr {:?} on stream id {}",
                    hdrs_to_strings(&list),
                    stream_id
                );
                let req = conn.app_data
                    .reqs
                    .iter_mut()
                    .find(|r| r.stream_id == Some(stream_id))
                    .unwrap();
                req.received_header_instant = Some(Instant::now());
            }
            Ok((stream_id, h3::Event::Data)) => {
                'data: loop {
                    match h3_conn.recv_body(&mut conn.conn, stream_id, buf) {
                        Ok(read) => {
                            debug!(
                                "got {} bytes of response data on stream {}: {}",
                                read, stream_id, String::from_utf8_lossy(&buf[..read])
                            );
                            let req = conn.app_data
                                .reqs
                                .iter_mut()
                                .find(|r| r.stream_id == Some(stream_id))
                                .unwrap();
                            req.received_body_bytes += read;
                        }
                        Err(h3::Error::Done) => {
                            break 'data;
                        }
                        Err(e) => {
                            unreachable!("{:?}", e)
                        }
                    }
                }
            }
            Ok((stream_id, h3::Event::Finished)) => {
                let req = conn.app_data
                    .reqs
                    .iter_mut()
                    .find(|r| r.stream_id == Some(stream_id))
                    .unwrap();
                req.received_body_instant = Some(Instant::now());
                conn.app_data.reqs_complete += 1;
                app_data.reqs_complete += 1;
                if log::log_enabled!(Info) {
                    let duration = (req.received_body_instant.unwrap() - req.received_header_instant.unwrap()).as_secs_f64();
                    let goodput = req.received_body_bytes as f64 * 8f64 / duration;
                    info!(
                        "recv h3 resp body {}: {} B, {:.6} s, {:.6} Gbps",
                        String::from_utf8_lossy(req.hdrs.iter().find(|h| h.name() == b":path").unwrap().value()),
                        req.received_body_bytes,
                        duration,
                        goodput / 1E9,
                    );
                }
                if conn.app_data.reqs_complete == conn.app_data.reqs.len() {
                    print_total_results(&conn.app_data.reqs);
                    conn.conn.close(true, H3_NO_ERROR, b"").unwrap();
                    return true
                }
            }
            Ok((_stream_id, h3::Event::Reset(_e))) => {
                unimplemented!()
            }
            Ok((_stream_id, h3::Event::PriorityUpdate)) => {
                unimplemented!()
            }
            Ok((_stream_id, h3::Event::GoAway)) => {
                unimplemented!()
            }
            Err(h3::Error::Done) => {
                break; // no more events to process
            }
            Err(_e) => {
                unimplemented!()
            }
        }
    }
    false
}

fn print_total_results(reqs: &[PartialRequest]) {
    let mut min_received_header_instant = INSTANT_MAX;
    let mut max_received_body_instant = INSTANT_ZERO;
    let mut sum_received_body_bytes = 0;
    for req in reqs {
        min_received_header_instant = min(req.received_header_instant.unwrap(), min_received_header_instant);
        max_received_body_instant = max(req.received_body_instant.unwrap(), max_received_body_instant);
        sum_received_body_bytes += req.received_body_bytes;
    }
    let duration = (max_received_body_instant - min_received_header_instant).as_secs_f64();
    let goodput = sum_received_body_bytes as f64 * 8f64 / duration;
    info!(
        "total: reqs {}, {} B, {:.6} s, {:.6} Gbps",
        reqs.len(),
        sum_received_body_bytes,
        duration,
        goodput / 1E9,
    );
}

fn send_requests(endpoint: &mut Endpoint<ConnAppData, AppData>) {
    for i in endpoint.conn_index_iter() {
        let Some(conn) = endpoint.conn(i) else {
            continue
        };
        // borrow mutable if necessary
        let conn = if conn.app_data.h3_conn.is_none() {
            continue // not yet ready for h3
        } else {
            endpoint.conn_mut(i).unwrap()
        };
        let h3_conn = conn.app_data.h3_conn.as_mut().unwrap();

        for req in conn.app_data.reqs.iter_mut().skip(conn.app_data.reqs_hdrs_sent) {
            let stream_id = match h3_conn.send_request(
                &mut conn.conn,
                &req.hdrs,
                true,
            ) {
                Ok(v) => v,
                Err(h3::Error::TransportError(quiche::Error::StreamLimit)) => {
                    continue // try again next time
                }
                Err(h3::Error::StreamBlocked) => {
                    unimplemented!()
                }
                Err(_e) => {
                    unimplemented!()
                }
            };
            info!("sent h3 req {:?}", &req.hdrs);
            req.stream_id = Some(stream_id);
            conn.app_data.reqs_hdrs_sent += 1;

            //TODO support sending body
        }
    }
}

struct PrettyConnectionError<'a>(&'a ConnectionError);

impl<'a> Debug for PrettyConnectionError<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionError")
            .field("is_app", &self.0.is_app)
            .field("error_code", &self.0.error_code)
            .field("reason", &String::from_utf8_lossy(&self.0.reason))
            .finish()
    }
}
