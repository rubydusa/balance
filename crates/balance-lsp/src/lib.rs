mod analysis;
mod backend;
mod diagnostics;
mod navigation;
mod symbols;

use tower_lsp::{LspService, Server};

/// Run the Balance LSP server over stdio. Blocks until the client disconnects.
pub async fn run() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(backend::Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
