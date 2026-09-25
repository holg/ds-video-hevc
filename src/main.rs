//! dsvideo-passthrough — a transparent proxy in front of Synology Video Station.
//!
//! DS Video (tvOS) asks Video Station to open a stream with `hls_remux`. For HEVC
//! sources on CPUs without an HEVC profile (e.g. cedarview), Video Station answers
//! error 1211, DS Video falls back to a full transcode and the NAS cannot keep up
//! (black screen). This proxy catches that 1211, re-opens the same file in `raw`
//! mode (what the web UI does) and hands DS Video the raw stream id as if it were
//! an `hls_remux` stream. The player then receives the original file, untouched.

use std::collections::HashSet;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use clap::Parser;
use futures_util::TryStreamExt;

#[derive(Parser)]
struct Args {
    /// Address the proxy listens on (point DS Video at this host:port).
    #[arg(long, default_value = "0.0.0.0:5080")]
    listen: SocketAddr,
    /// Video Station / DSM base URL.
    #[arg(long, default_value = "http://127.0.0.1:5000")]
    upstream: String,
}

struct AppState {
    upstream: String,
    client: reqwest::Client,
    /// Stream ids we opened in raw mode on behalf of DS Video.
    raw_streams: Mutex<HashSet<String>>,
}

type Shared = Arc<AppState>;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

const OPEN_API: &str = "SYNO.VideoStation2.Streaming";
const ERR_NO_STREAM_FORMAT: i64 = 1211;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let state = Arc::new(AppState {
        upstream: args.upstream.trim_end_matches('/').to_string(),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http client"),
        raw_streams: Mutex::new(HashSet::new()),
    });
    let app = axum::Router::new().fallback(handle).with_state(state);
    let listener = tokio::net::TcpListener::bind(args.listen).await.expect("bind");
    eprintln!("dsvideo-passthrough listening on {} -> {}", args.listen, args.upstream);
    axum::serve(listener, app).await.expect("serve");
}

async fn handle(State(st): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, 1 << 20).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::PAYLOAD_TOO_LARGE, e.to_string()).into_response(),
    };
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().unwrap_or("").to_string();
    let params = merged_params(&query, &parts.headers, &body);

    log_request(&parts.method, &path, &params);

    // 1. Streaming.open with hls_remux → retry as raw when Video Station refuses.
    if parts.method == Method::POST
        && path == "/webapi/entry.cgi"
        && param(&params, "api") == Some(OPEN_API)
        && param(&params, "method") == Some("open")
        && param(&params, "hls_remux").is_some()
    {
        return open_with_raw_fallback(&st, &parts.headers, &query, &body, &params).await;
    }

    // 2. Stream / close for a stream id we opened as raw → switch format to raw.
    if path.starts_with("/webapi/VideoStation/vtestreaming.cgi") {
        if let Some(id) = param(&params, "id").map(unquote) {
            if st.raw_streams.lock().unwrap().contains(id) {
                let method = param(&params, "method").unwrap_or("");
                let new_path = if method == "stream" {
                    "/webapi/VideoStation/vtestreaming.cgi/1.mp4".to_string()
                } else {
                    path.clone()
                };
                if method == "close" {
                    st.raw_streams.lock().unwrap().remove(id);
                }
                eprintln!("   ↳ raw passthrough: {method} {id}");
                let (q, b) = if parts.method == Method::GET || parts.method == Method::HEAD {
                    (set_param(&query, "format", "raw"), body.clone())
                } else {
                    let form = set_param(&String::from_utf8_lossy(&body), "format", "raw");
                    (query.clone(), Bytes::from(form))
                };
                return forward_streaming(&st, parts.method, &new_path, &q, &parts.headers, b).await;
            }
        }
    }

    // API calls (small JSON): buffer them so the outcome can be logged.
    let is_stream = param(&params, "method") == Some("stream") || path.contains("vtestreaming.cgi/");
    if path.starts_with("/webapi/") && !is_stream {
        return match forward_buffered(&st, parts.method, &path, &query, &parts.headers, body).await {
            Ok(r) => {
                log_response(&r);
                r.into_response()
            }
            Err(e) => bad_gateway(e),
        };
    }

    forward_streaming(&st, parts.method, &path, &query, &parts.headers, body).await
}

fn log_response(r: &Buffered) {
    let outcome = match decode_json(&r.headers, &r.body) {
        Some(j) if j["success"] == true => "ok".to_string(),
        Some(j) => format!("error {}", j["error"]),
        None => format!(
            "{} bytes, {}",
            r.body.len(),
            r.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("?")
        ),
    };
    eprintln!("   ← {} {outcome}", r.status);
}

