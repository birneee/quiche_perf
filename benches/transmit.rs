use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use quiche_perf::args::{ClientArgs, ServerArgs};
use quiche_perf::client::client;
use quiche_perf::server::server;
use std::io::Write;
use std::thread;
use std::time::Duration;
use log::LevelFilter;

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    targets = targets
);
criterion_main!(benches);

fn targets(c: &mut Criterion) {
    env_logger::builder().filter_level(LevelFilter::Error).init();
    let mut g = c.benchmark_group("perf");
    let num_bytes = 1E9 as usize;
    g.throughput(Throughput::Bits(num_bytes as u64 * 8));
    g.bench_function("transmit-1G", |b| b.iter(|| transmit(num_bytes, false, false)));
    g.bench_function("transmit-1G-gso-gro", |b| b.iter(|| transmit(num_bytes, true, true)));
    g.finish()
}

fn transmit(num_bytes: usize, gso: bool, gro: bool) {
    let (mut close_pipe_tx, mut close_pipe_rx) = mio::unix::pipe::new().unwrap();
    let server_join_handle = thread::spawn(move|| {
        server(&ServerArgs {
            cert: None,
            key: None,
            max_udp_payload: 1500-44,
            disable_gro: !gro,
            disable_gso: !gso,
            bind: "127.0.0.1:4433".parse().unwrap(),
            max_streams_bidi: 100,
            max_streams_uni: 100,
            idle_timeout: 1000,
        }, Some(&mut close_pipe_rx));
    });
    let client_join_handle = thread::spawn(move || {
        let app_data = client(&ClientArgs {
            url: format!("https://127.0.0.1:4433/mem/{}", num_bytes),
            addr: None,
            no_verify: true,
            max_udp_payload: 1500-44,
            disable_gro: !gro,
            disable_gso: !gso,
            cert: None,
            streams: 1,
            silent_close: true,
            idle_timeout: 1000,
        });
        assert_eq!(app_data.reqs_complete, 1)
    });
    client_join_handle.join().unwrap();
    close_pipe_tx.write(&[0]).unwrap();
    server_join_handle.join().unwrap();
}