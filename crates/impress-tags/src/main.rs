//! CLI binary for impress-tags.
//!
//! Usage:
//!   impress-tags validate <tag-path>    # Validate and normalize a tag path
//!   impress-tags hierarchy <tag-path>   # Show hierarchy segments for a tag
//!   impress-tags info <tag-path>        # Show full tag information as JSON
//!   impress-tags --version

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("impress-tags {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("impress-tags - hierarchical tag namespace management");
        println!();
        println!("USAGE:");
        println!("  impress-tags validate <tag-path>   Validate and normalize a tag path");
        println!("  impress-tags hierarchy <tag-path>  Show path segments as JSON array");
        println!("  impress-tags info <tag-path>       Show full tag info as JSON object");
        println!("  impress-tags --version             Print version");
        println!("  impress-tags --help                Print this help");
        println!();
        println!("TAG PATH FORMAT:");
        println!("  Hierarchical paths like: methods/sims/hydro/AMR");
        println!("  Separators: / or \\ (normalized to /)");
        println!("  Whitespace around segments is stripped");
        return;
    }

    if args.len() < 3 {
        eprintln!("usage: impress-tags <validate|hierarchy|info> <tag-path>");
        eprintln!("       impress-tags --version");
        std::process::exit(1);
    }

    let command = &args[1];
    let tag_path = &args[2];

    match command.as_str() {
        "validate" => {
            // Use parse_tag_path to normalize and validate.
            // Returns None if the path is empty/invalid after normalization.
            match impress_tags::parse_tag_path(tag_path) {
                Some(normalized) => {
                    let depth = impress_tags::tag_depth(&normalized);
                    let leaf = impress_tags::tag_leaf(&normalized);
                    let parent = impress_tags::tag_parent(&normalized);
                    let result = serde_json::json!({
                        "valid": true,
                        "normalized": normalized,
                        "depth": depth,
                        "leaf": leaf,
                        "parent": parent,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    );
                }
                None => {
                    let result = serde_json::json!({
                        "valid": false,
                        "input": tag_path,
                        "error": "empty or invalid tag path",
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    );
                    std::process::exit(1);
                }
            }
        }
        "hierarchy" => {
            // Show the path segments as a JSON array (the full ancestry)
            match impress_tags::parse_tag_path(tag_path) {
                Some(normalized) => {
                    // Build the full ancestor path chain:
                    //   methods/sims/hydro → ["methods", "methods/sims", "methods/sims/hydro"]
                    let segments: Vec<&str> = normalized.split('/').collect();
                    let ancestry: Vec<String> = (1..=segments.len())
                        .map(|n| segments[..n].join("/"))
                        .collect();
                    let json = serde_json::to_string_pretty(&ancestry)
                        .unwrap_or_else(|_| "[]".to_string());
                    println!("{}", json);
                }
                None => {
                    eprintln!("error: empty or invalid tag path: {:?}", tag_path);
                    std::process::exit(1);
                }
            }
        }
        "info" => {
            // Show comprehensive tag info as JSON
            match impress_tags::parse_tag_path(tag_path) {
                Some(normalized) => {
                    let depth = impress_tags::tag_depth(&normalized);
                    let leaf = impress_tags::tag_leaf(&normalized);
                    let parent = impress_tags::tag_parent(&normalized);
                    let segments: Vec<&str> = normalized.split('/').collect();

                    let result = serde_json::json!({
                        "path": normalized,
                        "leaf": leaf,
                        "depth": depth,
                        "parent": parent,
                        "segments": segments,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    );
                }
                None => {
                    eprintln!("error: empty or invalid tag path: {:?}", tag_path);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("error: unknown command: {}", command);
            eprintln!("usage: impress-tags <validate|hierarchy|info> <tag-path>");
            std::process::exit(1);
        }
    }
}
