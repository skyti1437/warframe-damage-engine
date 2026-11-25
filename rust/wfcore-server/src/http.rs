use crate::{catalog, data::{EngineData, Result}};
use serde_json::{json, Value};
use std::io::{self, Cursor, Read};
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Request, Server, StatusCode};

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const CONTENT_TYPE: &str = "application/json; charset=utf-8";

pub struct Service {
    pub data: EngineData,
    health_json: Arc<Vec<u8>>,
    catalog_json: Arc<Vec<u8>>,
}

impl Service {
    pub fn new(data: EngineData) -> Result<Self> {
        let health_json = Arc::new(serde_json::to_vec(&data.health())?);
        let catalog_json = Arc::new(serde_json::to_vec(&catalog::build(&data)?)?);
        Ok(Self { data, health_json, catalog_json })
    }

    pub fn health_json(&self) -> &[u8] { &self.health_json }

    pub fn catalog_json(&self) -> &[u8] { &self.catalog_json }

    fn dispatch(&self, method: &Method, target: &str, length: Option<&str>, body: impl Read) -> Reply {
        let path = target.split(['?', '#']).next().unwrap_or_default();
        match (method, path) {
            (Method::Get, "/api/health") => Reply { status: 200, cache: "no-store", body: Arc::clone(&self.health_json) },
            (Method::Get, "/api/catalog") => Reply { status: 200, cache: "no-cache", body: Arc::clone(&self.catalog_json) },
            (Method::Post, "/api/simulate" | "/api/build" | "/api/analyze" | "/api/trait-matrix") => {
                match parse_object(length, body) {
                    Err(reply) => reply,
                    Ok(_) => Reply::error(501, "not_implemented", "endpoint is not implemented in Phase 2a"),
                }
            }
            _ => Reply::error(404, "not_found", "endpoint not found"),
        }
    }

    fn handle_request(&self, request: &mut Request) -> Reply {
        let method = request.method().clone();
        let target = request.url().to_owned();
        let headers = request.headers();
        let mut lengths = headers.iter().filter(|header| header.field.equiv("Content-Length"));
        let length = lengths.next().map(|header| header.value.as_str().to_owned())
            .filter(|_| lengths.next().is_none() && !headers.iter().any(|header| header.field.equiv("Transfer-Encoding")));
        // tiny_http 会在首次调用 as_reader 时发送 100 Continue。先经过校验，
        // 只有允许读取的请求才取得读取器。
        let body = DeferredBody(request);
        self.dispatch(&method, &target, length.as_deref(), body)
    }

    fn respond(&self, mut request: Request) -> io::Result<()> {
        let reply = self.handle_request(&mut request);
        request.respond(reply.into_http())
    }

    pub fn serve(self, address: SocketAddr, workers: usize) -> Result<()> {
        let state = Arc::new(self);
        let server = Arc::new(Server::http_with_body_limit(address, MAX_REQUEST_BYTES)?);
        let mut threads = Vec::with_capacity(workers);
        for index in 0..workers {
            let state = Arc::clone(&state);
            let server = Arc::clone(&server);
            threads.push(thread::Builder::new().name(format!("wf-http-{index}")).spawn(move || {
                while let Ok(request) = server.recv() {
                    if let Err(error) = state.respond(request) {
                        eprintln!("发送响应失败：{error}");
                    }
                }
            })?);
        }
        eprintln!("wfcore-server 监听 {}，工作线程 {workers}", server.server_addr());
        for worker in threads {
            worker.join().map_err(|_| io::Error::other("HTTP 工作线程异常退出"))?;
        }
        Ok(())
    }
}

struct DeferredBody<'a>(&'a mut Request);

impl Read for DeferredBody<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.as_reader().read(buffer)
    }
}

struct Reply {
    status: u16,
    cache: &'static str,
    body: Arc<Vec<u8>>,
}

