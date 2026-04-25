mod mirrorfs;
mod webdav;

use std::path::PathBuf;

use mirrorfs::MirrorFS;
use nfsserve::tcp::{NFSTcp, NFSTcpListener};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <directory> [nfs-bind-address] [options]", args[0]);
        eprintln!();
        eprintln!("Arguments:");
        eprintln!("  directory          Path to export via NFS/WebDAV");
        eprintln!("  nfs-bind-address   Host:port for NFS (default: 0.0.0.0:2049)");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --dav-port <port>  Enable WebDAV on the given port");
        eprintln!("  --allow-ip <ips>   Comma-separated list of allowed client IPs (NFS only)");
        eprintln!("  --allow-uid <uids> Comma-separated list of allowed UIDs (NFS only)");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} /data                          # NFS only", args[0]);
        eprintln!("  {} /data --dav-port 8080          # WebDAV only", args[0]);
        eprintln!("  {} /data 0.0.0.0:2049 --dav-port 8080  # Both NFS + WebDAV", args[0]);
        std::process::exit(1);
    }

    let mut dir: Option<String> = None;
    let mut nfs_bind = "0.0.0.0:2049".to_string();
    let mut dav_port: Option<u16> = None;
    let mut allowed_ips: Vec<String> = Vec::new();
    let mut allowed_uids: Vec<u32> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--dav-port" => {
                i += 1;
                if i < args.len() {
                    dav_port = args[i].parse().ok();
                }
            }
            "--allow-ip" => {
                i += 1;
                if i < args.len() {
                    allowed_ips.extend(args[i].split(',').map(|s| s.to_string()));
                }
            }
            "--allow-uid" => {
                i += 1;
                if i < args.len() {
                    allowed_uids.extend(args[i].split(',').filter_map(|s| s.parse::<u32>().ok()));
                }
            }
            _ => {
                if dir.is_none() {
                    dir = Some(args[i].clone());
                } else {
                    nfs_bind = args[i].clone();
                }
            }
        }
        i += 1;
    }

    let dir = dir.unwrap_or_else(|| ".".to_string());
    let path = PathBuf::from(&dir);
    if !path.exists() {
        eprintln!("Error: directory '{}' does not exist", path.display());
        std::process::exit(1);
    }
    if !path.is_dir() {
        eprintln!("Error: '{}' is not a directory", path.display());
        std::process::exit(1);
    }

    let fs = MirrorFS::new(path.clone());

    // Start WebDAV if requested
    let dav_handle = if let Some(port) = dav_port {
        let dav_fs = webdav::MirrorDavFS::new(fs.clone());
        let dav_handler = dav_fs.build_handler();
        let addr = format!("0.0.0.0:{}", port);
        Some(tokio::spawn(async move {
            run_dav_server(&addr, dav_handler).await;
        }))
    } else {
        None
    };

    // Start NFS if no --dav-port-only mode, or if nfs_bind is explicitly set
    // Actually, always start NFS unless user only wants WebDAV?
    // For now: start NFS always, unless --dav-port is set but no nfs_bind is given.
    // Wait, the logic is simpler: if user provides [nfs-bind-address], start NFS.
    // But default is to start NFS on 0.0.0.0:2049.
    // Let's start NFS always, it's the primary purpose of this binary.
    let nfs_handle = tokio::spawn(async move {
        let mut listener = NFSTcpListener::bind(&nfs_bind, fs).await.unwrap_or_else(|e| {
            eprintln!("Error: failed to bind NFS to {}: {}", nfs_bind, e);
            std::process::exit(1);
        });

        if !allowed_ips.is_empty() {
            listener.with_allowed_ips(allowed_ips.clone());
            println!("NFS IP whitelist: {:?}", allowed_ips);
        }
        if !allowed_uids.is_empty() {
            listener.with_allowed_uids(allowed_uids.clone());
            println!("NFS UID whitelist: {:?}", allowed_uids);
        }

        let port = listener.get_listen_port();
        let ip = listener.get_listen_ip();
        println!("NFS server listening on {}:{}", ip, port);
        println!("Exporting: {}", dir);
        println!();
        println!("Mount with:");
        println!("  Linux:  mount -t nfs -o nolock,vers=3,tcp,port={},mountport={} {}:/ <mnt>", port, port, ip);
        println!(
            "  macOS:  mount_nfs -o nolocks,vers=3,tcp,port={},mountport={} {}:/ <mnt>",
            port, port, ip
        );

        listener.handle_forever().await.unwrap_or_else(|e| {
            eprintln!("NFS server error: {}", e);
            std::process::exit(1);
        });
    });

    if let Some(h) = dav_handle {
        let (nfs_res, dav_res) = tokio::join!(nfs_handle, h);
        if let Err(e) = nfs_res {
            eprintln!("NFS task error: {:?}", e);
        }
        if let Err(e) = dav_res {
            eprintln!("WebDAV task error: {:?}", e);
        }
    } else {
        if let Err(e) = nfs_handle.await {
            eprintln!("NFS task error: {:?}", e);
        }
    }
}

async fn run_dav_server(addr: &str, handler: dav_server::DavHandler) {
    use axum::extract::{Request, State};
    use axum::response::IntoResponse;
    use axum::routing::any;
    use axum::Router;
    use tokio::net::TcpListener;
    use tower_http::trace::TraceLayer;

    async fn dav_handler(
        State(handler): State<dav_server::DavHandler>,
        req: Request,
    ) -> impl IntoResponse {
        handler.handle(req).await
    }

    let app = Router::new()
        .fallback(any(dav_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(handler);

    let listener = TcpListener::bind(addr).await.unwrap_or_else(|e| {
        eprintln!("Error: failed to bind WebDAV to {}: {}", addr, e);
        std::process::exit(1);
    });
    let local_addr = listener.local_addr().unwrap();
    println!("WebDAV server listening on http://{}", local_addr);
    println!();
    println!("WebDAV mount:");
    println!("  Linux:  mount -t davfs http://{}/ <mnt>", local_addr);
    println!("  macOS:  Finder → Go → Connect to Server → http://{}", local_addr);
    println!(
        "  Windows: Explorer → Map Network Drive → http://{}",
        local_addr
    );

    axum::serve(listener, app).await.unwrap_or_else(|e| {
        eprintln!("WebDAV server error: {}", e);
        std::process::exit(1);
    });
}


