use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// Update the signature to accept the 'name' attribute
pub async fn start_proxy(name: &str, listen_addr: &str, target_addr: &str, log_payloads: bool) -> io::Result<()> {
    let listener = TcpListener::bind(listen_addr).await?;
    println!(
        "\x1b[35m[{}]\x1b[0m Pipeline active on {} -> forwarding to {}",
        name, listen_addr, target_addr
    );

    loop {
        let (client_stream, client_addr) = listener.accept().await?;
        let target_string = target_addr.to_string();

        tokio::spawn(async move {
            println!("\x1b[36m[+ Flow Connected]\x1b[0m Connection tracked from {}", client_addr);

            if let Err(e) = handle_session(client_stream, &target_string, log_payloads).await {
                eprintln!("\x1b[31m[! Flow Error]\x1b[0m Pipeline ruptured: {}", e);
            }

            println!("\x1b[33m[- Flow Disconnected]\x1b[0m Session closed for {}", client_addr);
        });
    }
}

async fn handle_session(mut client_stream: TcpStream, target_addr: &str, log_payloads: bool) -> io::Result<()> {
    let mut target_stream = TcpStream::connect(target_addr).await?;

    // Break streams down into readable/writable raw splits
    let (mut client_reader, mut client_writer) = client_stream.split();
    let (mut target_reader, mut target_writer) = target_stream.split();

    // Outbound Interception Loop (Client -> Target)
    let client_to_target = async {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = client_reader.read(&mut buffer).await?;
            if bytes_read == 0 { break; } // EOF reached

            if log_payloads { log_payload("OUTBOUND", &buffer[..bytes_read]); }
            target_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Inbound Interception Loop (Target -> Client)
    let target_to_client = async {
        let mut buffer = [0u8; 4096];
        loop {
            let bytes_read = target_reader.read(&mut buffer).await?;
            if bytes_read == 0 { break; } // EOF reached

            if log_payloads { log_payload("INBOUND", &buffer[..bytes_read]); }
            client_writer.write_all(&buffer[..bytes_read]).await?;
        }
        io::Result::Ok(())
    };

    // Keep pipelines executing simultaneously until closure
    tokio::try_join!(client_to_target, target_to_client)?;
    Ok(())
}

/// Formats and renders raw intercepted traffic payloads to the screen
fn log_payload(direction: &str, data: &[u8]) {
    let color = if direction == "OUTBOUND" { "\x1b[32m" } else { "\x1b[34m" };
    let reset = "\x1b[0m";
    
    println!("\n{}[{} Payload - {} bytes]{}", color, direction, data.len(), reset);
    
    // Generate clean hex-dump visual overview
    for chunk in data.chunks(16) {
        let hex_string: Vec<String> = chunk.iter().map(|b| format!("{:02X}", b)).collect();
        let ascii_string: String = chunk.iter().map(|&b| {
            if b >= 32 && b <= 126 { b as char } else { '.' }
        }).collect();
        
        println!("  {:48} | {}", hex_string.join(" "), ascii_string);
    }
    println!();
}
