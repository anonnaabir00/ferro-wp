pub mod fastcgi;
pub mod tls;

use ferro_core::config::Config;
use ferro_core::site::{Registry, Site, SiteKind};
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{HeaderValue, HOST};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::ServerConfig;
use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tokio_util::io::ReaderStream;
use tracing::{debug, info, warn};

use tls::Ca;

const BODY_LIMIT: u64 = 1024 * 1024 * 1024; // 1 GB uploads
const FCGI_TIMEOUT: Duration = Duration::from_secs(300);

pub struct RouterState {
    pub registry: Arc<RwLock<Registry>>,
    pub config: Arc<RwLock<Config>>,
    pub ca: Arc<Ca>,
}

#[derive(Clone)]
pub struct Router {
    state: Arc<RouterState>,
}

type BoxedBody = BoxBody<Bytes, std::io::Error>;

impl Router {
    pub fn new(state: Arc<RouterState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// Plain HTTP: health, CA download, and a redirect to HTTPS for
    /// everything else.
    pub async fn run_http(self: Arc<Self>, shutdown: watch::Receiver<()>) -> std::io::Result<()> {
        let port = self.state.config.read().unwrap().http_port;
        let listeners = bind_loopback(port).await?;
        info!("http listener on 127.0.0.1:{port} (redirects to https)");
        run_listeners(
            listeners,
            shutdown,
            move |router, stream| {
                tokio::spawn(async move {
                    serve_conn(router, stream, Scheme::Http).await;
                });
            },
            self.clone(),
        )
        .await
    }

    /// HTTPS: TLS termination with per-site certs, static files, FastCGI.
    pub async fn run_https(self: Arc<Self>, shutdown: watch::Receiver<()>) -> std::io::Result<()> {
        let port = self.state.config.read().unwrap().https_port;
        let listeners = bind_loopback(port).await?;
        let tls_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(self.state.ca.clone());
        let acceptor = TlsAcceptor::from(Arc::new(tls_config));
        info!("https listener on 127.0.0.1:{port}");
        run_listeners(
            listeners,
            shutdown,
            move |router, stream| {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => serve_conn(router, tls_stream, Scheme::Https).await,
                        Err(err) => debug!("tls handshake failed: {err}"),
                    }
                });
            },
            self.clone(),
        )
        .await
    }

    async fn respond(&self, req: Request<Incoming>, scheme: Scheme) -> Response<BoxedBody> {
        let host = host_of(&req);
        if host == "ferro.localhost" {
            return self.respond_meta(&req);
        }

        if scheme == Scheme::Http {
            // Everything else on plain HTTP upgrades to HTTPS.
            let https_port = self.state.config.read().unwrap().https_port;
            let location = format!(
                "https://{host}:{https_port}{}",
                req.uri()
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/")
            );
            return redirect(&location);
        }

        let site = match self.resolve_site(&host) {
            Some(site) => site,
            None => return self.no_such_site(&host),
        };
        if !site.running {
            return text(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!(
                    "ferro: site `{}` is stopped — start it with `ferro start {}`\n",
                    site.name, site.name
                ),
            );
        }
        self.respond_site(&site, req).await
    }

    fn respond_meta(&self, req: &Request<Incoming>) -> Response<BoxedBody> {
        match req.uri().path() {
            "/ca" => {
                let pem = self.state.ca.cert_pem().to_string();
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/x-x509-ca-cert")
                    .header("content-disposition", "attachment; filename=ferro-ca.pem")
                    .body(full(pem.into_bytes()))
                    .unwrap()
            }
            "/" => {
                let registry = self.state.registry.read().unwrap();
                let config = self.state.config.read().unwrap();
                let mut body = String::from("ferro ok\n\nsites:\n");
                for site in &registry.sites {
                    body.push_str(&format!(
                        "  {} -> {}\n",
                        site.name,
                        site.url(config.https_port)
                    ));
                }
                text(StatusCode::OK, &body)
            }
            _ => text(StatusCode::OK, "ferro ok\n"),
        }
    }

    fn resolve_site(&self, host: &str) -> Option<Site> {
        {
            let registry = self.state.registry.read().unwrap();
            if let Some(site) = registry.by_host(host) {
                return Some(site.clone());
            }
        }
        // Parked fallback: an unregistered folder in a parked path still
        // gets served ("drop a folder in ~/Sites and it works").
        let name = host.strip_suffix(".localhost")?;
        if name.is_empty() || Site::validate_name(name).is_err() {
            return None;
        }
        let config = self.state.config.read().unwrap();
        for parked in &config.parked_paths {
            let candidate = parked.join(name);
            if candidate.is_dir() {
                return Some(Site {
                    name: name.to_string(),
                    path: candidate,
                    php_version: config.default_php.clone(),
                    kind: SiteKind::Parked,
                    running: true,
                    xdebug: false,
                    db_name: None,
                    db_user: None,
                    db_pass: None,
                    media_proxy: None,
                });
            }
        }
        None
    }

    /// WordPress-aware serving: existing files go out directly, .php
    /// files run through the site's pool, everything else falls through
    /// to /index.php (pretty permalinks).
    async fn respond_site(&self, site: &Site, req: Request<Incoming>) -> Response<BoxedBody> {
        let uri = req.uri().clone();
        let path = uri.path();
        let Some(relative) = sanitize(path) else {
            return text(StatusCode::BAD_REQUEST, "invalid path\n");
        };
        let root = &site.path;
        let candidate = root.join(&relative);

        if candidate.is_file() {
            if candidate.extension().is_some_and(|e| e == "php") {
                return self.run_php(site, &candidate, req).await;
            }
            return serve_file(&candidate).await;
        }
        if candidate.is_dir() {
            let index_php = candidate.join("index.php");
            if index_php.is_file() {
                return self.run_php(site, &index_php, req).await;
            }
            let index_html = candidate.join("index.html");
            if index_html.is_file() {
                return serve_file(&index_html).await;
            }
            return text(StatusCode::FORBIDDEN, "403 — directory listing is off\n");
        }

        // Missing file: media proxy for uploads, else WP front controller.
        if let Some(proxy) = &site.media_proxy {
            if let Some(origin) = proxy_origin_if_uploads(path, proxy) {
                if proxy.mode == "proxy" {
                    return self
                        .proxy_fetch(&site.name, &origin, path, proxy.cache)
                        .await;
                }
                return redirect(&origin);
            }
        }

        let front = root.join("index.php");
        if front.is_file() {
            return self.run_php(site, &front, req).await;
        }
        text(StatusCode::NOT_FOUND, &format!("404 — no file at {path}\n"))
    }

    async fn run_php(
        &self,
        site: &Site,
        script: &Path,
        req: Request<Incoming>,
    ) -> Response<BoxedBody> {
        let socket = ferro_core::paths::pool_socket(&site.name);
        if !socket.exists() {
            return text(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "ferro: php-fpm pool for `{}` is not reachable ({})\n\
                     fix: run `ferro doctor` — the pool may still be starting, \
                     or PHP {} failed to launch (see ~/Library/Logs/ferro/)\n",
                    site.name,
                    socket.display(),
                    site.php_version
                ),
            );
        }

        let body_bytes = match collect_body(req).await {
            Ok(b) => b,
            Err(response) => return *response,
        };
        let params = self.fcgi_params(site, script, &body_bytes.headers, &body_bytes);
        match tokio::time::timeout(
            FCGI_TIMEOUT,
            fastcgi::request(&socket, &params, &body_bytes.bytes),
        )
        .await
        {
            Ok(Ok(result)) => {
                if !result.stderr.is_empty() {
                    debug!(site = %site.name, "php-fpm stderr: {}", result.stderr.trim());
                }
                fcgi_to_response(&result.stdout)
            }
            Ok(Err(err)) => text(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "ferro: FastCGI error for `{}`: {err}\nfix: check ~/Library/Logs/ferro/{}-php.log\n",
                    site.name, site.name
                ),
            ),
            Err(_) => text(
                StatusCode::GATEWAY_TIMEOUT,
                &format!("ferro: PHP request for `{}` timed out\n", site.name),
            ),
        }
    }

    fn fcgi_params(
        &self,
        site: &Site,
        script: &Path,
        headers: &hyper::HeaderMap,
        body: &CollectedBody,
    ) -> Vec<(String, String)> {
        let config = self.state.config.read().unwrap();
        let root = site.path.display().to_string();
        let script_filename = script.display().to_string();
        let script_name = script
            .strip_prefix(&site.path)
            .map(|p| format!("/{}", p.display()))
            .unwrap_or_else(|_| "/index.php".to_string());
        let mut params: Vec<(String, String)> = vec![
            ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
            (
                "SERVER_SOFTWARE".into(),
                format!("ferrod/{}", ferro_core::VERSION),
            ),
            ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
            ("REQUEST_METHOD".into(), body.method.clone()),
            ("REQUEST_URI".into(), body.request_uri.clone()),
            ("QUERY_STRING".into(), body.query.clone()),
            ("DOCUMENT_ROOT".into(), root),
            ("SCRIPT_FILENAME".into(), script_filename),
            ("SCRIPT_NAME".into(), script_name.clone()),
            ("DOCUMENT_URI".into(), script_name),
            (
                "SERVER_NAME".into(),
                body.host.split(':').next().unwrap_or_default().into(),
            ),
            ("HTTP_HOST".into(), body.host.clone()),
            ("SERVER_ADDR".into(), "127.0.0.1".into()),
            ("REMOTE_ADDR".into(), "127.0.0.1".into()),
            ("REMOTE_PORT".into(), "0".into()),
            ("SERVER_PORT".into(), config.https_port.to_string()),
            ("HTTPS".into(), "on".into()),
        ];
        for (name, value) in headers {
            let key = name.as_str().to_uppercase().replace('-', "_");
            let Ok(value) = value.to_str() else { continue };
            match key.as_str() {
                "CONTENT_TYPE" => params.push(("CONTENT_TYPE".into(), value.into())),
                "CONTENT_LENGTH" => params.push(("CONTENT_LENGTH".into(), value.into())),
                "HOST" => {}
                _ => params.push((format!("HTTP_{key}"), value.into())),
            }
        }
        if !headers.contains_key("content-length") {
            params.push(("CONTENT_LENGTH".into(), body.bytes.len().to_string()));
        }
        params
    }

    /// Media proxy "proxy" mode: fetch from origin and stream back,
    /// optionally caching to disk. (Cache writes land in Phase 5.)
    async fn proxy_fetch(
        &self,
        _site: &str,
        origin_url: &str,
        _path: &str,
        _cache: bool,
    ) -> Response<BoxedBody> {
        let _ = origin_url;
        text(
            StatusCode::BAD_GATEWAY,
            "ferro: media proxy mode `proxy` is not implemented yet (Phase 5) — use mode `redirect`\n",
        )
    }

    fn no_such_site(&self, host: &str) -> Response<BoxedBody> {
        let name = host.strip_suffix(".localhost").unwrap_or(host);
        let config = self.state.config.read().unwrap();
        let body = format!(
            "ferro: no site named `{name}`\n\n\
             Drop a folder named `{name}` into a parked folder ({}) and reload,\n\
             or create a site with `ferro new {name}`.\n",
            config
                .parked_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        text(StatusCode::NOT_FOUND, &body)
    }
}

