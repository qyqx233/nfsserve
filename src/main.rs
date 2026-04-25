mod mirrorfs;

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
        eprintln!("Usage: {} <directory> [bind-address] [options]", args[0]);
        eprintln!();
        eprintln!("Arguments:");
        eprintln!("  directory       Path to export via NFS");
        eprintln!("  bind-address    Host:port to listen on (default: 0.0.0.0:2049)");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --allow-ip <ips>   Comma-separated list of allowed client IPs");
        eprintln!("  --allow-uid <uids> Comma-separated list of allowed UIDs (requires AUTH_UNIX)");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} /data", args[0]);
        eprintln!("  {} /data 0.0.0.0:2049", args[0]);
        eprintln!("  {} /data 0.0.0.0:2049 --allow-ip 192.168.1.41,127.0.0.1", args[0]);
        eprintln!("  {} /data 0.0.0.0:2049 --allow-uid 0,1000", args[0]);
        std::process::exit(1);
    }

    let mut dir: Option<String> = None;
    let mut bind_addr = "0.0.0.0:2049".to_string();
    let mut allowed_ips: Vec<String> = Vec::new();
    let mut allowed_uids: Vec<u32> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
                    bind_addr = args[i].clone();
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

    let fs = MirrorFS::new(path);
    let mut listener = NFSTcpListener::bind(&bind_addr, fs).await.unwrap_or_else(|e| {
        eprintln!("Error: failed to bind to {}: {}", bind_addr, e);
        std::process::exit(1);
    });

    if !allowed_ips.is_empty() {
        listener.with_allowed_ips(allowed_ips.clone());
        println!("IP whitelist: {:?}", allowed_ips);
    }
    if !allowed_uids.is_empty() {
        listener.with_allowed_uids(allowed_uids.clone());
        println!("UID whitelist: {:?}", allowed_uids);
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
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    });
}
