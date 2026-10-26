use rtl_spectrum_studio::{runtime::Runtime, device::{self,RtlLibrary}};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, atomic::Ordering};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const APP_ID: &str = "rtl-sdr-spectrum-studio";
const MAX_ACTION_BYTES: u64 = 128 * 1024;
const INDEX: &str = include_str!("../../web/index.html");
const CSS: &str = include_str!("../../web/app.css");
const JAVASCRIPT: &str = include_str!("../../web/app.js");

#[derive(Debug)]
struct Options {
    port: u16,
    project: Option<PathBuf>,
    open_browser: bool,
    self_check: bool,
}

fn parse_options() -> Result<Option<Options>, String> {
    let mut args = std::env::args().skip(1);
    let mut options = Options { port: 8765, project: None, open_browser: true, self_check: false };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                options.port = args.next().ok_or("--port requires a number")?
                    .parse::<u16>().map_err(|_| "Port must be between 0 and 65535")?;
            }
            "--project-dir" => options.project = Some(PathBuf::from(args.next().ok_or("--project-dir requires a path")?)),
            "--open-browser" => options.open_browser = true,
            "--no-open" => options.open_browser = false,
            "--self-check" => options.self_check = true,
            "--help" | "-h" => {
                println!("RTL-SDR Spectrum Studio\n  --project-dir <folder>\n  --port <number>       default 8765\n  --open-browser       show the local app (default)\n  --no-open            run without opening a browser\n  --self-check         check embedded UI and DLL without starting a server or receiver\n\nCtrl+C stops reception and safely finishes active recordings.");
                return Ok(None);
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    Ok(Some(options))
}

fn is_project(path: &Path) -> bool {
    let marker=path.join("studio-root.json");
    if marker.metadata().is_ok_and(|m|m.len()<=4096) {
        if let Ok(value)=std::fs::read(&marker).and_then(|bytes|serde_json::from_slice::<Value>(&bytes).map_err(std::io::Error::other)) {
            if value["app_id"].as_str()==Some(APP_ID)&&value["layout_version"].as_u64()==Some(1) {return true;}
        }
    }
    path.join("tools").is_dir() && std::fs::read_to_string(path.join("requirements.md"))
        .is_ok_and(|s| s.lines().next().unwrap_or_default().contains("RTL-SDR Spectrum Studio"))
}

fn project_folder(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = path.canonicalize().map_err(|e| format!("Cannot open project folder: {e}"))?;
        return if is_project(&path) { Ok(path) } else { Err("The folder must contain a valid studio-root.json or this app's requirements.md and tools directory".into()) };
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(exe.ancestors().skip(1).map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("rtl-sdr-spectrum-studio"));
        candidates.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    candidates.into_iter().find(|p| is_project(p))
        .ok_or_else(|| "Project folder not found. Pass --project-dir <folder>.".to_string())?
        .canonicalize().map_err(|e| e.to_string())
}

fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str())
}

fn valid_host(host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
        || (port == 80 && matches!(host.as_str(), "127.0.0.1" | "localhost"))
}

fn valid_origin(origin: &str, port: u16) -> bool {
    origin.strip_prefix("http://").is_some_and(|host| valid_host(host, port))
}

fn origin_allowed(path: &str, origin: Option<&str>, site: Option<&str>, port: u16) -> bool {
    // A browser may label ordinary navigation from a new tab as cross-site.
    // Serve the UI entry point; retain the stronger site check on all APIs.
    origin.is_none_or(|value| valid_origin(value, port))
        && !(path.starts_with("/api/") && site == Some("cross-site"))
}

fn common_headers(content_type: &str, dynamic: bool) -> Vec<Header> {
    [
        ("Content-Type", content_type),
        ("Cache-Control", if dynamic { "no-store" } else { "no-cache" }),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        ("Cross-Origin-Resource-Policy", "same-origin"),
        ("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"),
    ].into_iter().filter_map(|(key, value)| Header::from_bytes(key, value).ok()).collect()
}

fn send_bytes(request: Request, status: u16, body: Vec<u8>, content_type: &str, dynamic: bool) {
    let mut response = Response::from_data(body).with_status_code(StatusCode(status));
    for header in common_headers(content_type, dynamic) { response.add_header(header); }
    if let Err(error) = request.respond(response) { eprintln!("HTTP response: {error}"); }
}

