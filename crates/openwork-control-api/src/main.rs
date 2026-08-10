use openwork_control_api::{Config, serve};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

#[tokio::main]
async fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--healthcheck")
    {
        std::process::exit(i32::from(healthcheck().is_err()));
    }
    if let Err(error) = run().await {
        eprintln!("control API failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    serve(config).await
}

fn healthcheck() -> Result<(), Box<dyn std::error::Error>> {
    let configured: SocketAddr = std::env::var("OPENWORK_CONTROL_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    let address = SocketAddr::from(([127, 0, 0, 1], configured.port()));
    let mut connection = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    connection.set_read_timeout(Some(Duration::from_secs(2)))?;
    connection.set_write_timeout(Some(Duration::from_secs(2)))?;
    connection
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    connection.take(1024).read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 200") {
        return Err("Control API health endpoint is not ready".into());
    }
    Ok(())
}
