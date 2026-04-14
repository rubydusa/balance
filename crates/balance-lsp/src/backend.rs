use std::path::PathBuf;
use std::sync::Arc;

use balance_lang::module::ModuleLoader;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::analysis::{analyze, DocumentState};
use crate::diagnostics::to_diagnostics;
use crate::navigation;
use crate::symbols;

pub struct Backend {
    pub client: Client,
    pub documents: Arc<DashMap<Url, DocumentState>>,
    pub loader: Arc<Mutex<Option<ModuleLoader>>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(DashMap::new()),
            loader: Arc::new(Mutex::new(None)),
        }
    }

    async fn analyze_and_publish(&self, uri: Url, text: String) {
        let path = uri.to_file_path().ok();
        let mut guard = self.loader.lock().await;
        let loader_opt = guard.as_mut();
        let doc = analyze(text, path.as_deref(), loader_opt);
        let diagnostics = to_diagnostics(&doc);
        self.documents.insert(uri.clone(), doc);
        drop(guard);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        #[allow(deprecated)]
        let root: Option<PathBuf> = params
            .root_uri
            .as_ref()
            .and_then(|u| u.to_file_path().ok())
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|fs| fs.first())
                    .and_then(|f| f.uri.to_file_path().ok())
            })
            .or_else(|| params.root_path.clone().map(PathBuf::from));

        if let Some(root) = root {
            let mut guard = self.loader.lock().await;
            *guard = Some(ModuleLoader::new(root));
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "balance-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "balance-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.analyze_and_publish(params.text_document.uri, params.text_document.text)
            .await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.pop() {
            self.analyze_and_publish(params.text_document.uri, change.text)
                .await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.analyze_and_publish(params.text_document.uri, text)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let doc = match self.documents.get(&uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let symbols = symbols::document_symbols(&doc.source, &doc.program);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = match self.documents.get(&uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        let loc = navigation::goto_definition(&doc.source, &doc.program, uri, pos);
        Ok(loc.map(GotoDefinitionResponse::Scalar))
    }

    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let doc = match self.documents.get(&uri) {
            Some(d) => d,
            None => return Ok(None),
        };
        Ok(navigation::hover(&doc.source, &doc.program, pos))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> RpcResult<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let mut out = Vec::new();
        for entry in self.documents.iter() {
            let uri = entry.key().clone();
            let doc = entry.value();
            let syms = symbols::document_symbols(&doc.source, &doc.program);
            flatten_symbols(&syms, &uri, &query, &mut out);
        }
        Ok(Some(out))
    }
}

#[allow(deprecated)]
fn flatten_symbols(
    syms: &[DocumentSymbol],
    uri: &Url,
    query: &str,
    out: &mut Vec<SymbolInformation>,
) {
    for s in syms {
        if query.is_empty() || s.name.to_lowercase().contains(query) {
            out.push(SymbolInformation {
                name: s.name.clone(),
                kind: s.kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: uri.clone(),
                    range: s.selection_range,
                },
                container_name: None,
            });
        }
        if let Some(children) = &s.children {
            flatten_symbols(children, uri, query, out);
        }
    }
}
