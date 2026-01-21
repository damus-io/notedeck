//! Example: Fetch publications from a relay
//!
//! This example demonstrates fetching NKBIP-01 publications from
//! wss://thecitadel.nostr1.com
//!
//! Run with: cargo run --example fetch_publication -p notedeck_publications

use futures_util::{SinkExt, StreamExt};
use notedeck_publications::{
    EventAddress, PublicationFetcher, KIND_PUBLICATION_CONTENT, KIND_PUBLICATION_INDEX,
};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const RELAY_URL: &str = "wss://thecitadel.nostr1.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to {}...", RELAY_URL);

    let (ws_stream, _) = connect_async(RELAY_URL).await?;
    let (mut write, mut read) = ws_stream.split();

    println!("Connected! Subscribing to publication indices (kind 30040)...\n");

    // Subscribe to recent publication indices
    let sub_id = "pub-discovery";
    let req = json!([
        "REQ",
        sub_id,
        {
            "kinds": [KIND_PUBLICATION_INDEX],
            "limit": 5
        }
    ]);

    write.send(Message::Text(req.to_string())).await?;

    let mut publications_found = Vec::new();

    // Read events until EOSE
    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                let parsed: Value = serde_json::from_str(&text)?;

                if let Some(arr) = parsed.as_array() {
                    match arr.first().and_then(|v| v.as_str()) {
                        Some("EVENT") => {
                            if let Some(event) = arr.get(2) {
                                let kind = event["kind"].as_u64().unwrap_or(0);
                                let pubkey = event["pubkey"].as_str().unwrap_or("");
                                let content = event["content"].as_str().unwrap_or("");

                                // Extract title and d-tag from tags
                                let tags = event["tags"].as_array();
                                let mut title = None;
                                let mut dtag = None;
                                let mut a_tags = Vec::new();

                                if let Some(tags) = tags {
                                    for tag in tags {
                                        if let Some(tag_arr) = tag.as_array() {
                                            let tag_name = tag_arr.first().and_then(|v| v.as_str());
                                            let tag_value = tag_arr.get(1).and_then(|v| v.as_str());

                                            match tag_name {
                                                Some("title") => {
                                                    title = tag_value.map(String::from)
                                                }
                                                Some("d") => dtag = tag_value.map(String::from),
                                                Some("a") => {
                                                    if let Some(v) = tag_value {
                                                        a_tags.push(v.to_string());
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }

                                if kind == KIND_PUBLICATION_INDEX as u64 {
                                    println!("📚 Publication Index Found:");
                                    println!(
                                        "   Title: {}",
                                        title.as_deref().unwrap_or("(untitled)")
                                    );
                                    println!("   D-tag: {}", dtag.as_deref().unwrap_or("(none)"));
                                    println!("   Author: {}...", &pubkey[..16]);
                                    println!("   Sections: {} referenced", a_tags.len());

                                    if let Some(dt) = &dtag {
                                        publications_found.push((
                                            pubkey.to_string(),
                                            dt.clone(),
                                            a_tags.clone(),
                                        ));
                                    }

                                    // Show first few section references
                                    for (i, a_tag) in a_tags.iter().take(3).enumerate() {
                                        if let Ok(addr) = EventAddress::from_a_tag(a_tag) {
                                            println!("      [{}] {}", i + 1, addr.dtag);
                                        }
                                    }
                                    if a_tags.len() > 3 {
                                        println!("      ... and {} more", a_tags.len() - 3);
                                    }
                                    println!();
                                } else if kind == KIND_PUBLICATION_CONTENT as u64 {
                                    println!("📄 Content Section:");
                                    println!(
                                        "   Title: {}",
                                        title.as_deref().unwrap_or("(untitled)")
                                    );
                                    println!(
                                        "   Content preview: {}...",
                                        &content.chars().take(100).collect::<String>()
                                    );
                                    println!();
                                }
                            }
                        }
                        Some("EOSE") => {
                            println!("--- End of stored events ---\n");
                            break;
                        }
                        Some("NOTICE") => {
                            if let Some(msg) = arr.get(1) {
                                println!("Relay notice: {}", msg);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Close(_) => {
                println!("Connection closed by relay");
                break;
            }
            _ => {}
        }
    }

    // If we found publications, fetch content for the first one
    if let Some((pubkey, dtag, a_tags)) = publications_found.first() {
        if !a_tags.is_empty() {
            println!("Fetching content for first publication...\n");

            // Parse addresses and build filter
            let addresses: Vec<EventAddress> = a_tags
                .iter()
                .filter_map(|a| EventAddress::from_a_tag(a).ok())
                .collect();

            if !addresses.is_empty() {
                let addr_refs: Vec<&EventAddress> = addresses.iter().collect();
                let filters = PublicationFetcher::build_filters(&addr_refs);

                println!(
                    "Built {} filter(s) for {} section(s)",
                    filters.len(),
                    addresses.len()
                );

                // Subscribe to content
                let content_sub_id = "pub-content";

                // Build JSON filter manually for the websocket
                for (i, addr) in addresses.iter().take(3).enumerate() {
                    let content_req = json!([
                        "REQ",
                        format!("{}-{}", content_sub_id, i),
                        {
                            "kinds": [addr.kind],
                            "authors": [hex::encode(addr.pubkey)],
                            "#d": [&addr.dtag],
                            "limit": 1
                        }
                    ]);

                    write.send(Message::Text(content_req.to_string())).await?;
                }

                // Read content events
                let mut content_count = 0;
                let timeout = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Text(text)) => {
                                let parsed: Value =
                                    serde_json::from_str(&text).unwrap_or(Value::Null);
                                if let Some(arr) = parsed.as_array() {
                                    match arr.first().and_then(|v| v.as_str()) {
                                        Some("EVENT") => {
                                            if let Some(event) = arr.get(2) {
                                                let title =
                                                    event["tags"].as_array().and_then(|tags| {
                                                        tags.iter().find_map(|t| {
                                                            let arr = t.as_array()?;
                                                            if arr.first()?.as_str()? == "title" {
                                                                arr.get(1)?
                                                                    .as_str()
                                                                    .map(String::from)
                                                            } else {
                                                                None
                                                            }
                                                        })
                                                    });
                                                let content =
                                                    event["content"].as_str().unwrap_or("");

                                                println!(
                                                    "📄 Section: {}",
                                                    title.unwrap_or_else(|| "(untitled)".into())
                                                );
                                                println!("   Content ({} chars):", content.len());
                                                // Show first 300 chars
                                                let preview: String =
                                                    content.chars().take(300).collect();
                                                for line in preview.lines().take(5) {
                                                    println!("   | {}", line);
                                                }
                                                println!();
                                                content_count += 1;
                                            }
                                        }
                                        Some("EOSE") => {
                                            if content_count >= 3 {
                                                return;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                });

                let _ = timeout.await;
                println!("Fetched {} content section(s)", content_count);
            }
        }
    }

    // Close connection
    write.send(Message::Close(None)).await?;
    println!("\nDone!");

    Ok(())
}
