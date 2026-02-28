use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "synapse",
    about = "Synapse IDL compiler — .bridge to Rust/Python/C++"
)]
enum Cli {
    /// Compile a .bridge schema file to target language bindings
    Compile {
        /// Path to the .bridge schema file
        file: PathBuf,

        /// Target language(s): rust, python, cpp
        #[arg(short, long, required = true, num_args = 1..)]
        lang: Vec<String>,

        /// Output directory (default: current directory)
        #[arg(long, default_value = ".")]
        out_dir: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli {
        Cli::Compile {
            file,
            lang,
            out_dir,
        } => {
            if let Err(e) = run_compile(&file, &lang, &out_dir) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn run_compile(file: &PathBuf, langs: &[String], output: &PathBuf) -> Result<(), String> {
    // Read source file
    let source = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read '{}': {e}", file.display()))?;

    // Validate languages up front
    for lang in langs {
        match lang.as_str() {
            "rust" | "python" | "cpp" => {}
            other => {
                return Err(format!(
                    "unknown language '{other}' (expected: rust, python, cpp)"
                ))
            }
        }
    }

    // Derive output stem from input filename
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("cannot determine stem of '{}'", file.display()))?;

    // Create output directory if needed
    std::fs::create_dir_all(output)
        .map_err(|e| format!("cannot create output dir '{}': {e}", output.display()))?;

    // Generate for each language
    for lang in langs {
        let (code, filename) = match lang.as_str() {
            "rust" => (synapse_idl::generate_rust(&source)?, format!("{stem}.rs")),
            "python" => (synapse_idl::generate_python(&source)?, format!("{stem}.py")),
            "cpp" => (synapse_idl::generate_cpp(&source)?, format!("{stem}.hpp")),
            _ => unreachable!(),
        };

        let out_path = output.join(&filename);
        std::fs::write(&out_path, &code)
            .map_err(|e| format!("cannot write '{}': {e}", out_path.display()))?;

        println!("  wrote {}", out_path.display());
    }

    Ok(())
}
