use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

use balance_lang::ast::Program;
use balance_lang::eval::Evaluator;
use balance_lang::lexer::span::{Span, Spanned};
use balance_lang::lexer::tokenize;
use balance_lang::module::ModuleLoader;
use balance_lang::package::PackageManifest;
use balance_lang::parser::error::offset_to_line_col;
use balance_lang::runtime::registry::Profile;
use balance_lang::runtime::value::Value;

#[derive(Parser)]
#[command(name = "balance", about = "Balance language interpreter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Run a Balance program
    Run {
        /// Path to the .bl file
        file: String,

        /// Entry point name (required when multiple entries exist)
        #[arg(long)]
        entry: Option<String>,

        /// Resolution profile name
        #[arg(long)]
        profile: Option<String>,

        /// Register a remote service: port:publish_id:endpoint
        #[arg(long = "remote-service")]
        remote_services: Vec<String>,

        /// Path to persistent event log (NDJSON)
        #[arg(long = "event-log")]
        event_log: Option<String>,

        /// Node identifier for distributed event attribution
        #[arg(long = "node-id")]
        node_id: Option<String>,

        /// Directory for persistent substrate state
        #[arg(long = "data-dir")]
        data_dir: Option<String>,

        /// Use bytecode VM for pure computation (falls back to interpreter for services)
        #[arg(long)]
        vm: bool,

        /// Hex-encoded HMAC-SHA256 signing key for transport boundary verification
        #[arg(long = "signing-key")]
        signing_key: Option<String>,
    },
    /// Type-check a Balance program (no execution)
    Check {
        /// Path to the .bl file
        file: String,
    },
    /// Run a Balance program and print full interaction/event trace
    Trace {
        /// Path to the .bl file
        file: String,

        /// Entry point name
        #[arg(long)]
        entry: Option<String>,

        /// Output trace as JSON
        #[arg(long)]
        json: bool,
    },
    /// Deploy a service over TCP
    Deploy {
        /// Service spec: file.bl:ServiceName
        spec: String,

        /// Address to bind the server to
        #[arg(long, default_value = "127.0.0.1:9000")]
        bind: String,

        /// Path to persistent event log (NDJSON)
        #[arg(long = "event-log")]
        event_log: Option<String>,

        /// Node identifier for distributed event attribution
        #[arg(long = "node-id")]
        node_id: Option<String>,

        /// Enable gossip protocol on this address (e.g., 127.0.0.1:7000)
        #[arg(long = "gossip")]
        gossip: Option<String>,

        /// Seed nodes for gossip cluster (repeatable)
        #[arg(long = "seed")]
        seeds: Vec<String>,

        /// Endpoint of existing replica to join (for replication)
        #[arg(long = "replica-of")]
        replica_of: Option<String>,

        /// Path to file-backed chain registry (NDJSON)
        #[arg(long = "chain-registry")]
        chain_registry: Option<String>,

        /// Hex-encoded HMAC-SHA256 signing key for transport boundary verification
        #[arg(long = "signing-key")]
        signing_key: Option<String>,
    },
    /// Compile a Balance program to bytecode IR (print to stdout)
    Compile {
        /// Path to the .bl file
        file: String,
    },
    /// Start an interactive REPL
    Repl,
}

fn read_and_parse(file: &str) -> (String, balance_lang::ast::Program) {
    let source = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{file}': {e}");
            process::exit(1);
        }
    };

    let tokens = match tokenize(&source) {
        Ok(t) => t,
        Err(spans) => {
            for span in &spans {
                let (line, col) = offset_to_line_col(&source, span.start);
                let text = &source[span.start..span.end];
                eprintln!("{file}:{line}:{col}: error: unexpected token '{text}'");
                print_source_line(&source, span.start);
            }
            process::exit(1);
        }
    };

    let (mut program, parse_errors) = balance_lang::parser::parse(&source, &tokens);

    if !parse_errors.is_empty() {
        for err in &parse_errors {
            let (line, col) = offset_to_line_col(&source, err.span.start);
            eprintln!("{file}:{line}:{col}: error: {}", err.message);
            print_source_line(&source, err.span.start);
        }
        process::exit(1);
    }

    // Expand macros after parsing, before type checking
    balance_lang::macro_expand::expand_program(&mut program);

    (source, program)
}

fn run_type_check(
    _source: &str,
    program: &balance_lang::ast::Program,
    file: &str,
) -> bool {
    run_type_check_with_root(program, file, None)
}

