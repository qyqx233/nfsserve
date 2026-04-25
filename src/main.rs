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
        eprintln!("Usage: {} <directory> [bind-address]", args[0]);
        eprintln!("  directory:      path to export via NFS");
        eprintln!("  bind-address:   host:port to listen on (default: 0.0.0.0:2049)");
        std::process::exit(1);
    }

    let dir = &args[1];
    let bind_addr = args.get(2).cloned().unwrap_or_else(|| "0.0.0.0:2049".to_string());

    let path = PathBuf::from(dir);
    if !path.exists() {
        eprintln!("Error: directory '{}' does not exist", path.display());
        std::process::exit(1);
    }
    if !path.is_dir() {
        eprintln!("Error: '{}' is not a directory", path.display());
        std::process::exit(1);
    }

    let fs = MirrorFS::new(path);
    let listener = NFSTcpListener::bind(&bind_addr, fs).await.unwrap_or_else(|e| {
        eprintln!("Error: failed to bind to {}: {}", bind_addr, e);
        std::process::exit(1);
    });

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