impl Reply {
    fn error(status: u16, code: &str, message: &str) -> Self {
        let body = json!({"error": {"code": code, "message": message}});
        Self {
            status, cache: "no-store",
            body: Arc::new(serde_json::to_vec(&body).expect("错误响应可序列化")),
        }
    }

    fn into_http(self) -> tiny_http::Response<Cursor<SharedBytes>> {
        let length = self.body.len();
        tiny_http::Response::new(
            StatusCode(self.status),
            vec![
                Header::from_bytes("Content-Type", CONTENT_TYPE).unwrap(),
                Header::from_bytes("Cache-Control", self.cache).unwrap(),
            ],
            Cursor::new(SharedBytes(self.body)),
            Some(length),
            None,
        )
    }
}

struct SharedBytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] { &self.0 }
}

fn parse_length(raw: Option<&str>) -> std::result::Result<usize, Reply> {
    let invalid = || Reply::error(400, "invalid_length", "invalid Content-Length");
    let raw = raw.ok_or_else(invalid)?.trim();
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let length = if raw.starts_with('-') { 0 } else { digits.parse::<usize>().unwrap_or(usize::MAX) };
    if !(1..=MAX_REQUEST_BYTES).contains(&length) {
        return Err(Reply::error(413, "request_too_large", "JSON body must be between 1 and 131072 bytes"));
    }
    Ok(length)
}

