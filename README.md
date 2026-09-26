# DS Video HEVC Passthrough

A small proxy that sits in front of Synology **Video Station** and lets the
**DS Video** app (Apple TV and other clients) play HEVC files untouched instead of
forcing a transcode the NAS can't handle.

## The problem

DS Video asks Video Station to open a stream with `hls_remux`. On NAS CPUs without
an HEVC profile (for example Intel Cedarview in the DS1813+), Video Station refuses
HEVC sources with **error 1211**. DS Video then falls back to a full transcode, the
NAS can't keep up, and you get a black screen.

The Video Station web UI doesn't have this problem because it opens the same file in
`raw` mode and streams the original bytes.

## How it works

`dsvideo-passthrough` is a transparent HTTP proxy. DS Video connects to it instead of
DSM, and every request is forwarded to Video Station unchanged, except:

1. **`SYNO.VideoStation2.Streaming` → `open` with `hls_remux`**. If Video Station
   answers error 1211, the proxy sends the same request again with `raw={}`
   instead and returns the new stream id to DS Video as if it were an
   `hls_remux` stream.
2. **`vtestreaming.cgi` `stream`/`close` for such a stream id**. The proxy
   rewrites these to `format=raw`, so the player gets the original file with
   range requests intact.

Anything Video Station can already remux is passed through as-is. The proxy logs
each API call and its outcome to stderr.

## Install on the NAS (DSM 7)

1. Download the `.spk` from [Releases](https://github.com/holg/ds-video-hevc/releases), or build it yourself (see below).
   Release packages are built by GitHub Actions from the tagged commit, and each
   comes with a signed build provenance attestation. To check a download:
   ```sh
   gh attestation verify dsvideo_passthrough-<version>.spk --repo holg/ds-video-hevc
   ```
2. In **Package Center → Manual Install**, pick
   `dsvideo_passthrough-<version>.spk`. Video Station must be installed.
3. The install wizard asks for:
   - **Listen port**, default `5080`. This is the port DS Video will use.
   - **Video Station URL**, default `http://127.0.0.1:5000`, which is the NAS
     itself.
4. In DS Video, change the server address to `<your-nas>:5080`.

The package is `noarch` and ships static binaries for x86_64, aarch64 and armv7.
The install script keeps only the one matching the NAS. Settings are stored in
`/var/packages/dsvideo_passthrough/var/config` (`PORT=`, `UPSTREAM=`) and survive
upgrades. The log is `dsvideo-passthrough.log` in the same directory and is rotated
at 5 MB.

## Build

Requirements: Rust (via `rustup`), plus GNU tar and `jq` for packaging
(`brew install gnu-tar jq` on macOS; Linux already has GNU tar).

```sh
# Static Linux (musl) binaries for all Synology targets
./cross_build_on_mac.sh

# Or just one target
./cross_build_on_mac.sh x86_64-unknown-linux-musl

# Build the .spk → dist/dsvideo_passthrough-<version>-<build>.spk
./make_spk.sh            # build number defaults to 0001
./make_spk.sh 0002
```

The crate is pure Rust with no OpenSSL or other C dependencies, so the musl
targets link with the `rust-lld` that ships with rustup. You don't need a
cross-compiling toolchain. The scripts work on macOS and Linux.

### Releases

[`.github/workflows/release.yml`](.github/workflows/release.yml) builds the
package on every push and pull request to `main`. To publish a release, bump
`version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.1.1 && git push origin v0.1.1
```

The workflow checks that the tag matches the `Cargo.toml` version, builds the
`.spk`, signs its provenance and attaches it with a `SHA256SUMS` file to the
release.

## Run manually

```sh
cargo run --release -- --listen 0.0.0.0:5080 --upstream http://127.0.0.1:5000
```

| Option       | Default                 | Meaning                                  |
|--------------|-------------------------|------------------------------------------|
| `--listen`   | `0.0.0.0:5080`          | Address the proxy listens on             |
| `--upstream` | `http://127.0.0.1:5000` | Video Station / DSM base URL (HTTP only) |

## End-to-end test

`selftest` imitates what DS Video does, so you can test without an Apple TV. It
logs in (you type the password; it isn't saved), opens an HEVC file with
`hls_remux` both directly and through the proxy, and checks that the proxied stream
returns a ranged MP4.

```sh
cargo run --release --bin selftest -- \
  --proxy    http://<nas>:5080 \
  --upstream http://<nas>:5000 \
  --account  <dsm-user> \
  --file-id  <video-station-file-id-of-an-hevc-file>
```

Expected output: the direct open fails with error 1211, the proxied open succeeds,
and the last line is `PASS`.

## Repository layout

| Path                     | Contents                                                      |
|--------------------------|---------------------------------------------------------------|
| `src/main.rs`            | The proxy                                                     |
| `src/bin/selftest.rs`    | End-to-end check against a real NAS                           |
| `synology/`              | DSM package sources: INFO, wizard, scripts, firewall service  |
| `cross_build_on_mac.sh`  | Cross-compiles the musl binaries                              |
| `make_spk.sh`            | Assembles the `.spk`                                          |
| `tools/savings.py`       | Estimates storage savings from re-encoding a library to HEVC  |

## Limitations

- Only plain `http://` upstreams are supported (the proxy is built without TLS).
  Keep it on the LAN, or put it behind a TLS-terminating reverse proxy.
- Raw passthrough sends the original file, so the client must be able to decode it
  (the Apple TV plays HEVC natively). No subtitle burn-in or audio transcoding
  happens in this mode.

## License

MIT, see [LICENSE](LICENSE).
