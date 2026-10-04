mod clock;
mod config;
mod error;
mod http;
mod index;
mod ingest;
mod isotime;
mod pyjson;
mod scan;
mod store;

use std::env;
use std::path::{self, Path};
use std::process::ExitCode;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config::{Dirs, Var};
use crate::http::App;
use crate::store::Store;

const USAGE: &str = "\
pulso-server: receives Pulso health samples and appends them to NDJSON files

Usage:
  pulso-server            run the server
  pulso-server reindex    rebuild the uuid index from the NDJSON files, then exit
  pulso-server help       show this text

Environment:
  PULSO_TOKEN  bearer token the app sends; required, the server does not run without it
  PULSO_BIND   bind address                (default 127.0.0.1)
  PULSO_PORT   listen port, 0 for any free (default 8787)
  PULSO_DATA   data directory              (default ./data)
  PULSO_INDEX  uuid index directory        (default <PULSO_DATA>/.index)

The index only serves de-duplication. It can be left out when the data directory
is copied, and `pulso-server reindex` rebuilds it from the NDJSON files. Run that
once before the first start on a data directory that already holds files, and
again after changing the files by hand. Stop the server first.

Contract: docs/PROTOCOL.md in the Pulso repository.
";

struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    fn usage(message: impl ToString) -> Failure {
        Failure {
            code: 2,
            message: message.to_string(),
        }
    }

    fn fatal(message: impl ToString) -> Failure {
        Failure {
            code: 1,
            message: message.to_string(),
        }
    }
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let command = args.next();
    let result = match (command.as_deref(), args.next()) {
        (None | Some("serve"), None) => serve(),
        (Some("reindex"), None) => reindex(),
        (Some("help" | "--help" | "-h"), None) => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(Failure::usage(format!("unknown arguments\n\n{USAGE}"))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure { code, message }) => {
            eprintln!("pulso-server: {message}");
            ExitCode::from(code)
        }
    }
}

fn environment(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn serve() -> Result<(), Failure> {
    let var: Var = &environment;
    // Before anything is created: a server without a token must leave no trace.
    let token = config::token(var).map_err(Failure::usage)?;
    let listen = config::listen(var).map_err(Failure::usage)?;
    let dirs = config::dirs(var);
    let store = Store::open(&dirs).map_err(Failure::fatal)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .map_err(Failure::fatal)?;
    runtime.block_on(async {
        let listener = TcpListener::bind((listen.bind.as_str(), listen.port))
            .await
            .map_err(|error| {
                Failure::fatal(format!("bind {}:{}: {error}", listen.bind, listen.port))
            })?;
        let address = listener.local_addr().map_err(Failure::fatal)?;
        println!(
            "pulso server on {address} — data: {} — index: {} — bearer token required",
            absolute(&dirs.data),
            absolute(&dirs.index)
        );
        http::serve(listener, Arc::new(App::new(store, &token))).await;
        Ok(())
    })
}

fn reindex() -> Result<(), Failure> {
    let var: Var = &environment;
    let dirs: Dirs = config::dirs(var);
    println!(
        "pulso-server: indexing {} into {}",
        absolute(&dirs.data),
        absolute(&dirs.index)
    );
    let summary = store::reindex(&dirs).map_err(Failure::fatal)?;
    println!(
        "pulso-server: indexed {} uuids and {} deleted uuids from {} files in {:.1} s",
        summary.uuids, summary.deleted, summary.files, summary.seconds
    );
    Ok(())
}

fn absolute(path: &Path) -> String {
    path::absolute(path)
        .unwrap_or_else(|_| path.to_owned())
        .display()
        .to_string()
}