fn parse_object(length: Option<&str>, body: impl Read) -> std::result::Result<serde_json::Map<String, Value>, Reply> {
    let length = parse_length(length)?;
    let mut bytes = Vec::with_capacity(length);
    body.take(length as u64).read_to_end(&mut bytes).map_err(|error| {
        Reply::error(400, "invalid_json", &error.to_string())
    })?;
    if bytes.len() != length {
        return Err(Reply::error(400, "invalid_json", "incomplete JSON body"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| Reply::error(400, "invalid_json", &error.to_string()))?;
    let payload: Value = serde_json::from_str(text).map_err(|error| Reply::error(400, "invalid_json", &error.to_string()))?;
    match payload {
        Value::Object(fields) => Ok(fields),
        _ => Err(Reply::error(422, "invalid_request", "JSON body must be an object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::BUILD_LABEL;
    use std::path::Path;
    use std::sync::OnceLock;

    fn state() -> &'static Service {
        static SERVICE: OnceLock<Service> = OnceLock::new();
        SERVICE.get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
            Service::new(EngineData::load(root).expect("加载仓库数据")).expect("构建目录")
        })
    }

    #[test]
    fn load_repository_data_and_build_catalog() {
        let state = state();
        let catalog: Value = serde_json::from_slice(state.catalog_json()).unwrap();
        assert_eq!(catalog["buildLabel"], BUILD_LABEL);
        assert_eq!(catalog["schemaVersion"], 3);
        assert_eq!(catalog["manualCounts"], json!({"mods": 406, "arcanes": 68, "enemies": 1351}));
        for (name, count) in [("weapons", 684), ("manualMods", 406), ("arcanes", 68), ("enemies", 1351)] {
            assert_eq!(catalog[name].as_array().unwrap().len(), count);
        }
        assert!(catalog["stances"].as_array().is_some_and(|rows| !rows.is_empty()));
        assert!(catalog["utilityMods"].as_array().is_some_and(|rows| !rows.is_empty()));
        assert!(catalog["statsByType"]["rifle"].as_array().is_some_and(|rows| !rows.is_empty()));
        let duviri = "/Lotus/Types/Enemies/Duviri/Avatars/DuviriMeleeAvatar";
        assert_eq!(state.data.enemies[duviri]["defenseSources"]["baseLevel"], "agent");
        assert!(state.data.weapons.values().any(|weapon| weapon["nameEn"] == "Burston Prime"));
        assert!(state.data.riven_templates["templates"].is_array());
        assert!(state.data.incarnon_defaults["strategies"].is_object());
    }

    #[test]
    fn health_and_catalog_reuse_serialized_bytes() {
        let state = state();
        let health = state.dispatch(&Method::Get, "/api/health?q=1", None, io::empty());
        assert_eq!((health.status, health.cache), (200, "no-store"));
        let payload: Value = serde_json::from_slice(&health.body).unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["buildLabel"], BUILD_LABEL);
        assert_eq!(payload["schemaVersion"], 3);
        let fingerprint = payload["engineFingerprint"].as_str().unwrap();
        assert_eq!(fingerprint.len(), 16);
        assert!(fingerprint.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        let left = state.dispatch(&Method::Get, "/api/catalog", None, io::empty());
        let right = state.dispatch(&Method::Get, "/api/catalog", None, io::empty());
        assert_eq!((left.status, left.cache), (200, "no-cache"));
        assert!(Arc::ptr_eq(&left.body, &right.body));
        for (reply, cache) in [(health, "no-store"), (left, "no-cache")] {
            let response = reply.into_http();
            assert!(response.headers().iter().any(|header| {
                header.field.equiv("Content-Type") && header.value.as_str() == CONTENT_TYPE
            }));
            assert!(response.headers().iter().any(|header| {
                header.field.equiv("Cache-Control") && header.value.as_str() == cache
            }));
        }
    }

    #[test]
    fn routing_and_request_validation() {
        let state = state();
        let check = |method, path, length, body, status, code| {
            let reply = state.dispatch(method, path, length, body);
            assert_eq!((reply.status, reply.cache), (status, "no-store"));
            let payload: Value = serde_json::from_slice(&reply.body).unwrap();
            assert_eq!(payload["error"]["code"], Value::String(code));
            assert!(payload["error"]["message"].as_str().is_some_and(|v| !v.is_empty()));
        };
        for (length, body, status, code) in [
            (None, b"".as_slice(), 400, "invalid_length"),
            (Some("NaN"), b"".as_slice(), 400, "invalid_length"),
            (Some("0"), b"".as_slice(), 413, "request_too_large"),
            (Some("131073"), b"".as_slice(), 413, "request_too_large"),
            (Some("999999999999999999999999"), b"".as_slice(), 413, "request_too_large"),
            (Some("1"), b"\xff".as_slice(), 400, "invalid_json"),
            (Some("1"), b"{".as_slice(), 400, "invalid_json"),
            (Some("2"), b"[".as_slice(), 400, "invalid_json"),
            (Some("2"), b"[]".as_slice(), 422, "invalid_request"),
        ] {
            check(&Method::Post, "/api/simulate", length, body, status, code.to_owned());
        }
        for path in ["/api/simulate", "/api/build", "/api/analyze", "/api/trait-matrix"] {
            check(&Method::Post, path, Some("2"), b"{}".as_slice(), 501, "not_implemented".to_owned());
        }
        check(&Method::Get, "/missing", None, b"".as_slice(), 404, "not_found".to_owned());
        check(&Method::Post, "/missing", None, b"".as_slice(), 404, "not_found".to_owned());
        let mut max_body = vec![b' '; MAX_REQUEST_BYTES];
        max_body[..2].copy_from_slice(b"{}");
        check(&Method::Post, "/api/simulate", Some("131072"), &max_body, 501, "not_implemented".to_owned());
    }

    #[test]
    fn transport_keeps_oversized_length_for_the_json_error() {
        // 模拟只送来头的客户端。长度上限在 tiny_http 建立 body 读取器前生效。
        let mut request: Request = tiny_http::TestRequest::new()
            .with_method(Method::Post)
            .with_path("/api/simulate")
            .with_header(Header::from_bytes("Content-Length", usize::MAX.to_string()).unwrap())
            .with_body_limit(MAX_REQUEST_BYTES)
            .into();
        let reply = state().handle_request(&mut request);
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(reply.status, 413);
        assert_eq!(body["error"]["code"], "request_too_large");
        let response = reply.into_http();
        assert!(response.headers().iter().any(|header| {
            header.field.equiv("Cache-Control") && header.value.as_str() == "no-store"
        }));
        request.respond(response).unwrap();
    }
}
