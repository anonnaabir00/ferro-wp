use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::HOST;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Host-header router that serves parked folders as static sites.
/// Phase 2 adds FastCGI for PHP and TLS termination.
pub struct Router {
    pub parked_paths: Vec<PathBuf>,
}

impl Router {
    /// Serve HTTP on loopback (v4 + v6) until `shutdown` fires.
    pub async fn run(
        self: Arc<Self>,
        port: u16,
        shutdown: watch::Receiver<()>,
    ) -> std::io::Result<()> {
        let mut listeners = Vec::new();
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(l) => listeners.push(l),
            Err(err) => return Err(err),
        }
        match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
            Ok(l) => listeners.push(l),
            Err(err) => warn!("cannot bind ::1:{port} ({err}); serving IPv4 loopback only"),
        }
        info!("router listening on 127.0.0.1:{port} (and ::1 if available)");
        let mut tasks = Vec::new();
        for listener in listeners {
            tasks.push(tokio::spawn(accept_loop(
                listener,
                self.clone(),
                shutdown.clone(),
            )));
        }
        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    }

    /// Map `foo.localhost` to the first parked folder containing `foo`.
    pub fn site_root(&self, host: &str) -> Option<(String, PathBuf)> {
        let name = host.strip_suffix(".localhost")?;
        if name.is_empty() || name.contains('/') {
            return None;
        }
        for parked in &self.parked_paths {
            let candidate = parked.join(name);
            if candidate.is_dir() {
                return Some((name.to_string(), candidate));
            }
        }
        None
    }

    async fn respond(&self, req: Request<Incoming>) -> Response<Full<Bytes>> {
        let host = req
            .headers()
            .get(HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_lowercase();

        if host == "ferro.localhost" {
            return text(StatusCode::OK, "ferro ok\n");
        }

        let Some((name, root)) = self.site_root(&host) else {
            return self.no_such_site(&host);
        };
        let _ = name;

        let Some(relative) = sanitize(req.uri().path()) else {
            return text(StatusCode::BAD_REQUEST, "invalid path\n");
        };
        let mut file = root.join(&relative);
        if file.is_dir() {
            file = file.join("index.html");
        }
        match tokio::fs::read(&file).await {
            Ok(contents) => {
                let mime = mime_guess::from_path(&file).first_or_octet_stream();
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", mime.as_ref())
                    .body(Full::new(Bytes::from(contents)))
                    .unwrap()
            }
            Err(_) => text(
                StatusCode::NOT_FOUND,
                &format!("404 — no file at {}\n", req.uri().path()),
            ),
        }
    }

    fn no_such_site(&self, host: &str) -> Response<Full<Bytes>> {
        let name = host.strip_suffix(".localhost").unwrap_or(host);
        let body = format!(
            "ferro: no site named `{name}`\n\n\
             Drop a folder named `{name}` into a parked folder ({}) and reload,\n\
             or create a site with `ferro new {name}`.\n",
            self.parked_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        text(StatusCode::NOT_FOUND, &body)
    }
}

async fn accept_loop(
    listener: TcpListener,
    router: Arc<Router>,
    mut shutdown: watch::Receiver<()>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let router = router.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |req| {
                                let router = router.clone();
                                async move { Ok::<_, Infallible>(router.respond(req).await) }
                            });
                            if let Err(err) = http1::Builder::new()
                                .serve_connection(io, service)
                                .await
                            {
                                debug!("connection error: {err}");
                            }
                        });
                    }
                    Err(err) => {
                        warn!("accept failed: {err}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            _ = shutdown.changed() => return,
        }
    }
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

fn text(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn site_root_matches_parked_folder() {
        let tmp = std::env::temp_dir().join(format!("ferro-test-{}", std::process::id()));
        let site = tmp.join("demo");
        std::fs::create_dir_all(&site).unwrap();
        let router = Router {
            parked_paths: vec![tmp.clone()],
        };
        let (name, root) = router.site_root("demo.localhost").unwrap();
        assert_eq!(name, "demo");
        assert_eq!(root, site);
        assert!(router.site_root("missing.localhost").is_none());
        assert!(router.site_root("example.com").is_none());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