struct CollectedBody {
    bytes: Vec<u8>,
    headers: hyper::HeaderMap,
    method: String,
    request_uri: String,
    query: String,
    host: String,
}

async fn collect_body(req: Request<Incoming>) -> Result<CollectedBody, Box<Response<BoxedBody>>> {
    let (parts, mut body) = req.into_parts();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(frame) => {
                if let Some(data) = frame.data_ref() {
                    if (bytes.len() + data.len()) as u64 > BODY_LIMIT {
                        return Err(Box::new(text(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "413 — request body too large\n",
                        )));
                    }
                    bytes.extend_from_slice(data);
                }
            }
            Err(_) => {
                return Err(Box::new(text(
                    StatusCode::BAD_REQUEST,
                    "400 — could not read request body\n",
                )))
            }
        }
    }
    let host = parts
        .headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_lowercase();
    let uri = parts.uri.clone();
    Ok(CollectedBody {
        bytes,
        headers: parts.headers,
        method: parts.method.to_string(),
        request_uri: uri.path().to_string(),
        query: uri.query().unwrap_or_default().to_string(),
        host,
    })
}

fn fcgi_to_response(stdout: &[u8]) -> Response<BoxedBody> {
    let (headers, body) = fastcgi::split_headers(stdout);
    let mut builder = Response::builder().status(StatusCode::OK);
    let mut has_type = false;
    for (name, value) in &headers {
        let lower = name.to_lowercase();
        match lower.as_str() {
            "status" => {
                let code: u16 = value
                    .split_whitespace()
                    .next()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(200);
                builder = builder.status(StatusCode::from_u16(code).unwrap_or(StatusCode::OK));
            }
            "content-length" | "transfer-encoding" | "connection" => {}
            "content-type" => {
                has_type = true;
                builder = builder.header("content-type", value.as_str());
            }
            _ => {
                if let Ok(v) = HeaderValue::from_str(value) {
                    builder = builder.header(name.as_str(), v);
                }
            }
        }
    }
    if !has_type {
        builder = builder.header("content-type", "text/html; charset=utf-8");
    }
    builder
        .body(full(body.to_vec()))
        .unwrap_or_else(|_| text(StatusCode::INTERNAL_SERVER_ERROR, "bad php response\n"))
}

