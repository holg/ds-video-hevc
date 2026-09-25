//! End-to-end check of the passthrough, without the Apple TV.
//!
//! Logs in (password is prompted, never stored), then plays DS Video's part:
//! open with `hls_remux` → must succeed through the proxy (Video Station alone
//! answers 1211 for HEVC) → fetch the first KiB of the stream → must be MP4.

use clap::Parser;

#[derive(Parser)]
struct Args {
    /// Proxy base URL (what DS Video talks to).
    #[arg(long, default_value = "http://127.0.0.1:5000")]
    proxy: String,
    /// Video Station base URL (to confirm the direct path still refuses).
    #[arg(long)]
    upstream: String,
    /// DSM account to log in with.
    #[arg(long)]
    account: String,
    /// Video Station file id of an HEVC file.
    #[arg(long)]
    file_id: u64,
}

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Res<()> {
    let a = Args::parse();
    let pw = rpassword::prompt_password(format!("Password for {}: ", a.account))?;
    let http = reqwest::Client::new();

    let login = post(&http, &a.upstream, &[
        ("api", "SYNO.API.Auth"), ("version", "6"), ("method", "login"),
        ("account", &a.account), ("passwd", &pw), ("session", "VideoStation"), ("format", "sid"),
    ]).await?;
    drop(pw);
    let sid = login["data"]["sid"].as_str().ok_or_else(|| format!("login failed: {}", login["error"]))?.to_string();
    println!("login ok");

    let file = format!("{{\"id\":{}}}", a.file_id);
    let remux = r#"{"device":"tvos","force_open_vte":false,"audio_track":1,"profile":""}"#;
    let open = |base: String| {
        let (http, sid, file) = (http.clone(), sid.clone(), file.clone());
        async move {
            post(&http, &base, &[
                ("api", "SYNO.VideoStation2.Streaming"), ("version", "1"), ("method", "open"),
                ("file", &file), ("hls_remux", remux), ("_sid", &sid),
            ]).await
        }
    };

    let direct = open(a.upstream.clone()).await?;
    println!("direct  open hls_remux: {}", summary(&direct));

    let proxied = open(a.proxy.clone()).await?;
    println!("proxied open hls_remux: {}", summary(&proxied));
    let result = check_stream(&http, &a.proxy, &proxied, &sid).await;

    let _ = post(&http, &a.upstream, &[
        ("api", "SYNO.API.Auth"), ("version", "6"), ("method", "logout"), ("session", "VideoStation"), ("_sid", &sid),
    ]).await;
    println!("logged out");
    result
}

async fn check_stream(http: &reqwest::Client, proxy: &str, open: &serde_json::Value, sid: &str) -> Res<()> {
    let id = open["data"]["stream_id"].as_str().ok_or("proxied open did not return a stream_id")?;
    let url = format!(
        "{proxy}/webapi/VideoStation/vtestreaming.cgi/DTV.mov?api=SYNO.VideoStation.Streaming&version=1&method=stream&id={id}&format=hls_remux&_sid={sid}"
    );
    let resp = http.get(&url).header("Range", "bytes=0-1023").send().await?;
    let status = resp.status();
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("?").to_string();
    let range = resp.headers().get("content-range").and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
    let head = resp.bytes().await?;
    let is_mp4 = head.len() >= 12 && &head[4..8] == b"ftyp";
    println!("stream: {status}, {ctype}, content-range {range}, {} bytes, mp4 header: {is_mp4}", head.len());

    let close = format!(
        "{proxy}/webapi/VideoStation/vtestreaming.cgi?api=SYNO.VideoStation.Streaming&version=1&method=close&id={id}&format=hls_remux&_sid={sid}"
    );
    let _ = http.get(&close).send().await;

    if status.as_u16() == 206 && is_mp4 { println!("PASS"); Ok(()) } else { Err("FAIL: stream is not a ranged MP4".into()) }
}

async fn post(http: &reqwest::Client, base: &str, pairs: &[(&str, &str)]) -> Res<serde_json::Value> {
    let body = form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
    let resp = http
        .post(format!("{base}/webapi/entry.cgi"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    Ok(serde_json::from_slice(&resp.bytes().await?)?)
}

fn summary(j: &serde_json::Value) -> String {
    if j["success"] == true { format!("ok, format {}", j["data"]["format"]) } else { format!("error {}", j["error"]) }
}