fn send_json(request: Request, status: u16, body: Value) {
    match serde_json::to_vec(&body) {
        Ok(bytes) => send_bytes(request, status, bytes, "application/json; charset=utf-8", true),
        Err(_) => send_bytes(request, 500, br#"{"ok":false,"error":"Response serialization failed"}"#.to_vec(), "application/json", true),
    }
}

fn failure(request: Request, status: u16, code: &str, message: &str) {
    send_json(request, status, json!({"ok":false,"error":message,"message":message,"code":code}));
}

fn hex(byte: u8) -> Option<u8> {
    match byte { b'0'..=b'9' => Some(byte-b'0'), b'a'..=b'f' => Some(byte-b'a'+10), b'A'..=b'F' => Some(byte-b'A'+10), _ => None }
}

fn decode_query(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes(); let mut decoded = Vec::with_capacity(bytes.len()); let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index+2 >= bytes.len() { return Err("Invalid URL escape".into()); }
                let high = hex(bytes[index+1]).ok_or("Invalid URL escape")?;
                let low = hex(bytes[index+2]).ok_or("Invalid URL escape")?;
                let byte = high*16+low;
                if byte == 0 { return Err("A path cannot contain a null byte".into()); }
                decoded.push(byte); index += 3;
            }
            b'+' => { decoded.push(b' '); index += 1; }
            byte => { decoded.push(byte); index += 1; }
        }
    }
    String::from_utf8(decoded).map_err(|_| "URL must contain valid UTF-8".into())
}

fn query_value(query: &str, name: &str) -> Result<Option<String>, String> {
    for item in query.split('&') {
        let (key, value) = item.split_once('=').unwrap_or((item, ""));
        if decode_query(key)? == name { return decode_query(value).map(Some); }
    }
    Ok(None)
}

fn allowed_download(project: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested);
    let candidate = if requested.is_absolute() { requested } else { project.join(requested) };
    let candidate = candidate.canonicalize().map_err(|_| "保存ファイルが見つかりません")?;
    let permitted = ["data", "verification"].iter().filter_map(|folder| project.join(folder).canonicalize().ok())
        .any(|root| candidate.starts_with(root));
    if !permitted || !candidate.is_file() { return Err("このプロジェクトの保存データだけを取得できます".into()); }
    Ok(candidate)
}

fn percent_filename(name: &str) -> String {
    let mut out = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') { out.push(byte as char); }
        else { out.push_str(&format!("%{byte:02X}")); }
    }
    out
}

fn download(request: Request, runtime: &Runtime, query: &str) {
    let path = match query_value(query, "path").and_then(|value| value.ok_or_else(|| "path is required".into())).and_then(|value| allowed_download(&runtime.project, &value)) {
        Ok(path) => path, Err(error) => { failure(request, 403, "DOWNLOAD_DENIED", &error); return; }
    };
    let file = match File::open(&path) { Ok(file) => file, Err(error) => { failure(request, 404, "FILE_NOT_FOUND", &error.to_string()); return; } };
    let content_type = match path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase().as_str() {
        "wav" => "audio/wav", "json" | "sigmf-meta" => "application/json", _ => "application/octet-stream",
    };
    let mut response = Response::from_file(file);
    for header in common_headers(content_type, true) { response.add_header(header); }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let fallback: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    let disposition = format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{}", percent_filename(&name));
    if let Ok(header) = Header::from_bytes("Content-Disposition", disposition) { response.add_header(header); }
    if let Err(error) = request.respond(response) { eprintln!("Download response: {error}"); }
}

