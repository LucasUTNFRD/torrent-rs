use bencode::Bencode;
use std::env;
use std::fs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = env::var("HOME")?;
    let path = format!("{}/.config/transmission/dht.dat", home);

    println!("Reading DHT data from: {}", path);

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to read file: {}", e);
            return Err(e.into());
        }
    };
    let decoded = Bencode::decode(&data)?;

    pretty_print(&decoded, 0);

    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn pretty_print(bencode: &Bencode, indent: usize) {
    let space = "  ".repeat(indent);
    match bencode {
        Bencode::Int(i) => println!("{}{}", space, i),
        Bencode::Bytes(b) => {
            if b.len() == 20 {
                println!("{}<ID (hex): {}>", space, to_hex(b));
            } else if let Ok(s) = std::str::from_utf8(b) {
                if !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_graphic() || c.is_ascii_whitespace())
                {
                    println!("{}\"{}\"", space, s);
                } else {
                    println!("{}<bytes (len {}): {:?}>", space, b.len(), b);
                }
            } else {
                println!("{}<bytes (len {}): {:?}>", space, b.len(), b);
            }
        }
        Bencode::List(l) => {
            println!("{}[", space);
            for item in l {
                pretty_print(item, indent + 1);
            }
            println!("{}]", space);
        }
        Bencode::Dict(d) => {
            println!("{}{{", space);
            for (k, v) in d {
                let key_str = std::str::from_utf8(k)
                    .map(|s| format!("\"{}\"", s))
                    .unwrap_or_else(|_| format!("{:?}", k));
                print!("{}{}: ", space, key_str);

                if key_str == "\"nodes\"" || key_str == "\"nodes6\"" {
                    if let Bencode::Bytes(nodes_bytes) = v {
                        println!();
                        decode_nodes(nodes_bytes, indent + 1, key_str == "\"nodes6\"");
                        continue;
                    }
                }

                match v {
                    Bencode::List(_) | Bencode::Dict(_) => {
                        println!();
                        pretty_print(v, indent + 1);
                    }
                    _ => {
                        pretty_print(v, 0);
                    }
                }
            }
            println!("{}}}", space);
        }
    }
}

fn decode_nodes(bytes: &[u8], indent: usize, is_ipv6: bool) {
    let space = "  ".repeat(indent);
    let initial_node_len = if is_ipv6 { 38 } else { 26 };

    let node_len = if bytes.len() % initial_node_len != 0 {
        let mut found_len = initial_node_len;
        for trial_len in [26, 32, 38, 42, 48] {
            if bytes.len() % trial_len == 0 {
                found_len = trial_len;
                break;
            }
        }
        found_len
    } else {
        initial_node_len
    };

    if bytes.len() % node_len != 0 {
        println!("{}<Invalid nodes length: {}>", space, bytes.len());
        return;
    }

    println!("{}  <Node length: {}>", space, node_len);
    println!("{}[", space);
    for chunk in bytes.chunks_exact(node_len) {
        let id = &chunk[0..20];
        let ip_start = 20;
        if is_ipv6 {
            if chunk.len() >= 38 {
                let ip = &chunk[ip_start..ip_start + 16];
                let port = u16::from_be_bytes([chunk[36], chunk[37]]);
                println!(
                    "{}  ID: {}, IP: {:?}, Port: {}",
                    space,
                    to_hex(id),
                    ip,
                    port
                );
            } else {
                println!("{}  Node: {}", space, to_hex(chunk));
            }
        } else {
            if chunk.len() >= 26 {
                let ip = &chunk[ip_start..ip_start + 4];
                let port = u16::from_be_bytes([chunk[24], chunk[25]]);
                println!(
                    "{}  ID: {}, IP: {}.{}.{}.{}, Port: {}",
                    space,
                    to_hex(id),
                    ip[0],
                    ip[1],
                    ip[2],
                    ip[3],
                    port
                );
            } else {
                println!("{}  Node: {}", space, to_hex(chunk));
            }
        }
        if node_len > (if is_ipv6 { 38 } else { 26 }) {
            let extra_start = if is_ipv6 { 38 } else { 26 };
            println!("{}    Extra: {}", space, to_hex(&chunk[extra_start..]));
        }
    }
    println!("{}]", space);
}
