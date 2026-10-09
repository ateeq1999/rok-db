//! `rok-db-gen`: generate a rok-db crate from `.sql` files.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rok_db_codegen::{Dependency, Options, Rename, differences, generate, write};

const USAGE: &str = "\
rok-db-gen: generate a rok-db crate (models, queries, migrations) from .sql files

USAGE:
    rok-db-gen generate [OPTIONS]   write the generated crate
    rok-db-gen check [OPTIONS]      fail when the generated crate is out of date (for CI)

OPTIONS:
    --config <file>         settings file (default: rok-db.toml, when it exists)
    --input <dir>           .sql files (default: db)
    --output <dir>          generated crate (default: db-gen)
    --crate-name <name>     package name of the generated crate (default: db-gen)
    --migration <name>      add a migration for the schema changes since the last one
    --rename <old=new>      a renamed table (`users=accounts`) or column
                            (`users.name=full_name`); repeatable
    --allow-destructive     allow steps that drop data or can fail on existing rows
    --strict                fail on warnings (queries without tenant or soft-delete filters)
    -h, --help              show this help

ENVIRONMENT:
    DATABASE_URL            PostgreSQL server for checking new or changed queries; the user
                            needs CREATEDB (a temporary database is created and dropped)

rok-db.toml:
    [gen]
    input = \"db\"
    output = \"db-gen\"
    crate_name = \"db-gen\"
    rok_db_version = \"0.4\"       # or: rok_db_path = \"crates/rok-db\"
    strict = false
";

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default, rename = "gen")]
    generate: GenConfig,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GenConfig {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    crate_name: Option<String>,
    rok_db_version: Option<String>,
    rok_db_path: Option<PathBuf>,
    strict: Option<bool>,
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let mut args = args.into_iter();
    let command = match args.next().as_deref() {
        Some("generate") => Command::Generate,
        Some("check") => Command::Check,
        Some("-h" | "--help") | None => {
            print!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some(other) => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    let mut flags = Flags::default();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--config" => flags.config = Some(PathBuf::from(value("--config")?)),
            "--input" => flags.input = Some(PathBuf::from(value("--input")?)),
            "--output" => flags.output = Some(PathBuf::from(value("--output")?)),
            "--crate-name" => flags.crate_name = Some(value("--crate-name")?),
            "--migration" => flags.migration = Some(value("--migration")?),
            "--rename" => {
                let spec = value("--rename")?;
                flags.renames.push(Rename::parse(&spec).ok_or_else(|| {
                    format!("--rename `{spec}`: expected `old=new` or `table.old=new`")
                })?);
            }
            "--allow-destructive" => flags.allow_destructive = true,
            "--strict" => flags.strict = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
        }
    }
    if command == Command::Check && (flags.migration.is_some() || !flags.renames.is_empty()) {
        return Err("`check` doesn't take --migration or --rename".to_owned());
    }
    let options = options(flags)?;
    let generated = generate(&options).map_err(|e| e.to_string())?;
    for warning in &generated.warnings {
        eprintln!("warning: {warning}");
    }
    match command {
        Command::Generate => {
            let written = write(&options, &generated).map_err(|e| e.to_string())?;
            if let Some(name) = &generated.new_migration {
                println!("added migration `{name}`");
            }
            if written.is_empty() {
                println!("{} is up to date", options.output.display());
            } else {
                for path in written {
                    println!("wrote {}", options.output.join(path).display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Check => {
            let stale = differences(&options, &generated);
            if stale.is_empty() {
                println!("{} is up to date", options.output.display());
                Ok(ExitCode::SUCCESS)
            } else {
                eprintln!(
                    "error: these generated files are out of date; run `rok-db-gen generate`:"
                );
                for path in stale {
                    eprintln!("  {}", options.output.join(path).display());
                }
                Ok(ExitCode::FAILURE)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Generate,
    Check,
}

#[derive(Debug, Default)]
struct Flags {
    config: Option<PathBuf>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    crate_name: Option<String>,
    migration: Option<String>,
    renames: Vec<Rename>,
    allow_destructive: bool,
    strict: bool,
}

fn options(flags: Flags) -> Result<Options, String> {
    let config_path = flags
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("rok-db.toml"));
    let (config, base) = match std::fs::read_to_string(&config_path) {
        Ok(text) => {
            let config: ConfigFile =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", config_path.display()))?;
            let base = config_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            (config.generate, base)
        }
        Err(_) if flags.config.is_none() => (GenConfig::default(), PathBuf::new()),
        Err(e) => return Err(format!("{}: {e}", config_path.display())),
    };
    if config.rok_db_version.is_some() && config.rok_db_path.is_some() {
        return Err("set either rok_db_version or rok_db_path, not both".to_owned());
    }
    let mut options = Options::new();
    let from_config = |p: Option<PathBuf>, default: PathBuf| p.map_or(default, |p| base.join(p));
    options.input = flags
        .input
        .unwrap_or_else(|| from_config(config.input, options.input.clone()));
    options.output = flags
        .output
        .unwrap_or_else(|| from_config(config.output, options.output.clone()));
    if let Some(name) = flags.crate_name.or(config.crate_name) {
        options.crate_name = name;
    }
    if let Some(version) = config.rok_db_version {
        options.rok_db = Dependency::Version(version);
    }
    if let Some(path) = config.rok_db_path {
        options.rok_db = Dependency::Path(base.join(path));
    }
    options.strict = flags.strict || config.strict.unwrap_or(false);
    options.database_url = std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty());
    options.migration = flags.migration;
    options.renames = flags.renames;
    options.allow_destructive = flags.allow_destructive;
    Ok(options)
}
