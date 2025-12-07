use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use aegis_core::{compiler, loader};

#[derive(Debug)]
struct Backend {
    client: Client
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    // 1. Initialisation : On dit à VS Code ce qu'on sait faire
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // On veut être notifié quand le texte change
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                // On supporte l'autocomplétion
                completion_provider: Some(CompletionOptions::default()),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Aegis LSP initialized!")
            .await;
    }

    // 2. Quand le fichier est ouvert ou modifié : ANALYSE D'ERREURS
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.validate_document(params.text_document.uri, params.text_document.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // En mode FULL sync, content_changes[0].text contient tout le fichier
        if let Some(change) = params.content_changes.into_iter().next() {
            self.validate_document(params.text_document.uri, change.text).await;
        }
    }

    // 3. Autocomplétion
    async fn completion(&self, _: CompletionParams) -> Result<Option<CompletionResponse>> {
        let keywords = vec![
            "var", "func", "if", "else", "while", "for", "return", 
            "class", "new", "import", "try", "catch", "namespace",
            "Math", "Http", "Json", "System"
        ];

        let items: Vec<CompletionItem> = keywords
            .into_iter()
            .map(|k| CompletionItem {
                label: k.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("Aegis Keyword".to_string()),
                ..Default::default()
            })
            .collect();

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

impl Backend {
    // La logique de validation utilise TON compilateur !
    async fn validate_document(&self, uri: Url, text: String) {
        let mut diagnostics = Vec::new();

        // 1. On essaie de compiler
        match compiler::compile(&text) {
            Ok(json_ast) => {
                // 2. Si compile OK, on essaie de loader
                if let Err(e) = loader::parse_block(&json_ast) {
                    // Erreur Loader
                    diagnostics.push(self.parse_error_message(&e));
                }
            },
            Err(e) => {
                // Erreur Parser
                diagnostics.push(self.parse_error_message(&e));
            }
        }

        // On envoie les erreurs (ou liste vide si tout va bien) à l'éditeur
        self.client.publish_diagnostics(uri, diagnostics, None).await;
    }

    // Helper pour transformer tes erreurs "[Ligne X] Msg" en format LSP
    fn parse_error_message(&self, msg: &str) -> Diagnostic {
        // Format attendu: "Message d'erreur (Line 10)" ou "[Ligne 10] Message"
        // On essaie d'extraire le numéro de ligne
        let mut line_num = 0;
        
        // Regex simpliste ou parsing manuel.
        // Tes erreurs ressemblent à : "Expect '(' (Line 5)" ou "[Ligne 5] Error"
        
        if let Some(start) = msg.find("(Line ") {
            if let Some(end) = msg[start..].find(')') {
                let num_str = &msg[start + 6 .. start + end];
                if let Ok(n) = num_str.parse::<u32>() {
                    line_num = n.saturating_sub(1); // LSP commence à 0, Aegis à 1
                }
            }
        } else if let Some(start) = msg.find("[Ligne ") {
             if let Some(end) = msg[start..].find(']') {
                let num_str = &msg[start + 7 .. start + end];
                if let Ok(n) = num_str.parse::<u32>() {
                    line_num = n.saturating_sub(1);
                }
            }
        }

        Diagnostic {
            range: Range {
                start: Position { line: line_num, character: 0 },
                end: Position { line: line_num, character: 100 }, // Souligne toute la ligne
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("Aegis Compiler".to_string()),
            message: msg.to_string(),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend { client });
    Server::new(stdin, stdout, socket).serve(service).await;
}