async fn open_with_raw_fallback(
    st: &AppState,
    headers: &HeaderMap,
    query: &str,
    body: &Bytes,
    params: &[(String, String)],
) -> Response {
    let first = match forward_buffered(st, Method::POST, "/webapi/entry.cgi", query, headers, body.clone()).await {
        Ok(r) => r,
        Err(e) => return bad_gateway(e),
    };
    let json = decode_json(&first.headers, &first.body);
    let refused = json.as_ref().is_some_and(|j| {
        j["success"] == false && j["error"]["code"].as_i64() == Some(ERR_NO_STREAM_FORMAT)
    });
    if !refused {
        return first.into_response();
    }

    let file = param(params, "file").unwrap_or("?");
    eprintln!("   ↳ hls_remux refused (1211) for file {file}; retrying as raw");

    // Same form, but raw={} instead of hls_remux={...}.
    let raw_form: String = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            form_urlencoded::parse(body)
                .filter(|(k, _)| k != "hls_remux")
                .chain(std::iter::once(("raw".into(), "{}".into()))),
        )
        .finish();
    let second = match forward_buffered(st, Method::POST, "/webapi/entry.cgi", query, headers, Bytes::from(raw_form)).await {
        Ok(r) => r,
        Err(e) => return bad_gateway(e),
    };
    let Some(j) = decode_json(&second.headers, &second.body) else {
        return first.into_response();
    };
    let Some(stream_id) = j["data"]["stream_id"].as_str().filter(|_| j["success"] == true) else {
        eprintln!("   ↳ raw open failed too: {j}");
        return first.into_response();
    };
    eprintln!("   ↳ raw open ok: stream_id {stream_id} ({})", j["data"]);
    st.raw_streams.lock().unwrap().insert(stream_id.to_string());

    let reply = serde_json::json!({
        "data": { "format": "hls_remux", "stream_id": stream_id },
        "success": true
    });
    ([(header::CONTENT_TYPE, "application/json; charset=\"UTF-8\"")], reply.to_string()).into_response()
}

// ---------- forwarding ----------

struct Buffered {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl IntoResponse for Buffered {
    fn into_response(self) -> Response {
        let mut resp = Response::new(Body::from(self.body));
        *resp.status_mut() = self.status;
        copy_headers(&self.headers, resp.headers_mut());
        resp
    }
}

fn build_request(
    st: &AppState,
    method: Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> reqwest::RequestBuilder {
    let url = if query.is_empty() {
        format!("{}{}", st.upstream, path)
    } else {
        format!("{}{}?{}", st.upstream, path, query)
    };
    let mut out = HeaderMap::new();
    // Host is kept: Video Station builds absolute URLs (playlists) from it,
    // so follow-up requests come back through the proxy.
    copy_headers(headers, &mut out);
    st.client.request(method, url).headers(out).body(body)
}

async fn forward_buffered(
    st: &AppState,
    method: Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Buffered, reqwest::Error> {
    let resp = build_request(st, method, path, query, headers, body).send().await?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.bytes().await?;
    Ok(Buffered { status, headers, body })
}

async fn forward_streaming(
    st: &AppState,
    method: Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let resp = match build_request(st, method, path, query, headers, body).send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(e),
    };
    let status = resp.status();
    let upstream_headers = resp.headers().clone();
    let len = resp.content_length();
    let mut out = Response::new(Body::from_stream(resp.bytes_stream().map_err(std::io::Error::other)));
    *out.status_mut() = status;
    copy_headers(&upstream_headers, out.headers_mut());
    if let Some(len) = len {
        out.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    out
}

fn copy_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (k, v) in from {
        if !HOP_BY_HOP.contains(&k.as_str()) {
            to.append(k.clone(), v.clone());
        }
    }
}

fn bad_gateway(e: reqwest::Error) -> Response {
    eprintln!("   !! upstream error: {e}");
    (StatusCode::BAD_GATEWAY, e.to_string()).into_response()
}

// ---------- params / json helpers ----------

fn merged_params(query: &str, headers: &HeaderMap, body: &Bytes) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|c| c.to_str().ok())
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    if is_form {
        v.extend(form_urlencoded::parse(body).into_owned());
    }
    v
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn unquote(s: &str) -> &str {
    s.trim_matches('"')
}

/// Replace (or add) one key in an urlencoded string, keeping the other pairs.
fn set_param(encoded: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in form_urlencoded::parse(encoded.as_bytes()) {
        if k == key {
            found = true;
            ser.append_pair(&k, value);
        } else {
            ser.append_pair(&k, &v);
        }
    }
    if !found {
        ser.append_pair(key, value);
    }
    ser.finish()
}

fn decode_json(headers: &HeaderMap, body: &Bytes) -> Option<serde_json::Value> {
    let gz = headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"gzip"));
    let raw = if gz {
        let mut s = Vec::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut s).ok()?;
        s
    } else {
        body.to_vec()
    };
    serde_json::from_slice(&raw).ok()
}

fn log_request(method: &Method, path: &str, params: &[(String, String)]) {
    let pick = |k| param(params, k).unwrap_or("");
    let api = pick("api");
    if api.is_empty() && !path.starts_with("/webapi") {
        return;
    }
    eprintln!(
        "{method} {path} api={api} method={} format={} id={}{}",
        pick("method"),
        pick("format"),
        unquote(pick("id")),
        param(params, "fragment_id").map(|f| format!(" frag={}", unquote(f))).unwrap_or_default(),
    );
}