async fn serve_file(path: &Path) -> Response<BoxedBody> {
    match tokio::fs::File::open(path).await {
        Ok(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            let stream = ReaderStream::new(file);
            let body = StreamBody::new(stream.map_ok(Frame::data)).boxed();
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", mime.as_ref())
                .header("content-length", len)
                .body(body)
                .unwrap()
        }
        Err(_) => text(StatusCode::NOT_FOUND, "404\n"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

fn host_of(req: &Request<Incoming>) -> String {
    req.headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

async fn serve_conn<S>(router: Arc<Router>, stream: S, scheme: Scheme)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let service = service_fn(move |req| {
        let router = router.clone();
        async move { Ok::<_, Infallible>(router.respond(req, scheme).await) }
    });
    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
        debug!("connection error: {err}");
    }
}

async fn bind_loopback(port: u16) -> std::io::Result<Vec<TcpListener>> {
    let mut listeners = vec![TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?];
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
        Ok(l) => listeners.push(l),
        Err(err) => warn!("cannot bind ::1:{port} ({err}); serving IPv4 loopback only"),
    }
    Ok(listeners)
}

async fn run_listeners(
    listeners: Vec<TcpListener>,
    shutdown: watch::Receiver<()>,
    on_stream: impl Fn(Arc<Router>, tokio::net::TcpStream) + Send + Sync + 'static + Clone,
    router: Arc<Router>,
) -> std::io::Result<()> {
    let mut tasks = Vec::new();
    for listener in listeners {
        let mut shutdown = shutdown.clone();
        let on_stream = on_stream.clone();
        let router = router.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, _)) => on_stream(router.clone(), stream),
                            Err(err) => {
                                warn!("accept failed: {err}");
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                    _ = shutdown.changed() => return,
                }
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
    Ok(())
}