fn handle_request(mut request: Request, runtime: &Runtime, port: u16) {
    if !header_value(&request, "Host").is_some_and(|host| valid_host(host, port)) {
        failure(request, 403, "HOST_DENIED", "ローカルのアプリアドレスから接続してください"); return;
    }
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    if !origin_allowed(path, header_value(&request, "Origin"), header_value(&request, "Sec-Fetch-Site"), port) {
        failure(request, 403, "ORIGIN_DENIED", "別のサイトからの操作は許可されていません"); return;
    }
    match (request.method(), path) {
        (&Method::Get, "/") | (&Method::Get, "/index.html") => send_bytes(request, 200, INDEX.as_bytes().to_vec(), "text/html; charset=utf-8", false),
        (&Method::Get, "/app.css") => send_bytes(request, 200, CSS.as_bytes().to_vec(), "text/css; charset=utf-8", false),
        (&Method::Get, "/app.js") => send_bytes(request, 200, JAVASCRIPT.as_bytes().to_vec(), "text/javascript; charset=utf-8", false),
        (&Method::Get, "/favicon.ico") => send_bytes(request, 204, Vec::new(), "image/x-icon", false),
        (&Method::Get, "/api/health") => send_json(request, 200, json!({"app":APP_ID,"version":env!("CARGO_PKG_VERSION"),"project":runtime.project,"port":port})),
        (&Method::Get, "/api/state") => send_json(request, 200, runtime.state()),
        (&Method::Get, "/api/frame") => send_json(request, 200, runtime.frame()),
        (&Method::Get, "/api/files") => send_json(request, 200, runtime.files()),
        (&Method::Get, "/api/audio") => {
            let after = match query_value(query, "after").and_then(|s| s.unwrap_or_else(|| "0".into()).parse::<u64>().map_err(|_| "after must be a nonnegative integer".into())) {
                Ok(value) => value, Err(message) => { failure(request, 400, "BAD_QUERY", &message); return; }
            };
            send_json(request, 200, runtime.audio(after));
        }
        (&Method::Get, "/api/download") => download(request, runtime, query),
        (&Method::Post, "/api/action") => {
            if !header_value(&request, "Content-Type").is_some_and(|value| value.split(';').next().is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))) {
                failure(request, 415, "JSON_REQUIRED", "Content-Type: application/json を指定してください"); return;
            }
            if request.body_length().is_some_and(|length| length as u64 > MAX_ACTION_BYTES) {
                failure(request, 413, "BODY_TOO_LARGE", "操作データが大きすぎます"); return;
            }
            let mut bytes = Vec::new();
            if let Err(error) = request.as_reader().take(MAX_ACTION_BYTES+1).read_to_end(&mut bytes) { failure(request, 400, "READ_FAILED", &error.to_string()); return; }
            if bytes.len() as u64 > MAX_ACTION_BYTES { failure(request, 413, "BODY_TOO_LARGE", "操作データが大きすぎます"); return; }
            let body: Value = match serde_json::from_slice(&bytes) { Ok(value) => value, Err(error) => { failure(request, 400, "INVALID_JSON", &error.to_string()); return; } };
            if !body.is_object() { failure(request, 400, "INVALID_JSON", "JSONオブジェクトを指定してください"); return; }
            if body["action"].as_str() == Some("shutdown") {
                runtime.shutdown(); runtime.shutdown_requested.store(true, Ordering::Release);
                send_json(request, 200, json!({"ok":true,"shutdown":true})); return;
            }
            match runtime.action(body) {
                Ok(value) => send_json(request, 200, value),
                Err(error) => {
                    let status = if matches!(error.code.as_str(), "VFO_IMPACT" | "confirmation_required") { 409 } else { 400 };
                    send_json(request, status, json!({"ok":false,"error":&error.message,"message":&error.message,"code":&error.code,"impacted_vfos":&error.impacted_vfos}));
                }
            }
        }
        _ => failure(request, 404, "NOT_FOUND", "ページまたは操作が見つかりません"),
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("cmd.exe").args(["/d", "/c", "start", "", url]).creation_flags(0x08000000).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    { Command::new("open").arg(url).spawn().map_err(|e| e.to_string())?; }
    #[cfg(all(unix, not(target_os = "macos")))]
    { Command::new("xdg-open").arg(url).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

fn existing_app(port: u16, project: &Path) -> bool {
    let Ok(mut socket) = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)) else { return false; };
    let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(1)));
    let request = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if socket.write_all(request.as_bytes()).is_err() { return false; }
    let mut bytes = Vec::new();
    if socket.take(16384).read_to_end(&mut bytes).is_err() { return false; }
    let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else { return false; };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes[offset+4..]) else { return false; };
    value["app"].as_str() == Some(APP_ID) && value["project"].as_str().is_some_and(|p| Path::new(p) == project)
}

fn run() -> Result<(), String> {
    let Some(options) = parse_options()? else { return Ok(()); };
    let project = project_folder(options.project)?;
    if options.self_check {
        let dll=device::dll_path(&project);
        let library=RtlLibrary::load(&dll)?;
        let result=json!({"app":APP_ID,"version":env!("CARGO_PKG_VERSION"),"project":project,"driver_dll":dll,
            "driver_library_loaded":true,"device_count":library.devices().len(),"architecture":std::env::consts::ARCH,
            "embedded_ui_bytes":{"html":INDEX.len(),"css":CSS.len(),"javascript":JAVASCRIPT.len()},
            "server_started":false,"receiver_opened":false,"recording_started":false});
        println!("{}",serde_json::to_string_pretty(&result).map_err(|e|e.to_string())?);
        return Ok(());
    }
    let server = match Server::http(("127.0.0.1", options.port)) {
        Ok(server) => Arc::new(server),
        Err(error) => {
            if options.port != 0 && existing_app(options.port, &project) {
                let url = format!("http://127.0.0.1:{}/", options.port);
                println!("Spectrum Studio is already running: {url}");
                if options.open_browser { open_browser(&url)?; }
                return Ok(());
            }
            return Err(format!("Cannot listen on 127.0.0.1:{}: {error}\nChoose another port with --port.", options.port));
        }
    };
    let port = server.server_addr().to_ip().ok_or("Expected a local TCP listener")?.port();
    let runtime = Runtime::new(project)?;
    let signal_runtime = Arc::clone(&runtime);
    ctrlc::set_handler(move || { signal_runtime.shutdown(); signal_runtime.shutdown_requested.store(true, Ordering::Release); }).map_err(|e| e.to_string())?;
    let mut workers = Vec::new();
    for index in 0..4 {
        let server = Arc::clone(&server); let runtime = Arc::clone(&runtime);
        workers.push(thread::Builder::new().name(format!("studio-http-{index}")).spawn(move || {
            while !runtime.shutdown_requested.load(Ordering::Acquire) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(request)) => {
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle_request(request, &runtime, port))).is_err() { eprintln!("A request handler failed; the server remains available."); }
                    }
                    Ok(None) => {}
                    Err(error) => { eprintln!("HTTP listener: {error}"); break; }
                }
            }
        }).map_err(|e| e.to_string())?);
    }
    let url = format!("http://127.0.0.1:{port}/");
    println!("RTL-SDR Spectrum Studio {}\n{url}\nCtrl+C safely stops reception and finishes recordings.", env!("CARGO_PKG_VERSION"));
    if options.open_browser { if let Err(error) = open_browser(&url) { eprintln!("Open {url} in a browser. Browser launch: {error}"); } }
    while !runtime.shutdown_requested.load(Ordering::Acquire) { thread::sleep(Duration::from_millis(100)); }
    runtime.shutdown();
    for worker in workers { if worker.join().is_err() { eprintln!("A server worker exited unexpectedly."); } }
    Ok(())
}