fn run_type_check_with_root(
    program: &balance_lang::ast::Program,
    file: &str,
    module_root: Option<std::path::PathBuf>,
) -> bool {
    let type_result = if let Some(root) = module_root {
        balance_lang::types::check::check_program_with_root(program, root)
    } else {
        balance_lang::types::check::check_program(program)
    };
    for w in &type_result.warnings {
        eprintln!("warning: {}", w.message);
    }
    if !type_result.errors.is_empty() {
        for e in &type_result.errors {
            eprintln!("{file}: type error: {}", e.message);
        }
        return false;
    }
    true
}

/// Print the source line containing the given byte offset, with a caret.
fn print_source_line(source: &str, offset: usize) {
    let (line, col) = offset_to_line_col(source, offset);
    if let Some(line_text) = source.lines().nth(line - 1) {
        let trimmed = line_text.trim_start();
        let indent = line_text.len() - trimmed.len();
        eprintln!("  {}", trimmed);
        let caret_pos = if col > indent { col - indent - 1 } else { 0 };
        eprintln!("  {}^", " ".repeat(caret_pos));
    }
}

/// If the program has imports, use the module loader to resolve them and
/// merge imported items into the program. Returns a list of (alias, symbol_names)
/// pairs for qualified name resolution of aliased imports.
fn resolve_module_imports(program: &mut Program, file: &str) -> Vec<(String, Vec<String>)> {
    if program.imports.is_empty() {
        return Vec::new();
    }

    let file_path = Path::new(file).canonicalize().unwrap_or_else(|_| Path::new(file).to_path_buf());
    let project_root = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut loader = ModuleLoader::new(project_root);
    match loader.load_file(&file_path) {
        Ok(mod_name) => {
            // Collect aliased import namespace bindings before resolving
            let mut namespace_bindings = Vec::new();
            if let Some(module) = loader.get_module(&mod_name) {
                for import in &module.program.imports {
                    let imp = &import.node;
                    if let Some(ref alias) = imp.alias {
                        let source_mod = imp.path.join(".");
                        let symbols = loader.exported_symbol_names(&source_mod);
                        if !symbols.is_empty() {
                            namespace_bindings.push((alias.clone(), symbols));
                        }
                    }
                }
            }

            match loader.resolve_imports(&mod_name) {
                Ok(imported_items) => {
                    // Prepend imported items before existing items
                    let existing = std::mem::take(&mut program.items);
                    for item in imported_items {
                        program.items.push(Spanned {
                            node: item,
                            span: Span::new(0, 0),
                        });
                    }
                    program.items.extend(existing);
                }
                Err(e) => {
                    eprintln!("import error: {e}");
                    process::exit(1);
                }
            }

            namespace_bindings
        }
        Err(e) => {
            eprintln!("module load error: {e}");
            process::exit(1);
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            file,
            entry,
            profile,
            remote_services,
            event_log,
            node_id,
            data_dir,
            vm,
            signing_key,
        } => {
            let (_source, mut program) = read_and_parse(&file);

            // Resolve module imports if any
            let ns_bindings = resolve_module_imports(&mut program, &file);

            // Run type checker (errors are fatal)
            if !run_type_check(&_source, &program, &file) {
                process::exit(1);
            }

            // If --vm flag is set, use bytecode compilation + VM execution
            if vm {
                match balance_lang::vm::compiler::compile_program(&program) {
                    Ok(compiled) => {
                        let mut machine = balance_lang::vm::machine::VirtualMachine::new();
                        match machine.execute(&compiled) {
                            Ok(Value::Unit) => {}
                            Ok(val) => println!("{val}"),
                            Err(e) => {
                                eprintln!("vm error: {e}");
                                process::exit(1);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("compilation error: {e}");
                        eprintln!("hint: --vm only supports pure computation (no service interactions)");
                        process::exit(1);
                    }
                }
                return;
            }

            let mut evaluator = Evaluator::new();
            // Bind module namespace aliases for qualified name resolution
            for (alias, symbols) in &ns_bindings {
                evaluator.bind_module_namespace(alias, symbols);
            }
            // Set module root for dynamic imports:
            // prefer balance.toml directory, fallback to file's parent
            let file_path = Path::new(&file)
                .canonicalize()
                .unwrap_or_else(|_| Path::new(&file).to_path_buf());
            let file_dir = file_path.parent().unwrap_or(Path::new("."));
            if let Some(manifest_path) = PackageManifest::find(file_dir) {
                if let Some(manifest_dir) = manifest_path.parent() {
                    evaluator.set_module_root(manifest_dir.to_path_buf());
                }
            } else {
                evaluator.set_module_root(file_dir.to_path_buf());
            }
            // Enable event persistence if --event-log is specified
            if let Some(ref log_path) = event_log {
                let nid = node_id.as_deref().unwrap_or("local");
                if let Err(e) = evaluator.set_event_persistence(Path::new(log_path), nid) {
                    eprintln!("error: event persistence: {e}");
                    process::exit(1);
                }
            }

            // Enable persistent substrate state if --data-dir is specified
            if let Some(ref dir) = data_dir {
                evaluator.set_substrate_storage_dir(Path::new(dir).to_path_buf());
            }

            // Set signing key for transport boundary verification
            if let Some(ref key_hex) = signing_key {
                match parse_hex_key(key_hex) {
                    Ok(key) => evaluator.set_signing_key(key),
                    Err(e) => {
                        eprintln!("error: invalid signing key: {e}");
                        process::exit(1);
                    }
                }
            }

            if let Some(ref p) = profile {
                evaluator.set_default_profile(p);
            }

            // Register remote services from --remote-service flags
            for spec in &remote_services {
                match parse_remote_service_spec(spec) {
                    Ok((port, publish_id, endpoint)) => {
                        evaluator.register_remote_service(&port, &publish_id, &endpoint);
                    }
                    Err(e) => {
                        eprintln!("error: invalid --remote-service spec '{spec}': {e}");
                        process::exit(1);
                    }
                }
            }

            // Load profiles from balance.toml if present
            load_profiles_from_manifest(&file, &mut evaluator);

            match evaluator.eval_program(&program, entry.as_deref()).await {
                Ok(Value::Unit) => {}
                Ok(val) => println!("{val}"),
                Err(e) => {
                    eprintln!("runtime error: {e}");
                    process::exit(1);
                }
            }
        }
        Commands::Check { file } => {
            let (_source, mut program) = read_and_parse(&file);

            // Resolve module imports (same as Run/Trace/Deploy)
            let _ns_bindings = resolve_module_imports(&mut program, &file);

            // Determine module root for dynamic import type resolution
            let file_path = Path::new(&file).canonicalize().unwrap_or_else(|_| PathBuf::from(&file));
            let file_dir = file_path.parent().unwrap_or(Path::new("."));
            let module_root = if let Some(manifest_path) = PackageManifest::find(file_dir) {
                manifest_path.parent().map(|p| p.to_path_buf())
            } else {
                Some(file_dir.to_path_buf())
            };

            let type_result = if let Some(root) = module_root {
                balance_lang::types::check::check_program_with_root(&program, root)
            } else {
                balance_lang::types::check::check_program(&program)
            };
            let mut has_errors = false;

            for w in &type_result.warnings {
                eprintln!("warning: {}", w.message);
            }
            for e in &type_result.errors {
                eprintln!("{file}: type error: {}", e.message);
                has_errors = true;
            }

            if has_errors {
                process::exit(1);
            } else {
                let warning_count = type_result.warnings.len();
                let error_count = type_result.errors.len();
                eprintln!(
                    "check: {} errors, {} warnings",
                    error_count, warning_count
                );
            }
        }
        Commands::Trace { file, entry, json } => {
            let (_source, mut program) = read_and_parse(&file);

            // Resolve module imports if any
            let ns_bindings = resolve_module_imports(&mut program, &file);

            if !run_type_check(&_source, &program, &file) {
                process::exit(1);
            }

            let mut evaluator = Evaluator::new();
            // Bind module namespace aliases for qualified name resolution
            for (alias, symbols) in &ns_bindings {
                evaluator.bind_module_namespace(alias, symbols);
            }
            // Set module root: prefer balance.toml directory, fallback to file's parent
            let file_path = Path::new(&file)
                .canonicalize()
                .unwrap_or_else(|_| Path::new(&file).to_path_buf());
            let file_dir = file_path.parent().unwrap_or(Path::new("."));
            if let Some(manifest_path) = PackageManifest::find(file_dir) {
                if let Some(manifest_dir) = manifest_path.parent() {
                    evaluator.set_module_root(manifest_dir.to_path_buf());
                }
            } else {
                evaluator.set_module_root(file_dir.to_path_buf());
            }
            match evaluator.eval_program(&program, entry.as_deref()).await {
                Ok(Value::Unit) => {}
                Ok(val) => {
                    if !json {
                        println!("{val}");
                    }
                }
                Err(e) => {
                    eprintln!("runtime error: {e}");
                    process::exit(1);
                }
            }

            if json {
                // JSON output
                let trace_data = serde_json::json!({
                    "interactions": evaluator.interaction_trace(),
                    "events": evaluator.event_trace(),
                    "resolutions": evaluator.resolution_decisions(),
                });
                println!("{}", serde_json::to_string_pretty(&trace_data).unwrap());
            } else {
                // Human-readable output
                eprintln!("\n--- Interaction Trace ---");
                for rec in evaluator.interaction_trace() {
                    eprintln!(
                        "  [{:>3}] {}.{} -> {}",
                        rec.id, rec.service_id, rec.method, rec.state
                    );
                }

                eprintln!("\n--- Event Trace ---");
                for event in evaluator.event_trace() {
                    eprintln!(
                        "  [{} t={}] {}.{} {:?}",
                        event.id, event.timestamp, event.source, event.event_type,
                        event.data.keys().collect::<Vec<_>>()
                    );
                }

                eprintln!("\n--- Resolution Log ---");
                for dec in evaluator.resolution_decisions() {
                    eprintln!(
                        "  {} [{}] -> {} (candidates: {}, profile: {})",
                        dec.port,
                        dec.publish_id,
                        dec.selected_service,
                        dec.candidates_after_filter,
                        dec.profile.as_deref().unwrap_or("none"),
                    );
                }
            }
        }
        Commands::Deploy { spec, bind, event_log, node_id, gossip, seeds, replica_of, chain_registry, signing_key } => {
            // Parse spec: "file.bl:ServiceName"
            let parts: Vec<&str> = spec.splitn(2, ':').collect();
            if parts.len() != 2 {
                eprintln!("error: deploy spec must be 'file.bl:ServiceName'");
                process::exit(1);
            }
            let (file, _service_name) = (parts[0], parts[1]);
            let (_source, mut program) = read_and_parse(file);
            let ns_bindings = resolve_module_imports(&mut program, file);

            if !run_type_check(&_source, &program, file) {
                process::exit(1);
            }

            let mut evaluator = Evaluator::new();
            // Enable event persistence if --event-log is specified
            if let Some(ref log_path) = event_log {
                let nid = node_id.as_deref().unwrap_or("local");
                if let Err(e) = evaluator.set_event_persistence(Path::new(log_path), nid) {
                    eprintln!("error: event persistence: {e}");
                    process::exit(1);
                }
            }
            // Wire file-backed chain registry if --chain-registry is specified
            if let Some(ref registry_path) = chain_registry {
                use balance_lang::runtime::chain_registry::{FileBackedChainProvider, ChainBackedRegistry};
                match FileBackedChainProvider::open(Path::new(registry_path)) {
                    Ok(provider) => {
                        let registry = ChainBackedRegistry::new(Box::new(provider));
                        evaluator.set_registry(Box::new(registry));
                        eprintln!("Chain registry loaded from {registry_path}");
                    }
                    Err(e) => {
                        eprintln!("error: chain registry: {e}");
                        process::exit(1);
                    }
                }
            }
            // Wire replica endpoints if --replica-of is specified
            if let Some(ref replica_endpoint) = replica_of {
                evaluator.set_replica_endpoints(vec![replica_endpoint.clone()]);
                eprintln!("Replica endpoint configured: {replica_endpoint}");
            }
            // Set signing key for transport boundary verification
            if let Some(ref key_hex) = signing_key {
                match parse_hex_key(key_hex) {
                    Ok(key) => {
                        evaluator.set_signing_key(key);
                        eprintln!("Transport signing key configured");
                    }
                    Err(e) => {
                        eprintln!("error: invalid signing key: {e}");
                        process::exit(1);
                    }
                }
            }
            // Bind module namespace aliases for qualified name resolution
            for (alias, symbols) in &ns_bindings {
                evaluator.bind_module_namespace(alias, symbols);
            }
            // Register all services without running entry
            match evaluator.eval_program(&program, Some("__deploy_no_entry__")).await {
                Ok(_) => {}
                Err(balance_lang::eval::RuntimeError::Error(msg)) if msg.contains("no entry") => {
                    // Expected — deploy mode doesn't need an entry point
                }
                Err(e) => {
                    eprintln!("runtime error during service registration: {e}");
                    process::exit(1);
                }
            }

            // Start gossip protocol if --gossip is specified
            let mut discovery_rx = None;
            let _gossip_handle = if let Some(ref gossip_addr) = gossip {
                use std::sync::Arc;
                use tokio::sync::Mutex;
                use balance_lang::runtime::gossip_registry::{GossipRegistry, start_gossip};

                let addr: std::net::SocketAddr = gossip_addr.parse()
                    .unwrap_or_else(|e| { eprintln!("error: invalid gossip address: {e}"); process::exit(1); });
                let seed_addrs: Vec<std::net::SocketAddr> = seeds.iter()
                    .map(|s| s.parse().unwrap_or_else(|e| { eprintln!("error: invalid seed address: {e}"); process::exit(1); }))
                    .collect();

                // Create discovery channel for relaying discovered services to evaluator
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                discovery_rx = Some(rx);

                let mut gossip_reg = GossipRegistry::with_addr(addr);
                gossip_reg.set_discovery_tx(tx);
                let gossip_reg = Arc::new(Mutex::new(gossip_reg));
                match start_gossip(Arc::clone(&gossip_reg), addr, seed_addrs).await {
                    Ok(handle) => {
                        eprintln!("Gossip protocol started on {gossip_addr}");
                        Some(handle)
                    }
                    Err(e) => {
                        eprintln!("error: gossip: {e}");
                        process::exit(1);
                    }
                }
            } else {
                None
            };

            // Parse signing key bytes for server-side verification
            let signing_key_bytes: Option<Vec<u8>> = signing_key.as_ref().map(|hex| {
                parse_hex_key(hex).unwrap_or_else(|e| {
                    eprintln!("error: invalid signing key: {e}");
                    process::exit(1);
                })
            });

            eprintln!("Deploying on {bind}...");

            let listener = tokio::net::TcpListener::bind(&bind).await
                .unwrap_or_else(|e| { eprintln!("error: bind {bind}: {e}"); process::exit(1); });
            eprintln!("Listening for connections...");

            loop {
                // Drain gossip discovery channel and register newly discovered services
                if let Some(ref mut rx) = discovery_rx {
                    while let Ok(desc) = rx.try_recv() {
                        eprintln!("Gossip discovered service: {} ({})", desc.publish_id, desc.port);
                        // If the discovered service has a transport endpoint, check if it matches
                        // a locally replicated service — if so, update replica endpoints
                        if let Some(ref endpoint) = desc.transport_endpoint {
                            if desc.replication_factor.unwrap_or(0) > 1 {
                                eprintln!("  -> Replica peer discovered at {endpoint}");
                                evaluator.update_replica_endpoints(&desc.publish_id, vec![endpoint.clone()]);
                            }
                        }
                        evaluator.register_remote_service(
                            &desc.port,
                            &desc.publish_id,
                            desc.transport_endpoint.as_deref().unwrap_or(""),
                        );
                    }
                }

                let (mut stream, peer) = listener.accept().await
                    .unwrap_or_else(|e| { eprintln!("accept error: {e}"); process::exit(1); });
                eprintln!("Connection from {peer}");

                match handle_deploy_request(&mut stream, &mut evaluator, signing_key_bytes.as_deref()).await {
                    Ok(()) => {}
                    Err(e) => eprintln!("request error: {e}"),
                }
            }
        }
        Commands::Compile { file } => {
            let (_source, program) = read_and_parse(&file);

            if !run_type_check(&_source, &program, &file) {
                process::exit(1);
            }

            match balance_lang::vm::compiler::compile_program(&program) {
                Ok(compiled) => {
                    println!("=== Compiled Program ===");
                    if let Some(ref name) = compiled.module_name {
                        println!("Module: {name}");
                    }
                    if let Some(idx) = compiled.entry_index {
                        println!("Entry function index: {idx}");
                    }
                    println!("Functions: {}", compiled.functions.len());

                    if !compiled.types.is_empty() {
                        println!("\n--- Types ---");
                        for t in &compiled.types {
                            let params = if t.type_params.is_empty() {
                                String::new()
                            } else {
                                format!("<{}>", t.type_params.join(", "))
                            };
                            println!("  {}{params}", t.name);
                            for (fname, ftype) in &t.fields {
                                println!("    {fname}: {ftype}");
                            }
                        }
                    }

                    if !compiled.services.is_empty() {
                        println!("\n--- Services ---");
                        for s in &compiled.services {
                            let pub_id = s.publish_id.as_deref().unwrap_or("(none)");
                            println!("  {} provides {} [publish: {}]", s.name, s.port, pub_id);
                        }
                    }

                    if !compiled.substrates.is_empty() {
                        println!("\n--- Substrates ---");
                        for sub in &compiled.substrates {
                            println!("  {}: {}", sub.name, sub.substrate_type);
                        }
                    }

                    if !compiled.imports.is_empty() {
                        println!("\n--- Imports ---");
                        for imp in &compiled.imports {
                            let path = imp.path.join(".");
                            if imp.names.is_empty() {
                                println!("  {path}");
                            } else {
                                println!("  {path}.{{{}}}", imp.names.join(", "));
                            }
                        }
                    }

                    if !compiled.dependencies.is_empty() {
                        println!("\n--- Dependencies ---");
                        for dep in &compiled.dependencies {
                            println!("  {dep}");
                        }
                    }

                    println!();
                    for (i, func) in compiled.functions.iter().enumerate() {
                        println!(
                            "--- Function #{i}: {} (arity={}, locals={}) ---",
                            func.name, func.arity, func.locals_count
                        );
                        for (j, op) in func.code.iter().enumerate() {
                            println!("  {j:>4}: {op:?}");
                        }
                        println!();
                    }
                }
                Err(e) => {
                    eprintln!("compilation error: {e}");
                    process::exit(1);
                }
            }
        }
        Commands::Repl => {
            eprintln!("Balance REPL v0.1.0");
            eprintln!("Type expressions or statements. Special commands: :load <path>, :trace, :events, :quit");
            eprintln!();

            let mut evaluator = Evaluator::new();
            let stdin = std::io::stdin();
            let mut line = String::new();

            loop {
                eprint!("balance> ");
                line.clear();
                match stdin.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("read error: {e}");
                        break;
                    }
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Special commands
                if let Some(path) = trimmed.strip_prefix(":load ") {
                    let path = path.trim();
                    match std::fs::read_to_string(path) {
                        Ok(source) => {
                            let tokens = match tokenize(&source) {
                                Ok(t) => t,
                                Err(spans) => {
                                    for span in &spans {
                                        let text = &source[span.start..span.end];
                                        eprintln!("error: unexpected token '{text}'");
                                    }
                                    continue;
                                }
                            };
                            let (program, parse_errors) =
                                balance_lang::parser::parse(&source, &tokens);
                            if !parse_errors.is_empty() {
                                for err in &parse_errors {
                                    eprintln!("parse error: {}", err.message);
                                }
                                continue;
                            }
                            match evaluator.eval_program(&program, None).await {
                                Ok(_) => eprintln!("loaded {path}"),
                                Err(e) => eprintln!("error loading {path}: {e}"),
                            }
                        }
                        Err(e) => eprintln!("error reading {path}: {e}"),
                    }
                    continue;
                }
                match trimmed {
                    ":quit" | ":q" => break,
                    ":trace" => {
                        for rec in evaluator.interaction_trace() {
                            eprintln!(
                                "  [{:>3}] {}.{} -> {}",
                                rec.id, rec.service_id, rec.method, rec.state
                            );
                        }
                        continue;
                    }
                    ":events" => {
                        for event in evaluator.event_trace() {
                            eprintln!(
                                "  [{} t={}] {}.{} {:?}",
                                event.id, event.timestamp, event.source, event.event_type,
                                event.data.keys().collect::<Vec<_>>()
                            );
                        }
                        continue;
                    }
                    _ => {}
                }

                // Try to parse as a program (item or statement)
                let tokens = match tokenize(trimmed) {
                    Ok(t) => t,
                    Err(spans) => {
                        for span in &spans {
                            let text = &trimmed[span.start..span.end];
                            eprintln!("error: unexpected token '{text}'");
                        }
                        continue;
                    }
                };

                let (program, parse_errors) = balance_lang::parser::parse(trimmed, &tokens);
                if !parse_errors.is_empty() {
                    for err in &parse_errors {
                        eprintln!("error: {}", err.message);
                    }
                    continue;
                }

                match evaluator.eval_program(&program, None).await {
                    Ok(Value::Unit) => {}
                    Ok(val) => println!("{val}"),
                    Err(e) => eprintln!("error: {e}"),
                }
            }
        }
    }
}

/// Parse a `--remote-service port:publish_id:endpoint` spec.
/// Parse a hex-encoded key string into bytes.
fn parse_hex_key(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err("hex string must have even length".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("invalid hex: {e}")))
        .collect()
}