/// Turn a URI path into a safe relative path; None on traversal attempts.
fn sanitize(uri_path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for segment in uri_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            s if s.contains('\\') || s.contains('\0') => return None,
            s => out.push(s),
        }
    }
    Some(out)
}

fn proxy_origin_if_uploads(path: &str, proxy: &ferro_core::site::MediaProxy) -> Option<String> {
    const PREFIX: &str = "/wp-content/uploads/";
    if path.starts_with(PREFIX) {
        Some(format!("{}{path}", proxy.origin.trim_end_matches('/')))
    } else {
        None
    }
}

fn full(bytes: Vec<u8>) -> BoxedBody {
    Full::new(Bytes::from(bytes))
        .map_err(|never| match never {})
        .boxed()
}

fn text(status: StatusCode, body: &str) -> Response<BoxedBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(full(body.as_bytes().to_vec()))
        .unwrap()
}

fn redirect(location: &str) -> Response<BoxedBody> {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header("location", location)
        .body(full(Vec::new()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferro_core::site::MediaProxy;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize("/../../etc/passwd").is_none());
        assert!(sanitize("/..").is_none());
        assert_eq!(
            sanitize("/css/site.css").unwrap(),
            PathBuf::from("css/site.css")
        );
        assert_eq!(sanitize("/").unwrap(), PathBuf::from(""));
    }

    #[test]
    fn uploads_paths_hit_the_media_proxy() {
        let proxy = MediaProxy {
            origin: "https://prod.example.com/".into(),
            mode: "redirect".into(),
            cache: false,
        };
        assert_eq!(
            proxy_origin_if_uploads("/wp-content/uploads/2026/01/a.jpg", &proxy),
            Some("https://prod.example.com/wp-content/uploads/2026/01/a.jpg".to_string())
        );
        assert!(proxy_origin_if_uploads("/wp-admin/", &proxy).is_none());
    }
}