fn main() {
    if let Err(error) = run() { eprintln!("Spectrum Studio: {error}"); std::process::exit(1); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_root_marker_requires_correct_identity_and_version() {
        let root=std::env::temp_dir().join(format!("spectrum-root-marker-{}",std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let marker=root.join("studio-root.json");
        std::fs::write(&marker,br#"{"app_id":"other-app","layout_version":1}"#).unwrap();
        assert!(!is_project(&root));
        std::fs::write(&marker,br#"{"app_id":"rtl-sdr-spectrum-studio","layout_version":2}"#).unwrap();
        assert!(!is_project(&root));
        std::fs::write(&marker,br#"{"app_id":"rtl-sdr-spectrum-studio","layout_version":1}"#).unwrap();
        assert!(is_project(&root));
        std::fs::remove_file(marker).unwrap();std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn loopback_host_and_origin_are_exact() {
        assert!(valid_host("127.0.0.1:8765",8765)); assert!(valid_host("LOCALHOST:8765",8765));
        for host in ["localhost.attacker:8765","192.0.2.1:8765","127.0.0.1:9000","127.0.0.1:8765@attacker"] { assert!(!valid_host(host,8765)); }
        assert!(valid_origin("http://localhost:8765",8765)); assert!(!valid_origin("https://localhost:8765",8765)); assert!(!valid_origin("null",8765));
        assert!(origin_allowed("/",None,Some("cross-site"),8765));
        assert!(origin_allowed("/app.js",None,Some("cross-site"),8765));
        assert!(!origin_allowed("/api/state",None,Some("cross-site"),8765));
        assert!(!origin_allowed("/api/action",Some("http://other-site.example"),Some("cross-site"),8765));
        assert!(origin_allowed("/api/action",Some("http://127.0.0.1:8765"),Some("same-origin"),8765));
    }
    #[test]
    fn query_decode_preserves_unicode_and_rejects_bad_escapes() {
        assert_eq!(decode_query("%E5%8F%97%E4%BF%A1+IQ%2B1").unwrap(),"受信 IQ+1");
        for value in ["%","%0","%GG","%FF","%00"] { assert!(decode_query(value).is_err()); }
        assert_eq!(query_value("after=42&path=data%2Ftest.iq","after").unwrap(),Some("42".into()));
    }
    #[test]
    fn download_is_confined_to_data_or_verification() {
        let root = std::env::temp_dir().join(format!("spectrum-path-test-{}",std::process::id()));
        std::fs::create_dir_all(root.join("data")).unwrap(); std::fs::create_dir_all(root.join("verification")).unwrap();
        std::fs::write(root.join("data/test.sigmf-meta"),b"{}").unwrap(); std::fs::write(root.join("requirements.md"),b"reference").unwrap();
        assert!(allowed_download(&root,"data/test.sigmf-meta").is_ok());
        assert!(allowed_download(&root,"data/../requirements.md").is_err()); assert!(allowed_download(&root,"requirements.md").is_err());
        // Temporary test data only; leave no files behind after the test.
        std::fs::remove_file(root.join("data/test.sigmf-meta")).unwrap(); std::fs::remove_file(root.join("requirements.md")).unwrap();
        std::fs::remove_dir(root.join("data")).unwrap();std::fs::remove_dir(root.join("verification")).unwrap();std::fs::remove_dir(root).unwrap();
    }
}
