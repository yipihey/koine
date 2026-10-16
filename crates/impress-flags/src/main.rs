//! CLI binary for impress-flags.
//!
//! Usage:
//!   impress-flags list                # List all flag colors with shorthands
//!   impress-flags parse <shorthand>   # Parse a flag shorthand, output JSON
//!   impress-flags describe <color>    # Describe a flag color
//!   impress-flags --version

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("impress-flags {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("impress-flags - flag (workflow state) management");
        println!();
        println!("USAGE:");
        println!("  impress-flags list               List all flag colors");
        println!("  impress-flags parse <shorthand>  Parse a flag shorthand to JSON");
        println!("  impress-flags describe <color>   Describe a flag color");
        println!("  impress-flags --version          Print version");
        println!("  impress-flags --help             Print this help");
        println!();
        println!("SHORTHAND GRAMMAR:");
        println!("  <color>[<style>][<length>]");
        println!("  color:  r=red  a=amber  b=blue  g=gray");
        println!("  style:  s=solid  -=dashed  .=dotted  (default: solid)");
        println!("  length: f=full  h=half  q=quarter  (default: full)");
        println!();
        println!("EXAMPLES:");
        println!("  r        red solid full");
        println!("  a-h      amber dashed half");
        println!("  b.q      blue dotted quarter");
        return;
    }

    // Default command is "list" if no args provided
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("list");

    match command {
        "list" => {
            // List all flag colors using the known FlagColor variants.
            // FlagColor::from_char covers: r=Red, a=Amber, b=Blue, g=Gray
            let colors = ['r', 'a', 'b', 'g'];
            let entries: Vec<serde_json::Value> = colors
                .iter()
                .filter_map(|&c| impress_flags::FlagColor::from_char(c))
                .map(|color| {
                    serde_json::json!({
                        "shorthand": color.shorthand().to_string(),
                        "name": color.display_name(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
            );
        }
        "parse" => {
            let shorthand = match args.get(2) {
                Some(s) => s.as_str(),
                None => {
                    eprintln!("error: missing shorthand argument");
                    eprintln!("usage: impress-flags parse <shorthand>");
                    std::process::exit(1);
                }
            };

            match impress_flags::parse_flag_command(shorthand) {
                Some(flag) => {
                    // Flag, FlagColor, FlagStyle, FlagLength all derive Serialize
                    match serde_json::to_string_pretty(&flag) {
                        Ok(json) => println!("{}", json),
                        Err(e) => {
                            eprintln!("error: serialization failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                None => {
                    eprintln!("error: invalid flag shorthand: {:?}", shorthand);
                    eprintln!("color must be one of: r (red), a (amber), b (blue), g (gray)");
                    std::process::exit(1);
                }
            }
        }
        "describe" => {
            let color_arg = match args.get(2) {
                Some(s) => s.as_str(),
                None => {
                    eprintln!("error: missing color argument");
                    eprintln!("usage: impress-flags describe <color>");
                    std::process::exit(1);
                }
            };

            // Accept full names or shorthands
            let color = match color_arg.to_lowercase().as_str() {
                "red" | "r" => Some(impress_flags::FlagColor::Red),
                "amber" | "a" | "yellow" => Some(impress_flags::FlagColor::Amber),
                "blue" | "b" => Some(impress_flags::FlagColor::Blue),
                "gray" | "g" | "grey" => Some(impress_flags::FlagColor::Gray),
                _ => None,
            };

            match color {
                Some(c) => {
                    let result = serde_json::json!({
                        "name": c.display_name(),
                        "shorthand": c.shorthand().to_string(),
                        "description": format!(
                            "{} flag — workflow triage marker. Use shorthand '{}' in flag commands.",
                            c.display_name(),
                            c.shorthand()
                        ),
                        "example_commands": [
                            c.shorthand().to_string(),
                            format!("{}-h", c.shorthand()),
                            format!("{}.q", c.shorthand()),
                        ],
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    );
                }
                None => {
                    eprintln!("error: unknown flag color: {:?}", color_arg);
                    eprintln!("valid colors: red, amber, blue, gray");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("error: unknown command: {}", command);
            eprintln!("usage: impress-flags <list|parse|describe>");
            std::process::exit(1);
        }
    }
}
