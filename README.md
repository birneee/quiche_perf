# Quiche Perf

> ⚠️ Not production ready

> This project is not affiliated with [Cloudflare quiche](https://github.com/cloudflare/quiche)

A simple QUIC and HTTP/3 performance tools.

## Features
- Serve data from memory as fast as possible
- Any H3 client can fetch data, e.g., browsers, curl
- Multi client support
- Fast UDP IO with GSO and GRO

## Build

With cargo:
```bash
cargo build --release
```

With nix:
```bash
nix build
```

## Run server

> See [here](#generate-certificate) how to generate a certificate

```bash
RUST_LOG=info target/release/quiche-perf server --cert cert.pem --key key.pem
```

## Run client

Example command to download a 1GB file

```bash
RUST_LOG=info target/release/quiche-perf client https://127.0.0.1:4433/mem/1GB --cert cert.pem
```

## Use browser as client

Example command to download a 1GB file with Chromium

```bash
SSLKEYLOGFILE=sslkeylog chromium 'https://127.0.0.1:4433/mem/1GB' \
  --user-data-dir=chromium-data \
  --origin-to-force-quic-on="*" \
  --ignore-certificate-errors-spki-list="`cat spki`"
```

## Generate certificate

Generate a self-signed TLS certificate, key and SPKI.

> if no keys are provided to the tools, a self-signed key pair is generated, and the spki hash is logged to stdout.

```bash
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem -sha256 -days 3650 -nodes -subj "/C=XX/ST=XX/L=XX/O=XX/OU=XX/CN=127.0.0.1"
openssl x509 -noout -pubkey -in cert.pem | openssl rsa -pubin -outform der | openssl dgst -sha256 -binary | openssl enc -base64 > spki
```