fn parse_remote_service_spec(spec: &str) -> Result<(String, String, String), String> {
    let parts: Vec<&str> = spec.splitn(3, ':').collect();
    if parts.len() != 3 {
        return Err("expected format port:publish_id:endpoint".to_string());
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

/// Handle a single deploy request from a TCP client.
async fn handle_deploy_request(
    stream: &mut tokio::net::TcpStream,
    evaluator: &mut Evaluator,
    signing_key: Option<&[u8]>,
) -> Result<(), String> {
    use balance_lang::runtime::tcp_transport::{
        read_frame, write_frame, TransportRequest, TransportResponse, verify_request_signature,
    };

    let request_bytes = read_frame(stream).await?;
    let request: TransportRequest = serde_json::from_slice(&request_bytes)
        .map_err(|e| format!("deserialize: {e}"))?;

    let request_id = request.request_id;

    // Verify request signature if signing key is configured
    if let Some(key) = signing_key {
        match &request.signature {
            Some(sig) => {
                if let Err(e) = verify_request_signature(key, &request.service_id, &request.method, &request.args, sig) {
                    let response = TransportResponse {
                        ok: false,
                        value: None,
                        error: Some(e),
                        events: Vec::new(),
                        lamport_time: evaluator.event_bus_lamport_time(),
                        request_id,
                    };
                    let response_json = serde_json::to_vec(&response).map_err(|e| format!("serialize: {e}"))?;
                    write_frame(stream, &response_json).await?;
                    return Ok(());
                }
            }
            None => {
                let response = TransportResponse {
                    ok: false,
                    value: None,
                    error: Some("request signature required but not provided".to_string()),
                    events: Vec::new(),
                    lamport_time: evaluator.event_bus_lamport_time(),
                    request_id,
                };
                let response_json = serde_json::to_vec(&response).map_err(|e| format!("serialize: {e}"))?;
                write_frame(stream, &response_json).await?;
                return Ok(());
            }
        }
    }

    // Snapshot event count before dispatch to capture new events
    let pre_event_count = evaluator.event_trace().len();

    let result = evaluator
        .dispatch_request(&request.service_id, &request.method, request.args)
        .await;

    // Collect new events emitted during dispatch
    let new_events = evaluator.event_trace()[pre_event_count..].to_vec();

    let lamport = evaluator.event_bus_lamport_time();
    let response = match result {
        Ok(val) => TransportResponse {
            ok: true,
            value: Some(val),
            error: None,
            events: new_events,
            lamport_time: lamport,
            request_id,
        },
        Err(e) => TransportResponse {
            ok: false,
            value: None,
            error: Some(e.to_string()),
            events: Vec::new(),
            lamport_time: lamport,
            request_id,
        },
    };

    let response_json =
        serde_json::to_vec(&response).map_err(|e| format!("serialize: {e}"))?;
    write_frame(stream, &response_json).await
}

/// Load profile configurations from balance.toml if present.
fn load_profiles_from_manifest(file: &str, evaluator: &mut Evaluator) {
    let file_path = Path::new(file)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(file).to_path_buf());
    let dir = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."));

    if let Some(manifest_path) = PackageManifest::find(dir) {
        if let Ok(content) = fs::read_to_string(&manifest_path) {
            if let Ok(table) = content.parse::<toml::Table>() {
                if let Some(profiles) = table.get("profiles").and_then(|p| p.as_table()) {
                    for (name, profile_config) in profiles {
                        if let Some(config) = profile_config.as_table() {
                            let mut profile = Profile::new(name);
                            for (k, v) in config {
                                if let Some(s) = v.as_str() {
                                    profile = profile.with_preference(k, s);
                                }
                            }
                            if let Err(e) = evaluator.register_profile(profile) {
                                eprintln!("Warning: {}", e);
                            }
                        }
                    }
                }
            }
        }
    }
}
